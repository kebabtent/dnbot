use common::discord::types::{ChannelId, GuildId};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
	pub discord_api_token: String,
	pub guild_id: GuildId,
	pub broadcast_channel_id: ChannelId,
	pub announce_channel_id: ChannelId,
	pub broadcast_bitrate: u32,
	#[serde(default)]
	pub log_file: bool,
}

impl Config {
	pub fn from_env() -> Result<Config, envy::Error> {
		envy::from_env()
	}
}
