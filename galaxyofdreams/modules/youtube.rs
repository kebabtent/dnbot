// use chrono::{DateTime, Utc};
use common::discord;
use common::discord::types::ChannelId;
use common::discord::Client;
use common::{EventHandler, Guild};
use futures::channel::mpsc;
use futures::select;
use futures::{FutureExt, StreamExt};
use http::StatusCode;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fmt, mem};
use tokio::time::{sleep, sleep_until, Instant};
use warp::filters::BoxedFilter;
use warp::hyper::body::Bytes;
use warp::{Filter, Reply};

macro_rules! first {
	($a:expr, $b:expr) => {
		Box::pin(
			sleep_until(
				$a.values()
					.chain($b.values())
					.min()
					.map(|v| *v)
					.unwrap_or_else(|| Instant::now() + Duration::from_secs(24 * 60 * 60)),
			)
			.fuse(),
		)
	};
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct YoutubeChannel(String);

impl fmt::Display for YoutubeChannel {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

impl PartialEq<&str> for YoutubeChannel {
	fn eq(&self, other: &&str) -> bool {
		&self.0 == *other
	}
}

impl From<&str> for YoutubeChannel {
	fn from(id: &str) -> Self {
		YoutubeChannel(id.to_owned())
	}
}

impl From<String> for YoutubeChannel {
	fn from(id: String) -> Self {
		YoutubeChannel(id)
	}
}

impl Deref for YoutubeChannel {
	type Target = str;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YoutubeId(String);

impl fmt::Display for YoutubeId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

impl PartialEq<&str> for YoutubeId {
	fn eq(&self, other: &&str) -> bool {
		&self.0 == *other
	}
}

impl From<&str> for YoutubeId {
	fn from(id: &str) -> Self {
		YoutubeId(id.to_owned())
	}
}

impl Deref for YoutubeId {
	type Target = str;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Subscription {
	channel_id: ChannelId,
	text: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct YoutubeConfig {
	enabled: bool,
	subscriptions: HashMap<YoutubeChannel, Subscription>,
	#[serde(default)]
	log_channel: Option<ChannelId>,
}

impl Default for YoutubeConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			subscriptions: HashMap::new(),
			log_channel: None,
		}
	}
}

#[derive(Debug)]
pub struct Youtube {
	// Safe to use std Mutex since we only need to keep the lock for a very short time and
	// don't need to hold it across await points
	config: Arc<Mutex<YoutubeConfig>>,
	sender: mpsc::Sender<Event>,
}

impl Youtube {
	pub fn new(client: Client, ext_url: &str) -> Self {
		let (sender, recv) = mpsc::channel(8);
		let config = Arc::new(Mutex::new(Default::default()));

		let announcer = Announcer {
			config: Arc::clone(&config),
			ext_url: format!("{}/yt", ext_url),
			recv,
			client,
		};
		announcer.spawn();

		Self { config, sender }
	}

	pub fn routes(&self) -> BoxedFilter<(impl Reply,)> {
		let config = Arc::clone(&self.config);
		let sender = self.sender.clone();
		let sender2 = self.sender.clone();

		let post = warp::path("yt")
			.and(warp::post())
			.and(warp::body::content_length_limit(1024 * 32))
			.and(warp::body::bytes())
			.map(move |bytes| http_post(&sender, bytes).unwrap_or(StatusCode::BAD_REQUEST));
		let get = warp::path("yt")
			.and(warp::get())
			.and(warp::query::<HashMap<String, String>>())
			.map(move |query| {
				http_get(&config, &sender2, query).unwrap_or(Box::new(StatusCode::BAD_REQUEST))
			});
		post.or(get).boxed()
	}
}

impl EventHandler for Youtube {
	fn config(&mut self, _guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let config = load_config!(name, "youtube", config);
		let mut inner = self.config.lock().unwrap();
		let old = mem::replace(inner.deref_mut(), config);
		if old.enabled != inner.enabled {
			if inner.enabled {
				info!(
					"Module enabled with {} subscriptions",
					inner.subscriptions.len()
				);
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
		}

		// Signal the announcer to update subscriptions
		if inner.enabled {
			let _ = self.sender.try_send(Event::UpdateSubscriptions);
		}

		None
	}

	fn event(&mut self, _guild: &Guild, _event: &discord::types::Event) -> bool {
		true
	}
}

#[derive(Debug)]
pub enum Event {
	Publication(Publication),
	UpdateSubscriptions,
	Subscribed(YoutubeChannel, u64),
	SubscriptionDenied(YoutubeChannel, Option<String>),
}

struct Announcer {
	config: Arc<Mutex<YoutubeConfig>>,
	ext_url: String,
	recv: mpsc::Receiver<Event>,
	client: Client,
}

impl Announcer {
	fn log(&self, message: String) {
		let channel_id = match self.config.lock().unwrap().log_channel {
			Some(c) => c,
			None => return,
		};
		let client = self.client.clone();
		tokio::spawn(async move {
			if let Err(e) = client
				.create_message(channel_id)
				.content(message)
				.send()
				.await
			{
				warn!("Unable to create log message: {}", e)
			}
		});
	}

	async fn run(mut self) {
		// Store most recent announcements to avoid duplicates
		let mut history = Buffer::new(10);
		// Subscribed channels with their expiration time
		let mut subscribed = HashMap::<YoutubeChannel, Instant>::new();
		// Pending subscriptions
		let mut pending = HashMap::<YoutubeChannel, Instant>::new();
		let day = Duration::from_secs(24 * 60 * 60);
		// Timer to trigger resubscribing. Use dummy timer at start
		let mut timer = Box::pin(sleep(day).fuse());
		let mut subscribing = false;
		let subscriber = Subscriber::new(self.ext_url.clone()).unwrap(); // TODO: remove unwrap
		let timeout = Duration::from_secs(30);
		loop {
			let item = select! {
				i = self.recv.next().fuse() => match i {
					Some(i) => i,
					None => break,
				},
				_ = timer => Event::UpdateSubscriptions,
			};

			match item {
				Event::Publication(p) => {
					let channel_id;
					let title;
					let content;

					{
						// Encapsulate the guard so the `Future` stays `Send`able
						let inner = self.config.lock().unwrap();
						if !inner.enabled {
							// Module is disabled: skip
							continue;
						} else if let Some(s) = inner.subscriptions.get(&p.yt_channel) {
							if history.contains(&p.yt_id) {
								// Duplicate announcement: skip
								continue;
							}
							// Announce
							channel_id = s.channel_id;
							title = p.title;
							content = s.text.replace("%ID%", &p.yt_id);
							history.insert(p.yt_id);
						} else {
							// We're not subscribed to this channel: skip
							info!(
								"Skipping announcement for '{}': not subscribed",
								p.yt_channel
							);
							continue;
						}
					}

					let client = self.client.clone();
					tokio::spawn(async move {
						match client
							.create_message(channel_id)
							.content(content)
							.send()
							.await
						{
							Ok(_) => info!("Announced '{}'", title),
							Err(e) => warn!("Failed to announce '{}': {}", title, e),
						}
					});
				}
				Event::UpdateSubscriptions => {
					let now = Instant::now();

					// Check for any expirations
					subscribed.retain(|_, instant| *instant > now);
					pending.retain(|yt_channel, instant| {
						let retain = *instant > now;
						if !retain {
							warn!(
								"Unable to subscribe to '{}': Validation timed out",
								yt_channel
							);
							self.log(format!(
								"Unable to subscribe to `{}`:\n```Validation timed out```",
								yt_channel
							))
						}
						retain
					});

					// Check for a (re)subscription
					let to_subscribe;
					{
						// Encapsulate the guard to keep the `Future` `Send`able
						let inner = self.config.lock().unwrap();
						if !inner.enabled {
							// Module is disabled: do nothing
							continue;
						}

						to_subscribe = inner
							.subscriptions
							.keys()
							.filter(|k| !subscribed.contains_key(*k) && !pending.contains_key(*k))
							.map(|k| k.clone())
							.next();
					}

					// Attempt to subscribe to one of the channels
					subscribing = to_subscribe.is_some();
					if let Some(yt_channel) = to_subscribe {
						debug!("Subscribing to '{}'", yt_channel);
						match subscriber.subscribe(&yt_channel).await {
							Ok(_) => {
								pending.insert(yt_channel, now + timeout);
							}
							Err(e) => {
								warn!("Unable to subscribe to '{}': {}", yt_channel, e);
								self.log(format!(
									"Unable to subscribe to `{}`:\n```{}```",
									yt_channel, e
								));
							}
						}
						timer = Box::pin(sleep(timeout).fuse());
						// Only one subscription at a time
						continue;
					}

					// Update the timer to the earliest expiration
					timer = first!(pending, subscribed);
				}
				Event::Subscribed(yt_channel, lease_seconds) => {
					info!("Subscribed to '{}'", yt_channel);
					pending.remove(&yt_channel);
					// Renew subscription 10 minutes before expiration
					subscribed.insert(
						yt_channel,
						Instant::now() + Duration::from_secs(lease_seconds.saturating_sub(600)),
					);

					if !subscribing {
						timer = first!(pending, subscribed);
					}
				}
				Event::SubscriptionDenied(yt_channel, reason) => {
					pending.remove(&yt_channel);
					timer = Box::pin(sleep(timeout).fuse());
					match reason {
						Some(reason) => {
							warn!("Subscription to '{}' denied: {}", yt_channel, reason);
							self.log(format!(
								"Subscription to `{}` denied:\n```{}```",
								yt_channel, reason
							));
						}
						None => {
							warn!("Subscription to '{}' denied", yt_channel);
							self.log(format!("Subscription to `{}` denied", yt_channel));
						}
					}
				}
			}
		}
	}

	fn spawn(self) {
		tokio::spawn(self.run());
	}
}

const HUB_URL: &str = "https://pubsubhubbub.appspot.com/subscribe";
const TOPIC_URL: &str = "https://www.youtube.com/xml/feeds/videos.xml?channel_id=";

struct Subscriber {
	ext_url: String,
	client: reqwest::Client,
}

impl Subscriber {
	pub fn new(ext_url: String) -> Result<Self, reqwest::Error> {
		let client = reqwest::ClientBuilder::new()
			.timeout(Duration::from_secs(10))
			.use_rustls_tls()
			.build()?;
		Ok(Self { ext_url, client })
	}

	async fn subscribe(&self, channel: &YoutubeChannel) -> Result<(), reqwest::Error> {
		let topic = format!("{}{}", TOPIC_URL, channel);
		let form = [
			("hub.mode", "subscribe"),
			("hub.topic", &topic),
			("hub.callback", &self.ext_url),
		];
		self.client.post(HUB_URL).form(&form).send().await?;
		Ok(())
	}
}

// Fixed size FIFO buffer
// TODO: replace with https://github.com/NULLx76/ringbuffer/
struct Buffer<T> {
	capacity: usize,
	inner: VecDeque<T>,
}

impl<T> Buffer<T> {
	fn new(capacity: usize) -> Self {
		assert!(capacity > 0);
		Self {
			capacity,
			inner: VecDeque::with_capacity(capacity),
		}
	}

	fn insert(&mut self, value: T) -> Option<T> {
		let pop = if self.inner.len() == self.capacity {
			self.inner.pop_front()
		} else {
			None
		};
		self.inner.push_back(value);
		pop
	}

	fn contains(&self, x: &T) -> bool
	where
		T: PartialEq<T>,
	{
		self.inner.contains(x)
	}
}

const BASE_NS: &str = "http://www.w3.org/2005/Atom";
const YT_NS: &str = "http://www.youtube.com/xml/schemas/2015";

#[derive(Debug)]
pub enum PubError {
	InvalidXml,
	MissingChild(&'static str),
	MissingChildInner(&'static str),
	// InvalidDateTime,
}

#[derive(Debug)]
pub struct Publication {
	title: String,
	yt_id: YoutubeId,
	yt_channel: YoutubeChannel,
	// published: DateTime<Utc>,
	// updated: DateTime<Utc>,
}

impl FromStr for Publication {
	type Err = PubError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let root: minidom::Element = s.parse().map_err(|_| PubError::InvalidXml)?;
		let entry = root
			.get_child("entry", BASE_NS)
			.ok_or_else(|| PubError::MissingChild("entry"))?;

		let title = entry_text(entry, "title", BASE_NS)?.into();
		let yt_id = entry_text(entry, "videoId", YT_NS)?.into();
		let yt_channel = entry_text(entry, "channelId", YT_NS)?.into();

		/*let published = DateTime::parse_from_rfc3339(entry_text(entry, "published", BASE_NS)?)
			.map_err(|_| PubError::InvalidDateTime)?
			.with_timezone(&Utc);

		let updated = DateTime::parse_from_rfc3339(entry_text(entry, "updated", BASE_NS)?)
			.map_err(|_| PubError::InvalidDateTime)?
			.with_timezone(&Utc);*/

		Ok(Publication {
			title,
			yt_id,
			yt_channel,
			// published,
			// updated,
		})
	}
}

// Shorthand function to get inner text of an element
fn entry_text<'a>(
	entry: &'a minidom::Element,
	name: &'static str,
	namespace: &str,
) -> Result<&'a str, PubError> {
	entry
		.get_child(name, namespace)
		.ok_or_else(|| PubError::MissingChild(name))?
		.texts()
		.next()
		.ok_or_else(|| PubError::MissingChildInner(name))
}

// HTTP server
fn http_get(
	config: &Arc<Mutex<YoutubeConfig>>,
	sender: &mpsc::Sender<Event>,
	query: HashMap<String, String>,
) -> Option<Box<dyn warp::Reply>> {
	debug!("HTTP GET");
	let mode = query.get("hub.mode")?;
	let topic = query.get("hub.topic")?;

	if !topic.starts_with(TOPIC_URL) {
		return None;
	}
	let yt_channel = YoutubeChannel::from(topic.get(TOPIC_URL.len()..)?);
	if !config.lock().ok()?.subscriptions.contains_key(&yt_channel) {
		return Some(Box::new(StatusCode::NOT_FOUND));
	}

	match mode.deref() {
		"denied" => {
			let reason = query.get("hub.reason").map(|r| r.to_string());
			let _ = sender
				.clone()
				.try_send(Event::SubscriptionDenied(yt_channel, reason));
			Some(Box::new(StatusCode::OK))
		}
		"subscribe" => {
			let challenge = query.get("hub.challenge")?;
			let lease_seconds = query.get("hub.lease_seconds")?.parse().ok()?;
			let _ = sender
				.clone()
				.try_send(Event::Subscribed(yt_channel, lease_seconds));
			Some(Box::new(challenge.to_string()))
		}
		_ => None,
	}
}

fn http_post(sender: &mpsc::Sender<Event>, bytes: Bytes) -> Option<StatusCode> {
	debug!("HTTP POST");
	let raw = std::str::from_utf8(&bytes).ok()?;
	let publication = Publication::from_str(raw).ok()?;
	let _ = sender.clone().try_send(Event::Publication(publication));
	Some(StatusCode::OK)
}

#[cfg(test)]
mod tests {
	use super::*;
	use chrono::TimeZone;

	#[test]
	fn publication() {
		const DATA: &str = r#"
			<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns="http://www.w3.org/2005/Atom">
			  <link rel="hub" href="https://pubsubhubbub.appspot.com"/>
			  <link rel="self" href="https://www.youtube.com/xml/feeds/videos.xml?channel_id=CHANNEL_ID"/>
			  <title>YouTube video feed</title>
			  <updated>2015-04-01T19:05:24.552394234+00:00</updated>
			  <entry>
				<id>yt:video:VIDEO_ID</id>
				<yt:videoId>VIDEO_ID</yt:videoId>
				<yt:channelId>CHANNEL_ID</yt:channelId>
				<title>Video title</title>
				<link rel="alternate" href="http://www.youtube.com/watch?v=VIDEO_ID"/>
				<author>
				 <name>Channel title</name>
				 <uri>http://www.youtube.com/channel/CHANNEL_ID</uri>
				</author>
				<published>2015-03-06T21:40:57+00:00</published>
				<updated>2015-03-09T19:05:24.552394234+00:00</updated>
			  </entry>
			</feed>
		"#;

		let publication = Publication::from_str(DATA).unwrap();
		assert_eq!(&publication.title, "Video title");
		assert_eq!(publication.yt_channel, "CHANNEL_ID");
		assert_eq!(publication.yt_id, "VIDEO_ID");
		assert_eq!(
			publication.published,
			Utc.ymd(2015, 3, 6).and_hms(21, 40, 57)
		);
		assert_eq!(
			publication.updated,
			Utc.ymd(2015, 3, 9).and_hms_nano(19, 5, 24, 552394234)
		);
	}
}
