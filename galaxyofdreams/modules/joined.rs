use common::discord::interaction::*;
use common::discord::types::{
	AllowedMentions, ApplicationCommandOption, ApplicationCommandOptionType, ChannelId, DateTime,
	Event, UserId,
};
use common::display::MaybeDisplay;
use common::{EventHandler, Guild};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Instant;
use std::{fmt, mem};

const COMMAND_NAME: &'static str = "joined";
const USER_OPTION_NAME: &'static str = "user";

#[derive(Debug, Deserialize, Serialize)]
pub struct JoinedConfig {
	enabled: bool,
	#[serde(default)]
	whitelist: Option<bool>,
	#[serde(default)]
	channels: HashSet<ChannelId>,
	#[serde(default)]
	cooldown: u32,
}

impl JoinedConfig {
	#[inline]
	fn is_whitelist(&self) -> bool {
		self.whitelist.unwrap_or(true)
	}

	#[inline]
	fn allowed(&self, channel_id: ChannelId) -> bool {
		self.channels.contains(&channel_id) == self.is_whitelist()
	}
}

impl Default for JoinedConfig {
	fn default() -> Self {
		Self {
			enabled: false,
			whitelist: Some(true),
			channels: HashSet::new(),
			cooldown: 0,
		}
	}
}

#[derive(Debug)]
pub struct Joined {
	config: JoinedConfig,
	last: Option<Instant>,
}

impl Joined {
	pub fn new() -> Self {
		Self {
			config: Default::default(),
			last: None,
		}
	}

	fn register_command(&self, guild: &Guild) {
		if guild.command(COMMAND_NAME).is_some() {
			return;
		}
		let client = guild.client();
		let application_id = guild.application_id();
		let guild_id = guild.id();
		tokio::spawn(async move {
			let option = ApplicationCommandOption {
				option_type: ApplicationCommandOptionType::User,
				name: USER_OPTION_NAME.into(),
				description: "User".into(),
				required: false,
				choices: Vec::new(),
				options: Vec::new(),
			};
			match client
				.create_command(
					application_id,
					guild_id,
					COMMAND_NAME,
					"See how long ago a user joined the server",
					vec![option],
				)
				.await
			{
				Ok(_) => debug!("Registered command"),
				Err(e) => warn!("Unable to register command: {}", e),
			}
		});
	}

	fn interaction(&mut self, guild: &Guild, interaction: &Interaction) -> bool {
		if !self.config.enabled {
			return true;
		}

		if interaction.data.name.as_deref() != Some(COMMAND_NAME) {
			return true;
		}

		let channel_id = match interaction.channel_id {
			Some(c) => c,
			None => return true,
		};

		// Get user id from argument. If no argument was given, set to user that send the command
		let user_id = match interaction
			.data
			.options
			.get(0)
			.filter(|o| o.name == USER_OPTION_NAME)
			.and_then(|o| o.value.as_deref())
			.and_then(|v| UserId::from_str(v).ok())
			.or_else(|| {
				interaction
					.member
					.as_ref()
					.and_then(|m| m.user.as_ref())
					.map(|u| u.id)
			}) {
			Some(id) => id,
			None => return true,
		};

		// From here on we consume the message: return `false`

		// Check if the command is allowed in this channel
		if !self.config.allowed(channel_id) {
			interaction
				.respond(guild)
				.content("Command not allowed in this channel")
				.ephemeral()
				.spawn();
			return false;
		}

		// Check cooldown
		if let Some(last) = self.last {
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

		self.last = Some(Instant::now());
		info!("Triggered{}", guild.channel(channel_id).display(" in #{}"));

		match guild.member(user_id) {
			Some(member) => {
				let content = format!(
					"<@{}> joined **{}** ({})",
					user_id,
					ReadableDuration::new(&member.joined_at),
					member.joined_at.format("%d-%m-%Y %H:%M:%S")
				);
				interaction
					.respond(guild)
					.content(content)
					.allowed_mentions(AllowedMentions::none())
					.spawn();
			}
			None => {
				interaction
					.respond(guild)
					.content("Unable to determine user join date")
					.ephemeral()
					.spawn();
			}
		}

		false
	}
}

impl EventHandler for Joined {
	fn config(&mut self, guild: &Guild, name: &str, config: Value) -> Option<Value> {
		let config = load_config!(name, "joined", config);
		let old = mem::replace(&mut self.config, config);
		if old.enabled != self.config.enabled {
			if self.config.enabled {
				info!("Module enabled");
			} else {
				info!("Module disabled");
			}
		} else {
			info!("Config updated");
		}
		self.register_command(guild);

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

struct ReadableDuration<'a>(&'a DateTime);

impl<'a> ReadableDuration<'a> {
	pub fn new(date_time: &'a DateTime) -> Self {
		Self(date_time)
	}
}

const PART_COUNT: usize = 7;
const PART_NAMES: [&str; 7] = ["year", "month", "week", "day", "hour", "minute", "second"];
const PART_SIZES: [u64; 7] = [31_557_600, 2_630_016, 604_800, 86_400, 3_600, 60, 1];
// 1y=365.25d 1mo=30.44d

impl fmt::Display for ReadableDuration<'_> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let secs = DateTime::now().timestamp() - self.0.timestamp();
		let past = secs > 0;
		let mut secs = secs.abs() as u64;
		let mut displayed = 0;
		if secs == 0 {
			return write!(f, "right now");
		}
		for i in 0..PART_COUNT {
			let amount = secs / PART_SIZES[i];
			if amount > 0 {
				if displayed > 0 {
					write!(f, " ")?;
				}
				write!(f, "{} {}", amount, PART_NAMES[i])?;
				if amount != 1 {
					write!(f, "s")?;
				}
				displayed += 1;
			}

			// Display up to 4 components
			if displayed == 4 {
				break;
			}
			secs %= PART_SIZES[i];
		}

		if past {
			write!(f, " ago")
		} else {
			write!(f, " from now")
		}
	}
}
