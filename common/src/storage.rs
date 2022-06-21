// use crate::config::Config;
// use crate::modules::{Configurator, DbConfigurator, FileConfigurator};
use anyhow::{anyhow, Result};
use sqlx::any::{AnyConnectOptions, AnyKind, AnyPool};
use std::convert::{TryFrom, TryInto};
use std::ops::Deref;

#[derive(Clone)]
pub struct Storage {
	kind: StorageKind,
	pool: AnyPool,
}

impl Storage {
	pub async fn new(uri: &str) -> Result<Self> {
		let options: AnyConnectOptions = uri.parse()?;
		Ok(Self {
			kind: options.kind().try_into()?,
			pool: AnyPool::connect_with(options).await?,
		})
	}

	pub fn kind(&self) -> StorageKind {
		self.kind
	}

	/*pub fn configurator(&self, config: &Config) -> Result<Configurator> {
		match self.kind {
			StorageKind::Sqlite => Ok(Configurator::File(FileConfigurator::new(
				config
					.module_config_dir()
					.ok_or_else(|| anyhow!("Module configuration directory not found"))?,
			)?)),
			StorageKind::Postgres => Ok(Configurator::Db(DbConfigurator::new(self.pool.clone()))),
		}
	}*/
}

impl Deref for Storage {
	type Target = AnyPool;

	fn deref(&self) -> &Self::Target {
		&self.pool
	}
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum StorageKind {
	Sqlite,
	Postgres,
}

impl StorageKind {
	pub fn is_sqlite(self) -> bool {
		self == StorageKind::Sqlite
	}

	pub fn is_postgres(self) -> bool {
		self == StorageKind::Postgres
	}
}

impl TryFrom<AnyKind> for StorageKind {
	type Error = anyhow::Error;

	fn try_from(kind: AnyKind) -> Result<Self> {
		match kind {
			AnyKind::Sqlite => Ok(StorageKind::Sqlite),
			// AnyKind::Postgres => Ok(StorageKind::Postgres),
			_ => Err(anyhow!("Unsupported db kind")),
		}
	}
}
