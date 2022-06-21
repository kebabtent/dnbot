#![recursion_limit = "1024"]

use anyhow::{anyhow, bail, Result};
use common::discord::types::Event;
use common::discord::{Builder, GatewayError, GatewayEvent};
use common::{EventHandler, Storage};
use config::Config;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use log::{info, warn, LevelFilter};
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config as LogConfig, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;
use modules::Configurator;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::select;
use warp::Filter;

mod config;
mod modules;

#[tokio::main]
async fn main() {
	if let Err(e) = real_main().await {
		panic!("{}", e);
	}
}

async fn real_main() -> Result<()> {
	// Logging
	let encoder = PatternEncoder::new("{d(%H:%M:%S)} {h({l})} {t} - {m}{n}");
	let stdout = ConsoleAppender::builder()
		.encoder(Box::new(encoder))
		.build();
	let log_config = LogConfig::builder()
		.appender(Appender::builder().build("stdout", Box::new(stdout)))
		.logger(Logger::builder().build("galaxyofdreams", LevelFilter::Debug))
		.logger(Logger::builder().build("discord_async::voice", LevelFilter::Debug))
		.logger(Logger::builder().build("sqlx", LevelFilter::Warn))
		.build(Root::builder().appender("stdout").build(LevelFilter::Info))?;
	log4rs::init_config(log_config)?;

	warn!("Starting..");

	// Global configuration
	if dotenv::dotenv().is_ok() {
		info!("Loaded .env file");
	}
	let config = Arc::new(Config::from_env()?);
	let guild_id = config.guild_id;
	let storage = Storage::new(&config.db_uri).await?;

	let (ev_send, mut ev_recv) = mpsc::unbounded();
	let mut ev_send = Some(ev_send);
	let discord = Builder::new(config.discord_api_token.clone(), move |ev| {
		if ev.is_none() {
			// Shutting down: drop the sender
			ev_send = None;
		}
		let ev_send = ev_send.clone();
		async move {
			let (mut ev_send, ev) = match (ev_send, ev) {
				(Some(s), Some(e)) => (s, e),
				_ => return Ok(()),
			};

			if let GatewayEvent::Event(e) = &ev {
				if e.guild_id().map(|i| i != guild_id).unwrap_or(false) {
					return Ok(());
				}
			}
			ev_send.send(ev).await.map_err(|_| GatewayError::Shutdown)
		}
	})
	.build()
	.await?;

	// Discard events until we find our guild
	let mut gc = None;
	while let Some(ev) = ev_recv.next().await {
		if let GatewayEvent::Event(Event::GuildCreate(c)) = ev {
			gc = Some(c);
			break;
		}
	}

	let mut guild = match gc {
		Some(gc) => discord.guild(ev_recv, gc).await?,
		None => bail!("Connection closed before receiving our guild"),
	};

	// Configurator allows on-the-fly changes to module configuration
	let module_config_dir = config
		.module_config_dir()
		.ok_or_else(|| anyhow!("Module configuration directory not found"))?;
	let mut configurator = Configurator::new(&storage, module_config_dir)?;

	// The configurator is not necessarily cancellation safe so we have to
	// move it to its own task and use a channel to receive its events.
	// See: https://docs.rs/tokio/1.11.0/tokio/macro.select.html#cancellation-safety
	let (mut config_send, mut config_recv) = mpsc::channel(16);
	tokio::spawn(async move {
		loop {
			let item = match configurator.next().await {
				Ok(c) => c,
				Err(e) => {
					warn!("Configurator: {}", e);
					continue;
				}
			};
			if config_send.send(item).await.is_err() {
				// Main task went away
				break;
			}
		}
	});

	// Set up our modules
	let youtube = modules::Youtube::new(discord.client(), &config.http_ext_url());
	let astronauts = modules::Astronauts::new(&guild, storage.clone()).await?;
	// let collab = modules::CollabPlaylist::new(storage.clone()).await?;
	// let routes = youtube.routes().or(collab.routes()).or(astronauts.routes());
	let routes = youtube.routes().or(astronauts.routes());

	let mut chain = modules::Filter::new()
		// .chain(modules::Automod::new())
		// .chain(modules::DJ::new())
		// .chain(modules::Levels::new(storage.clone()).await?)
		.chain(modules::Joined::new())
		.chain(modules::Commands::new())
		.chain(modules::LinkOnly::new())
		.chain(modules::RoleAssign::new(discord.client(), storage.clone()).await?)
		// .chain(collab)
		.chain(astronauts)
		.chain(youtube);

	// HTTP server
	let addr: SocketAddr = format!("0.0.0.0:{}", config.http_port()).parse()?;
	let http = warp::serve(routes).bind(addr);
	tokio::spawn(http);

	loop {
		select! {
			c = config_recv.next() => {
				match c {
					Some((name, config)) => {
						chain.config(&guild, &name, config);
					}
					None => break
				}
			}
			ge = guild.next() => {
				match ge {
					Some(event) => chain.guild_event(&guild, &event),
					None => break
				}
			}
		};
	}

	info!("Done");
	Ok(())
}
