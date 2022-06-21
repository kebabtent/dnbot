use super::MapConfig;
use anyhow::{anyhow, ensure, Result};
use common::discord::client::ButtonComponent;
use common::discord::types::{ChannelId, DateTime, GuildId, RoleId, UserId};
use common::discord::Client;
use common::{EventHandler, Guild, Storage};
use futures::channel::{mpsc, oneshot};
use futures::{SinkExt, StreamExt};
use http::{Method, StatusCode};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{query, query_scalar};
use std::convert::Infallible;
use std::mem;
use std::net::{IpAddr, SocketAddr};
use std::ops::DerefMut;
use std::sync::{Arc, Mutex};
use warp::Filter;

type SharedConfig = Arc<Mutex<AstronautsConfig>>;

const CREATE_TABLES_SQLITE: &'static str = r#"
	CREATE TABLE IF NOT EXISTS astronauts (
		user_id INTEGER PRIMARY KEY,
		is_active INTEGER NOT NULL,
		created_timestamp INTEGER NOT NULL,
		updated_timestamp INTEGER NOT NULL,
		counter INTEGER NOT NULL
	);
	
	CREATE TABLE IF NOT EXISTS astronaut_log (
		astronaut_log_id INTEGER PRIMARY KEY AUTOINCREMENT,
		user_id INTEGER NOT NULL,
		is_active INTEGER NOT NULL,
		created_timestamp INTEGER NOT NULL,
		origin TEXT NOT NULL
	);
	
	CREATE INDEX IF NOT EXISTS astronaut_log_user ON astronaut_log (user_id);
"#;

#[derive(Debug, Deserialize, Serialize)]
pub struct AstronautsConfig {
	enabled: bool,
	api_secret: String,
	role_id: RoleId,
	#[serde(default)]
	announce: Option<AstronautsAnnounceConfig>,
}

shared_config!(AstronautsConfig);

