pub use self::astronauts::{Astronauts, AstronautsConfig};
// pub use self::automod::{Automod, AutomodConfig};
// pub use self::collab_playlist::{CollabPlaylist, CollabPlaylistConfig};
pub use self::commands::{Commands, CommandsConfig};
// pub use self::dj::DJ;
pub use self::filter::Filter;
pub use self::joined::{Joined, JoinedConfig};
pub use self::link_only::{LinkOnly, LinkOnlyConfig};
pub use self::role_assign::{RoleAssign, RoleAssignConfig};
pub use self::youtube::{Youtube, YoutubeConfig};
use anyhow::{anyhow, Result};
use common::{Storage, StorageKind};
use futures::channel::mpsc;
use futures::StreamExt;
use hotwatch::Hotwatch;
use log::{info, warn};
use serde_json::Value;
use sqlx::{query, AnyPool, Row};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::BufReader;
use std::ops::Deref;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;
// pub use crate::levels::{Levels, LevelsConfig, LevelsStorageProvider};

macro_rules! load_config {
	($n:expr, $t:expr, $e:expr) => {{
		if $n != $t {
			return Some($e);
		}

		match serde_json::from_value($e) {
			Ok(c) => c,
			Err(e) => {
				warn!("Unable to load {} config: {}", $t, e);
				return None;
			}
		}
	}};
}

macro_rules! shared_config {
	($t:ty) => {
		impl<F, T> MapConfig<F, T> for Arc<Mutex<$t>>
		where
			F: FnOnce(std::sync::MutexGuard<$t>) -> T,
		{
			fn map(&self, f: F) -> Result<T> {
				let guard = self.lock().map_err(|_| anyhow!("Poisoned lock"))?;
				Ok(f(guard))
			}
		}
	};
}

mod astronauts;
// mod automod;
// mod collab_playlist;
mod commands;
// mod dj;
mod filter;
mod joined;
// mod levels;
mod link_only;
mod role_assign;
pub mod youtube;

pub enum Configurator {
	Db(DbConfigurator),
	File(FileConfigurator),
}

impl Configurator {
	pub fn new(storage: &Storage, module_config_dir: &PathBuf) -> Result<Self> {
		let c = match storage.kind() {
			StorageKind::Sqlite => Configurator::File(FileConfigurator::new(module_config_dir)?),
			StorageKind::Postgres => Configurator::Db(DbConfigurator::new(storage.deref().clone())),
		};
		Ok(c)
	}

	pub async fn next(&mut self) -> Result<(String, Value)> {
		match self {
			Configurator::Db(db) => db.next().await,
			Configurator::File(file) => file.next().await,
		}
	}
}

pub struct DbConfigurator {
	pool: AnyPool,
	init: bool,
	configs: VecDeque<(String, Value)>,
	versions: HashMap<String, i32>,
}

impl DbConfigurator {
	pub fn new(pool: AnyPool) -> Self {
		Self {
			pool,
			init: true,
			configs: VecDeque::with_capacity(4),
			versions: HashMap::new(),
		}
	}

	async fn next(&mut self) -> Result<(String, Value)> {
		loop {
			if let Some(c) = self.configs.pop_front() {
				return Ok(c);
			}

			if self.init {
				self.init = false;
				info!("Init configurator");
			} else {
				sleep(Duration::from_secs(10)).await;
			}

			let mut cursor = query("SELECT * FROM module_config").fetch(&self.pool);
			while let Some(row) = cursor.next().await {
				let row = match row {
					Ok(r) => r,
					Err(_) => continue,
				};

				let name: String = row.try_get("name")?;
				let version: i32 = row.try_get("version")?;
				if self
					.versions
					.get(&name)
					.map(|v| *v == version)
					.unwrap_or(false)
				{
					continue;
				}

				let data = row.try_get("data")?;
				let data = serde_json::from_str(data)?;
				self.versions.insert(name.clone(), version);
				self.configs.push_back((name.to_owned(), data));
			}
		}
	}
}

pub struct FileConfigurator {
	#[allow(dead_code)]
	watcher: Hotwatch,
	recv: mpsc::Receiver<PathBuf>,
}

impl FileConfigurator {
	pub fn new(path: &PathBuf) -> Result<Self> {
		let (mut send, recv) = mpsc::channel(16);

		for entry in fs::read_dir(path)? {
			let path = entry?.path();
			if path.is_file() {
				if let Err(e) = send.try_send(path) {
					warn!("Missed a config file: {}", e.into_inner().display());
				}
			}
		}

		let mut watcher = Hotwatch::new()?;
		watcher.watch(path, move |ev| {
			if let hotwatch::Event::Write(path) = ev {
				let _ = send.try_send(path);
			}
		})?;

		Ok(Self { watcher, recv })
	}

	async fn next(&mut self) -> Result<(String, Value)> {
		let path = self
			.recv
			.next()
			.await
			.ok_or_else(|| anyhow!("Hotwatch closed"))?;
		let name = path
			.file_stem()
			.and_then(|s| s.to_str())
			.ok_or_else(|| anyhow!("Invalid file name"))?
			.to_owned();
		let file = fs::File::open(path)?;
		let reader = BufReader::new(file);
		let data = serde_json::from_reader(reader)?;
		Ok((name, data))
	}
}

// Shorthand for reading from the shared config
pub trait MapConfig<F, T> {
	fn map(&self, f: F) -> Result<T>;
}
