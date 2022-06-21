use common::discord::interaction::*;
use common::discord::types::{ChannelId, Embed, Event};
use common::display::MaybeDisplay;
use common::{EventHandler, Guild};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::mem;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandsConfig {
	enabled: bool,
	#[serde(default)]
	whitelist: Option<bool>,
	#[serde(default)]
	channels: HashSet<ChannelId>,
	commands: HashMap<String, Command>,
	cdn_url: String,
	cooldown: u32,
}

impl CommandsConfig {
	#[inline]
	fn is_whitelist(&self) -> bool {
		self.whitelist.unwrap_or(true)
	}

	#[inline]
	fn allowed(&self, channel_id: ChannelId) -> bool {
		self.channels.contains(&channel_id) == self.is_whitelist()
	}
}

impl Default for CommandsConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			whitelist: None,
			channels: HashSet::new(),
			commands: HashMap::new(),
			cdn_url: String::new(),
			cooldown: 0,
		}
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "content", rename_all = "lowercase")]
pub enum CommandType {
	Text(String),
	Image(String),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Command {
	#[serde(default)]
	use_global_list: Option<bool>,
	#[serde(default)]
	whitelist: Option<bool>,
	#[serde(default)]
	channels: HashSet<ChannelId>,
	#[serde(flatten)]
	command_type: CommandType,
	description: String,
}

impl Command {
	#[inline]
	fn use_global_list(&self) -> bool {
		self.use_global_list.unwrap_or(self.channels.is_empty())
	}

	#[inline]
	fn is_whitelist(&self) -> bool {
		self.whitelist.unwrap_or(true)
	}

	#[inline]
	fn allowed(&self, channel_id: ChannelId) -> bool {
		self.channels.contains(&channel_id) == self.is_whitelist()
	}
}

#[derive(Debug)]
pub struct Commands {
	config: CommandsConfig,
	last: HashMap<String, Instant>,
}

impl Commands {
	pub fn new() -> Self {
		Self {
			config: Default::default(),
			last: HashMap::new(),
		}
	}

	fn register_commands(&self, guild: &Guild) {
		let commands: Vec<_> = self
			.config
			.commands
			.iter()
			.filter(|&(n, c)| match guild.command(n) {
				Some(ac) => c.description != ac.description,
				None => true,
			})
			.map(|(name, command)| (name.clone(), command.description.clone()))
			.collect();
		if commands.is_empty() {
			return;
		}
		let client = guild.client();
		let guild_id = guild.id();
		let application_id = guild.application_id();

		tokio::spawn(async move {
			for (name, description) in commands {
				match client
					.create_command(application_id, guild_id, &name, &description, Vec::new())
					.await
				{
					Ok(_) => debug!("Registered '{}'", name),
					Err(e) => warn!("Unable to register '{}': {}", name, e),
				}
				sleep(Duration::from_secs(5)).await;
			}
		});
	}

	fn interaction(&mut self, guild: &Guild, interaction: &Interaction) -> bool {
		if !self.config.enabled {
			return true;
		}

		/*// Check if first word matches a known command
		let first = match message.content.split(" ").next() {
			Some(w) => w,
			None => return true,
		};
		if !first.starts_with("!") || first.len() <= 1 {
			return true;
		}
		let command_name = &first[1..];
		let command = match self.config.commands.get(command_name) {
			Some(c) => c,
			None => return true,
		};*/

		// Check if command is from this module
		let command_name = match &interaction.data.name {
			Some(n) => n,
			None => return true,
		};
		let command = match self.config.commands.get(command_name) {
			Some(c) => c,
			None => return true,
		};

		let channel_id = match interaction.channel_id {
			Some(id) => id,
			None => return true,
		};

		// From here on we consume the message

		// Check if the command is allowed in this channel
		let allowed = if command.use_global_list() {
			self.config.allowed(channel_id)
		} else {
			command.allowed(channel_id)
		};
		if !allowed {
			interaction
				.respond(guild)
				.content("Command not allowed in this channel")
				.ephemeral()
				.spawn();
			return false;
		}

		// Check cooldown
		if let Some(last) = self.last.get(command_name) {
			let left = (self.config.cooldown as u64).saturating_sub(last.elapsed().as_secs());
			if left > 1 {
				interaction
					.respond(guild)
					.content(format!("Command on cooldown for {} more seconds", left))
					.ephemeral()
					.spawn();
				return false;
			}
		}

		self.last.insert(command_name.to_string(), Instant::now());
		info!(
			"Triggered '{}'{}",
			command_name,
			guild.channel(channel_id).display(" in #{}")
		);

		let res = match &command.command_type {
			CommandType::Text(text) => interaction.respond(guild).content(text.clone()),
			CommandType::Image(name) => {
				let embed = Embed::new().image(format!("{}{}", self.config.cdn_url, name));
				interaction.respond(guild).embed(embed)
			}
		};
		res.spawn();

		false
	}
}

impl EventHandler for Commands {
	fn config(&mut self, guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let config = load_config!(name, "commands", config);
		let old = mem::replace(&mut self.config, config);
		if old.enabled != self.config.enabled {
			if self.config.enabled {
				info!(
					"Module enabled with {} commands",
					self.config.commands.len()
				);
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
		}
		self.register_commands(guild);

		None
	}

	fn event(&mut self, guild: &Guild, event: &Event) -> bool {
		if let Event::InteractionCreate(ic) = event {
			self.interaction(guild, &ic.interaction)
		} else {
			true
		}
	}
}