impl Default for AstronautsConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			api_secret: String::new(),
			role_id: RoleId::from(0),
			announce: None,
		}
	}
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AstronautsAnnounceConfig {
	channel_id: ChannelId,
	text: String,
	#[serde(default)]
	button: Option<AstronautsButtonConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AstronautsButtonConfig {
	text: String,
	url: String,
}

pub struct Astronauts {
	config: SharedConfig,
	storage: Storage,
	sender: mpsc::Sender<Event>,
}

impl Astronauts {
	pub async fn new(guild: &Guild, storage: Storage) -> Result<Self> {
		let (sender, recv) = mpsc::channel(8);
		let config = Arc::new(Mutex::new(Default::default()));

		let astronauts = Self {
			config: config.clone(),
			storage: storage.clone(),
			sender,
		};
		astronauts.init_storage().await?;

		let shuttle = Shuttle {
			config,
			guild_id: guild.id(),
			client: guild.client(),
			storage,
			recv,
		};
		shuttle.spawn();

		Ok(astronauts)
	}

	pub fn routes(
		&self,
	) -> impl warp::Filter<Extract = (StatusCode,), Error = warp::Rejection> + Clone {
		let config = Arc::clone(&self.config);
		let sender = self.sender.clone();
		warp::path("astronaut")
			.and(warp::addr::remote())
			.and(warp::method())
			.and(warp::header::optional::<String>("authorization"))
			.and(warp::path::param::<UserId>())
			.and_then(move |origin, method, auth, user_id| {
				let config = Arc::clone(&config);
				let sender = sender.clone();
				http_request(config, sender, origin, method, auth, user_id)
			})
	}

	async fn init_storage(&self) -> Result<()> {
		ensure!(self.storage.kind().is_sqlite(), "Unsupported db type");
		let mut tx = self.storage.begin().await?;
		{
			let mut res = query(CREATE_TABLES_SQLITE).execute_many(&mut tx).await;
			while let Some(r) = res.next().await {
				r?;
			}
		}
		tx.commit().await?;

		Ok(())
	}
}

impl EventHandler for Astronauts {
	fn config(&mut self, _guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let config = load_config!(name, "astronauts", config);
		let mut inner = self.config.lock().unwrap();
		let old = mem::replace(inner.deref_mut(), config);
		if old.enabled != inner.enabled {
			if inner.enabled {
				info!("Module enabled");
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
		}

		None
	}
}

pub struct Shuttle {
	config: SharedConfig,
	guild_id: GuildId,
	client: Client,
	storage: Storage,
	recv: mpsc::Receiver<Event>,
}

impl Shuttle {
	async fn update_role(&mut self, user_id: UserId, add: bool) -> Result<()> {
		let (role_id, announce) = self.config.map(|c| {
			let ann = c.announce.as_ref().map(|a| {
				let button = a.button.as_ref().map(|b| {
					ButtonComponent::link(b.url.clone())
						.label(b.text.clone())
						.emoji(&emoji::travel_and_places::transport_air::ROCKET)
				});

				(
					a.channel_id,
					a.text.replace("{user}", &format!("<@{}>", user_id)),
					button,
				)
			});
			(c.role_id, ann)
		})?;

		let contains = self
			.client
			.get_guild_member(self.guild_id, user_id)
			.await?
			.roles
			.contains(&role_id);
		if contains && !add {
			self.client
				.remove_guild_member_role(self.guild_id, user_id, role_id)
				.await?;
			debug!("Removed role for {}", user_id);
		} else if !contains && add {
			self.client
				.add_guild_member_role(self.guild_id, user_id, role_id)
				.await?;
			debug!("Added role for {}", user_id);
			if let Some((channel_id, text, button)) = announce {
				let mut msg = self.client.create_message(channel_id).content(text);
				if let Some(button) = button {
					msg = msg.component_row(button.into());
				}

				msg.send().await?;
			}
		}
		Ok(())
	}

	async fn update_db(&mut self, event: &Event) -> Result<()> {
		let log = if event.add { "Adding" } else { "Removing" };
		info!(
			"{} astronaut status for {} (origin: {})",
			log, event.user_id, event.origin
		);

		let mut tx = self.storage.begin().await?;

		let was_active =
			query_scalar::<_, bool>("SELECT is_active FROM astronauts WHERE user_id = ?")
				.bind(event.user_id)
				.fetch_optional(&mut tx)
				.await?;

		let now = DateTime::now();

		match was_active {
			None => {
				// Insert
				query("INSERT INTO astronauts (user_id, is_active, created_timestamp, updated_timestamp, counter) VALUES (?, ?, ?, ?, 1)")
					.bind(event.user_id)
					.bind(event.add)
					.bind(&now)
					.bind(&now)
					.execute(&mut tx)
					.await?;
			}
			Some(x) if x != event.add => {
				// Update
				query("UPDATE astronauts SET is_active = ?, updated_timestamp = ?, counter = counter + 1 WHERE user_id = ?")
					.bind(event.add)
					.bind(&now)
					.bind(event.user_id)
					.execute(&mut tx)
					.await?;
			}
			_ => {}
		}

		// Add log entry
		query(
			"INSERT INTO astronaut_log (user_id, is_active, created_timestamp, origin) VALUES (?, ?, ?, ?)",
		)
			.bind(event.user_id)
			.bind(event.add)
			.bind(&now)
			.bind(event.origin.to_string())
			.execute(&mut tx)
			.await?;

		// Commit
		tx.commit().await?;

		Ok(())
	}

	async fn run(mut self) {
		while let Some(event) = self.recv.next().await {
			let mut res = self.update_db(&event).await;
			let _ = event.send.send(res.is_ok());
			if res.is_ok() {
				res = self.update_role(event.user_id, event.add).await;
			}
			if let Err(e) = res {
				warn!("Shuttle: {}", e);
			}
		}
	}

	fn spawn(self) {
		tokio::spawn(self.run());
	}
}

pub struct Event {
	add: bool,
	user_id: UserId,
	origin: IpAddr,
	send: oneshot::Sender<bool>,
}

impl Event {
	fn new(add: bool, user_id: UserId, origin: IpAddr) -> (Self, oneshot::Receiver<bool>) {
		let (send, recv) = oneshot::channel();
		let event = Event {
			add,
			user_id,
			origin,
			send,
		};
		(event, recv)
	}
}

async fn http_request(
	config: Arc<Mutex<AstronautsConfig>>,
	mut sender: mpsc::Sender<Event>,
	origin: Option<SocketAddr>,
	method: Method,
	auth: Option<String>,
	user_id: UserId,
) -> Result<StatusCode, Infallible> {
	let origin = match origin {
		Some(a) => a.ip(),
		None => return Ok(StatusCode::BAD_REQUEST),
	};

	let add = match method {
		Method::PUT => true,
		Method::DELETE => false,
		_ => return Ok(StatusCode::METHOD_NOT_ALLOWED),
	};
	let (event, recv) = Event::new(add, user_id, origin);

	if let Some(auth) = auth.as_ref().and_then(|a| a.strip_prefix("Secret ")) {
		let config = match config.lock() {
			Ok(c) => c,
			Err(_) => return Ok(StatusCode::INTERNAL_SERVER_ERROR),
		};
		if auth != &config.api_secret {
			return Ok(StatusCode::UNAUTHORIZED);
		}
	} else {
		return Ok(StatusCode::UNAUTHORIZED);
	}

	if sender.send(event).await.is_err() {
		return Ok(StatusCode::INTERNAL_SERVER_ERROR);
	}

	match recv.await {
		Ok(x) if x => Ok(StatusCode::OK),
		_ => Ok(StatusCode::INTERNAL_SERVER_ERROR),
	}
}
