use common::discord::types::event;
use common::discord::types::{ChannelId, Event, Message, RoleId};
use common::display::MaybeDisplay;
use common::{EventHandler, Guild};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::mem;

#[derive(Debug, Deserialize, Serialize)]
pub struct LinkOnlyConfig {
	enabled: bool,
	channels: HashSet<ChannelId>,
	#[serde(default)]
	log_channel: Option<ChannelId>,
	#[serde(default)]
	bypass_minimum_role: Option<RoleId>,
}

impl Default for LinkOnlyConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			channels: HashSet::new(),
			log_channel: None,
			bypass_minimum_role: None,
		}
	}
}

#[derive(Debug)]
pub struct LinkOnly {
	config: LinkOnlyConfig,
}

impl LinkOnly {
	pub fn new() -> Self {
		Self {
			config: Default::default(),
		}
	}

	fn message(&mut self, guild: &Guild, message: &Message) -> bool {
		// Check if link only mode is enabled for this channel
		if !self.config.channels.contains(&message.channel_id) {
			return true;
		}

		// Check if the message contains a link
		if message.content.contains("https://") || message.content.contains("http://") {
			return true;
		}

		// Bot and system messages are ignored
		if message
			.member
			.as_ref()
			.and_then(|m| m.user.as_ref())
			.map(|u| u.is_bot() || u.is_system())
			.unwrap_or(false)
		{
			return true;
		}

		// Check if the user is allowed to bypass link only mode
		if let Some(bypass_position) = self
			.config
			.bypass_minimum_role
			.and_then(|r| guild.role(r))
			.map(|r| r.position)
		{
			if let Some(m) = &message.member {
				if guild.member_role_position(m) >= bypass_position {
					info!(
						"User{} can bypass link only mode",
						message.author.as_ref().display(" '{}'")
					);
					return true;
				}
			}
		}

		// Delete the message and optionally log it
		let client = guild.client();
		let ids = (message.channel_id, message.id);
		let log_message = self.config.log_channel.map(|id| {
			let msg = format!(
				"Deleted message{} in <#{}> because it did not contain a link:\n```{}```",
				message
					.author
					.as_ref()
					.map(|a| &a.id)
					.display(" from <@{}>"),
				message.channel_id,
				message
			);
			(id, msg)
		});

		tokio::spawn(async move {
			match client.delete_message(ids).await {
				Ok(_) => info!("Message deleted"),
				Err(e) => {
					warn!("Unable to delete message: {}", e);
					return;
				}
			}

			if let Some((id, msg)) = log_message {
				if let Err(e) = client.create_message(id).content(msg).send().await {
					warn!("Unable to log deletion: {}", e);
				}
			}
		});

		false
	}
}

impl EventHandler for LinkOnly {
	fn config(&mut self, _guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let config = load_config!(name, "link_only", config);
		let old = mem::replace(&mut self.config, config);
		if old.enabled != self.config.enabled {
			if self.config.enabled {
				info!("Module enabled in {} channels", self.config.channels.len());
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
		}

		None
	}

	fn event(&mut self, guild: &Guild, event: &Event) -> bool {
		if let Event::MessageCreate(event::MessageCreate { message }) = event {
			self.message(guild, message)
		} else {
			true
		}
	}
}
