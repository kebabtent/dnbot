use anyhow::Result;
use common::discord::client::{ButtonComponent, RowComponent};
use common::discord::interaction::CanRespond;
use common::discord::types::{
	ChannelId, Color, Embed, Event, Interaction, MessageId, PartialEmoji, RoleId,
};
use common::discord::Client;
use common::{EventHandler, Guild, Storage};
use log::{info, warn};
use metrohash::MetroHash64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{query, query_as};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::mem;

const BUTTON_ID_PREFIX: &'static str = "roleassign";

const CREATE_TABLE_SQLITE: &'static str = r#"
	CREATE TABLE IF NOT EXISTS role_assign (
		id INTEGER PRIMARY KEY NOT NULL,
		hash INTEGER NOT NULL,
		channel_id INTEGER NOT NULL,
		message_id INTEGER NOT NULL
	);
"#;

#[derive(Debug, Deserialize, Serialize)]
pub struct RoleAssignConfig {
	enabled: bool,
	messages: Vec<RoleAssignMessage>,
}

impl Default for RoleAssignConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			messages: Vec::new(),
		}
	}
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct RoleAssignMessage {
	id: i32,
	channel_id: ChannelId,
	message: String,
	buttons: Vec<Vec<RoleAssignButton>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoleAssignButton {
	#[serde(default)]
	emoji: Option<PartialEmoji>,
	#[serde(default)]
	label: Option<String>,
	role_id: RoleId,
}

impl PartialEq for RoleAssignButton {
	fn eq(&self, other: &Self) -> bool {
		let emoji = match (&self.emoji, &other.emoji) {
			(None, None) => true,
			(Some(a), Some(b)) => a.id == b.id && a.name == b.name && a.animated == b.animated,
			_ => false,
		};
		emoji && self.label == other.label && self.role_id == other.role_id
	}
}

impl Eq for RoleAssignButton {}

impl Hash for RoleAssignButton {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.emoji.is_some().hash(state);
		if let Some(emoji) = &self.emoji {
			emoji.id.hash(state);
			emoji.name.hash(state);
			emoji.animated.hash(state);
		}
		self.label.hash(state);
		self.role_id.hash(state);
	}
}

pub struct RoleAssign {
	config: RoleAssignConfig,
	client: Client,
	storage: Storage,
}

impl RoleAssign {
	pub async fn new(client: Client, storage: Storage) -> Result<Self> {
		let r = Self {
			config: Default::default(),
			client,
			storage,
		};
		r.init_storage().await?;
		Ok(r)
	}

	async fn init_storage(&self) -> Result<()> {
		query(CREATE_TABLE_SQLITE).execute(&*self.storage).await?;
		Ok(())
	}

	fn update_messages(&self, guild: &Guild) {
		for msg in &self.config.messages {
			self.update_message(guild, msg);
		}
	}

	fn update_message(&self, guild: &Guild, msg: &RoleAssignMessage) {
		let id = msg.id;
		let channel_id = msg.channel_id;

		// During startup, this function will be called for every message.
		// It is highly likely that in between server restarts the config has not changed,
		// so we compare the computed hash with the stored hash to prevent unnecessary API calls
		let mut hasher = MetroHash64::new();
		msg.hash(&mut hasher);
		let hash = hasher.finish() as i64;

		let client = self.client.clone();
		let storage = self.storage.clone();

		let (embed, rows) = render(guild, id, msg, None);
		let fut = async move {
			let storage = &*storage;

			let mut ids = query_as::<_, (i64, ChannelId, MessageId)>(
				"SELECT hash, channel_id, message_id FROM role_assign WHERE id = ?",
			)
			.bind(id)
			.fetch_optional(storage)
			.await?;

			if let Some((h, c, m)) = ids {
				if hash == h {
					return Ok(());
				}

				if c != channel_id {
					// This message was moved to a different channel
					info!("Deleting message {}", id);
					client.delete_message((c, m)).await?;
					ids = None;
				}
			}

			let message_id = if let Some((_, c, m)) = ids {
				info!("Updating message {}", id);
				client
					.edit_message(c, m)
					.content("")
					.embed(embed)
					.component_rows(rows)
					.send()
					.await?;
				m
			} else {
				info!("Creating message {}", id);
				client
					.create_message(channel_id)
					.embed(embed)
					.component_rows(rows)
					.send()
					.await?
					.id
			};

			query("DELETE FROM role_assign WHERE id = ?")
				.bind(id)
				.execute(storage)
				.await?;

			query("INSERT INTO role_assign (id, hash, channel_id, message_id) VALUES (?, ?, ?, ?)")
				.bind(id)
				.bind(hash)
				.bind(channel_id)
				.bind(message_id)
				.execute(storage)
				.await?;

			Result::<_>::Ok(())
		};

		tokio::spawn(async move {
			if let Err(e) = fut.await {
				warn!("Update message: {}", e);
			}
		});
	}

