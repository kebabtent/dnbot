use anyhow::Result;
use async_fuse::Fuse;
use chrono::{DateTime, Utc};
use common::discord::types::{ChannelId, Embed};
use common::discord::voice::source::ffmpeg_stream;
use common::discord::voice::{Controller, Event, Listener, Updater};
use common::discord::Client;
use common::{Guild, HasUpdater, VoiceEventHandler};
// use futures::channel::mpsc;
use futures::StreamExt;
use log::{debug, info, warn};
use serde::Deserialize;
use std::pin::Pin;
use std::time::Duration;
use tokio::select;
use tokio::task::JoinHandle;
use tokio::time::sleep;

// type EventSend = mpsc::Sender<()>;
// type EventRecv = mpsc::Receiver<()>;
type Sleep = Pin<Box<tokio::time::Sleep>>;

pub struct Radio {
	updater: Updater,
	// event_send: EventSend,
}

impl Radio {
	pub fn new(
		guild: &Guild,
		broadcast: ChannelId,
		announce: ChannelId,
		bitrate: u32,
	) -> Result<Self> {
		let (updater, controller, listener) = guild.create_player();
		// let (event_send, event_recv) = mpsc::channel(16);

		let host = Host {
			// guild_id: guild.id(),
			channel_id: broadcast,
			// client: guild.client(),
			controller,
			listener,
			// event_recv,
			try_connect: Fuse::empty(),
			try_play: Fuse::empty(),
			connected: false,
			playing: false,
			bitrate,
		};
		host.spawn();

		let announcer = Announcer::new(announce, guild.client())?;
		announcer.spawn();

		Ok(Self {
			updater,
			// event_send,
		})
	}
}

impl HasUpdater for Radio {
	fn updater(&mut self) -> &mut Updater {
		&mut self.updater
	}
}

impl VoiceEventHandler for Radio {}

struct Host {
	// guild_id: GuildId,
	channel_id: ChannelId,
	// client: Client,
	controller: Controller,
	listener: Listener,
	// event_recv: EventRecv,
	try_connect: Fuse<Sleep>,
	try_play: Fuse<Sleep>,
	connected: bool,
	playing: bool,
	bitrate: u32,
}

impl Host {
	fn connect(&mut self, delayed: bool) {
		self.connected = false;
		let duration = Duration::from_secs(if delayed { 3 } else { 0 });
		self.try_connect.set(Box::pin(sleep(duration)));
	}

	fn play(&mut self, delayed: bool) {
		self.playing = false;
		let duration = Duration::from_secs(if delayed { 3 } else { 0 });
		self.try_play.set(Box::pin(sleep(duration)));
	}

	async fn run(mut self) {
		self.connect(false);
		loop {
			select! {
				ev = self.listener.next() => {
					let ev = match ev {
						Some(e) => e,
						None => {
							info!("Player shutdown");
							break;
						}
					};
					match ev {
						Event::Connected(_) => {
							info!("Connected");
							self.connected = true;
							if !self.playing {
								self.play(false);
							}
						}
						Event::ConnectError => {
							warn!("Unable to connect");
							self.connect(true);
						}
						Event::Playing => {
							info!("Playing");
							self.playing = true;
						}
						Event::Stopped(_) => {
							warn!("Stopped playing");
							self.play(true);
						}
						Event::Finished => {
							warn!("End of stream");
							self.play(true);
						}
						Event::Disconnected(_) => {
							warn!("Disconnected");
							self.connect(true);
							self.playing = false;
						}
						Event::Reconnecting(_) => {
							info!("Reconnecting");
							self.connected = false;
							self.playing = false;
						}
					}
				}
				_ = &mut self.try_connect => {
					debug!("Connect");
					self.try_connect.clear();
					if self.connected {
						continue;
					}
					self.controller.connect(self.channel_id);
				}
				_ = &mut self.try_play => {
					debug!("Play");
					self.try_play.clear();
					if self.playing {
						continue;
					}
					match ffmpeg_stream("https://streamer.radio.co/s1086ffd2f/listen", true, self.bitrate) {
						Ok(s) => {
							self.controller.play(s);
						}
						Err(e) => {
							warn!("ffmpeg: {}", e);
							self.play(true);
						}
					}
				}
			}
		}
	}

