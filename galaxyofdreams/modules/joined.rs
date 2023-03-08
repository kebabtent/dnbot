use chrono::Utc;
use chronoutil::{shift_months, shift_years};
use common::discord::interaction::*;
use common::discord::types::{
	AllowedMentions, ApplicationCommandOption, ApplicationCommandOptionType, ChannelId, Event,
	UserId,
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

type DateTime = chrono::DateTime<chrono::Utc>;

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
				let ts = member.joined_at.timestamp();
				let content = format!(
					"<@{user_id}> joined **{}** (<t:{ts}:d> <t:{ts}:T>)",
					member.joined_at.readable(),
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

struct ReadableDuration(DateTime, DateTime);

impl ReadableDuration {
	pub fn new(date_time: DateTime) -> Self {
		Self(date_time, Utc::now())
	}

	#[cfg(test)]
	fn set_now(mut self, now: DateTime) -> Self {
		self.1 = now;
		self
	}
}

trait MakeReadableDuration {
	fn readable(&self) -> ReadableDuration;
}

impl MakeReadableDuration for DateTime {
	fn readable(&self) -> ReadableDuration {
		ReadableDuration::new(*self)
	}
}

impl MakeReadableDuration for common::discord::types::DateTime {
	fn readable(&self) -> ReadableDuration {
		ReadableDuration::new(self.clone().into_inner())
	}
}

const PART_COUNT: usize = 5;
const PART_NAMES: [&str; PART_COUNT] = ["week", "day", "hour", "minute", "second"];
const PART_SIZES: [u64; PART_COUNT] = [604_800, 86_400, 3_600, 60, 1];

impl fmt::Display for ReadableDuration {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let mut n = 0;
		let mut dt = self.0;
		let mut now = self.1;

		if dt == now {
			return write!(f, "right now");
		}

		let past = if now > dt {
			true
		} else {
			mem::swap(&mut dt, &mut now);
			false
		};
		let mut sh;

		// O(y+m) but yolo
		let mut y = 0;
		loop {
			sh = shift_years(dt, 1);
			if sh > now {
				break;
			}
			dt = sh;
			y += 1;
		}

		if y > 0 {
			n += 1;
			write!(f, "{y} year")?;
			if y > 1 {
				write!(f, "s")?;
			}
		}

		let mut m = 0;
		loop {
			sh = shift_months(dt, 1);
			if sh > now {
				break;
			}
			dt = sh;
			m += 1;
		}

		if m > 0 {
			if n > 0 {
				write!(f, " ")?;
			}
			n += 1;
			write!(f, "{m} month")?;
			if m > 1 {
				write!(f, "s")?;
			}
		}

		let secs = now.timestamp() - dt.timestamp();
		let mut secs = secs as u64;
		for i in 0..PART_COUNT {
			let amount = secs / PART_SIZES[i];
			if amount > 0 {
				if n > 0 {
					write!(f, " ")?;
				}
				write!(f, "{} {}", amount, PART_NAMES[i])?;
				if amount != 1 {
					write!(f, "s")?;
				}
				n += 1;
			}

			// Display up to 4 components
			if n == 4 {
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

#[cfg(test)]
mod tests {
	use super::DateTime;
	use crate::modules::joined::MakeReadableDuration;
	use chrono::{NaiveDateTime, Utc};

	#[test]
	fn readable_duration() {
		let now = DateTime::from_utc(
			NaiveDateTime::from_timestamp_opt(1678278264, 0).unwrap(),
			Utc,
		);
		let ts = [1457370320, 1458431838];
		let d = [
			"7 years 19 hours 19 minutes 4 seconds ago",
			"6 years 11 months 2 weeks 2 days ago",
		];
		for (ts, d) in ts.into_iter().zip(d.into_iter()) {
			let dt = DateTime::from_utc(NaiveDateTime::from_timestamp_opt(ts, 0).unwrap(), Utc);
			assert_eq!(format!("{}", dt.readable().set_now(now)), d);
		}
	}
}