	fn interaction(&self, guild: &Guild, interaction: &Interaction) -> Option<bool> {
		if !interaction.interaction_type.is_component_interaction() {
			return Some(true);
		}

		let mut parts = match interaction.data.custom_id.as_deref() {
			Some(id) => id.split("_"),
			None => return Some(true),
		};

		if parts.next() != Some(BUTTON_ID_PREFIX) {
			return Some(true);
		}

		let msg_id = parts.next()?.parse::<i32>().ok()?;
		let message = self.config.messages.iter().find(|&m| m.id == msg_id)?;

		let idx = parts.next()?.parse::<usize>().ok()?;
		let button = message.buttons.get(idx / 5)?.get(idx % 5)?;
		let member = interaction.member.as_ref()?;
		let user = member.user.as_ref()?;
		let has_role = member.roles.contains(&button.role_id);

		let guild_id = guild.id();
		let user_id = user.id;
		let role_id = button.role_id;

		let action = if has_role { "remove" } else { "add" };
		info!("{}: {} role {}", user, action, role_id);

		let client = guild.client();
		let (embed, rows) = render(guild, msg_id, message, Some((button.role_id, !has_role)));
		let resp = interaction
			.respond(guild)
			.content("")
			.embed(embed)
			.component_rows(rows);

		let fut = async move {
			if has_role {
				client
					.remove_guild_member_role(guild_id, user_id, role_id)
					.await?;
			} else {
				client
					.add_guild_member_role(guild_id, user_id, role_id)
					.await?;
			}
			resp.send().await?;
			Result::<_>::Ok(())
		};

		tokio::spawn(async move {
			if let Err(e) = fut.await {
				warn!("Button click respond: {}", e);
			}
		});

		None
	}
}

impl EventHandler for RoleAssign {
	fn config(&mut self, guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let mut config: RoleAssignConfig = load_config!(name, "role_assign", config);

		let mut ids = HashSet::with_capacity(config.messages.len());
		config.messages.retain(|m| ids.insert(m.id));
		for m in &mut config.messages {
			m.buttons.truncate(5);
			for r in &mut m.buttons {
				r.truncate(5);
			}
		}

		let old = mem::replace(&mut self.config, config);
		if old.enabled != self.config.enabled {
			if self.config.enabled {
				info!(
					"Module enabled with {} messages",
					self.config.messages.len()
				);
				self.update_messages(guild);
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
			for msg in &self.config.messages {
				let o = old.messages.iter().find(|m| m.id == msg.id);
				if o.map(|m| m != msg).unwrap_or(true) {
					// Message has changed or didn't exist yet
					self.update_message(guild, msg);
				}
			}
		}

		None
	}

	fn event(&mut self, guild: &Guild, event: &Event) -> bool {
		if !self.config.enabled {
			return true;
		}

		if let Event::InteractionCreate(ic) = event {
			self.interaction(guild, &ic.interaction).unwrap_or(false)
		} else {
			true
		}
	}
}

fn render(
	guild: &Guild,
	id: i32,
	msg: &RoleAssignMessage,
	change: Option<(RoleId, bool)>,
) -> (Embed, Vec<RowComponent>) {
	let embed = Embed::new()
		.description(msg.message.to_string())
		.color(Color::BLUE);

	let mut rows = Vec::with_capacity(5);
	for (i, r) in msg.buttons.iter().enumerate() {
		let mut row = RowComponent::new();
		for (j, b) in r.iter().enumerate() {
			let mut count = guild
				.members()
				.filter(|m| m.roles.contains(&b.role_id))
				.count();
			if let Some((role_id, increment)) = change {
				if role_id == b.role_id {
					if increment {
						count += 1;
					} else {
						count -= 1;
					}
				}
			}

			let mut button =
				ButtonComponent::secondary(format!("{}_{}_{}", BUTTON_ID_PREFIX, id, 5 * i + j));
			if let Some(emoji) = &b.emoji {
				button = button.emoji(emoji.clone());
			}
			if let Some(label) = &b.label {
				button = button.label(format!("{} ({})", label, count));
			}
			row = row.button(button);
		}
		rows.push(row);
	}
	(embed, rows)
}