	fn spawn(self) -> JoinHandle<()> {
		tokio::spawn(self.run())
	}
}

struct Announcer {
	channel_id: ChannelId,
	client: Client,
	http: reqwest::Client,
	current: Option<String>,
	// current: Option<(String, String)>,
	// schedule: Vec<Entry>,
	// i: u8,
}

impl Announcer {
	fn new(channel_id: ChannelId, client: Client) -> Result<Self> {
		let http = reqwest::Client::builder()
			.connect_timeout(Duration::from_secs(10))
			.timeout(Duration::from_secs(30))
			.build()?;
		Ok(Self {
			channel_id,
			client,
			http,
			current: None,
			// schedule: Vec::new(),
			// i: 0,
		})
	}

	/*async fn update(&mut self) -> Result<()> {
		if self.i % 10 == 0 {
			// Periodically refresh our schedule
			self.i = 0;
			let body = self
				.http
				.get("https://public.radio.co/stations/s1086ffd2f/embed/schedule")
				.send()
				.await?
				.error_for_status()?
				.bytes()
				.await?;
			let schedule: Schedule = serde_json::from_slice(&body)?;
			self.schedule = schedule.data;
		}
		self.i += 1;

		let now = Utc::now();
		let current = self
			.schedule
			.iter()
			.filter(|e| now >= e.start && now < e.end)
			.next();

		if self.current.as_ref().map(|(a, n)| (a, n))
			!= current.map(|e| (&e.playlist.artist, &e.playlist.name))
		{
			if let Some(e) = current {
				// Announce
				let pl = &e.playlist;
				let mut embed = Embed::new()
					.title("Now playing")
					.description(format!("{} - {}", pl.artist, pl.name))
					.image(pl.artwork.replace(".100.", ".600."))
					.timestamp(e.start.clone());

				if let Ok(color) = pl.colour.parse::<Color>() {
					embed = embed.color(color);
				}

				self.client
					.create_message(self.channel_id)
					.embed(embed)
					.send()
					.await?;

				debug!("Announce {} - {}", pl.artist, pl.name);
				self.current = Some((pl.artist.clone(), pl.name.clone()));
			} else {
				self.current = None;
			}
		}
		Ok(())
	}*/

	async fn update(&mut self) -> Result<()> {
		let body = self
			.http
			.get("https://public.radio.co/api/v2/s1086ffd2f/track/current")
			.send()
			.await?
			.error_for_status()?
			.bytes()
			.await?;
		let current = serde_json::from_slice::<Current>(&body)?.data;

		if self.current.as_ref() == Some(&current.title) {
			return Ok(());
		}

		// Announce
		let embed = Embed::new()
			.title("Now playing")
			.url("https://goatshedmusic.com/player/")
			.description(current.title.clone())
			.image(current.artwork_urls.large)
			.timestamp(current.start_time);

		self.client
			.create_message(self.channel_id)
			.embed(embed)
			.send()
			.await?;

		debug!("Announce {}", current.title);
		self.current = Some(current.title);

		Ok(())
	}

	async fn run(mut self) {
		loop {
			if let Err(e) = self.update().await {
				warn!("Announcer: {}", e);
			}
			sleep(Duration::from_secs(60)).await;
		}
	}

	fn spawn(self) -> JoinHandle<()> {
		tokio::spawn(async move {
			self.run().await;
		})
	}
}

#[derive(Deserialize)]
struct Current {
	data: CurrentData,
}

#[derive(Deserialize)]
struct CurrentData {
	title: String,
	start_time: DateTime<Utc>,
	artwork_urls: CurrentArt,
}

#[derive(Deserialize)]
struct CurrentArt {
	large: String,
}

/*#[derive(Deserialize)]
struct Schedule {
	data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
	start: DateTime<Utc>,
	end: DateTime<Utc>,
	playlist: EntryPlaylist,
}

#[derive(Deserialize)]
struct EntryPlaylist {
	name: String,
	colour: String,
	artist: String,
	title: String,
	artwork: String,
}*/
