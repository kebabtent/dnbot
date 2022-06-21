use common::discord::types::GuildId;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
	pub discord_api_token: String,
	pub guild_id: GuildId,
	#[serde(default)]
	pub http_port: Option<u16>,
	pub http_ext_host: String,
	#[serde(default)]
	pub http_ext_port: Option<u16>,
	#[serde(default)]
	pub http_ext_secure: Option<bool>,
	pub db_uri: String,
	#[serde(default)]
	pub module_config_dir: Option<PathBuf>,
}

impl Config {
	pub fn from_env() -> Result<Config, envy::Error> {
		envy::from_env()
	}

	pub fn http_port(&self) -> u16 {
		self.http_port.unwrap_or(80)
	}

	fn http_ext_secure(&self) -> bool {
		self.http_ext_secure.unwrap_or(false)
	}

	pub fn http_ext_url(&self) -> String {
		let protocol = if self.http_ext_secure() { "s" } else { "" };
		let port = if let Some(p) = self.http_ext_port.or(self.http_port) {
			format!(":{}", p)
		} else {
			String::new()
		};

		format!("http{}://{}{}", protocol, self.http_ext_host, port)
	}

	pub fn module_config_dir(&self) -> Option<&PathBuf> {
		self.module_config_dir.as_ref().filter(|&p| p.is_dir())
	}
}
