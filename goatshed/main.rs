use anyhow::{bail, Result};
use common::discord::types::Event;
use common::discord::{Builder, GatewayError, GatewayEvent};
use common::EventHandler;
use config::Config;
use futures::channel::mpsc;
use futures::SinkExt;
use futures::StreamExt;
use log::{info, warn, LevelFilter};
use log4rs::append::console::ConsoleAppender;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config as LogConfig, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;
use std::sync::Arc;
use tokio::signal;

mod config;
mod radio;

#[tokio::main]
async fn main() {
	if let Err(e) = real_main().await {
		panic!("{}", e);
	}
}

async fn real_main() -> Result<()> {
	let env = dotenv::dotenv().is_ok();
	let config = Arc::new(Config::from_env()?);

	let pattern = "{d(%H:%M:%S)} {h({l})} {t} - {m}{n}";

	// Logging
	let stdout = ConsoleAppender::builder()
		.encoder(Box::new(PatternEncoder::new(pattern)))
		.build();
	let mut builder =
		LogConfig::builder().appender(Appender::builder().build("stdout", Box::new(stdout)));
	let mut root = Root::builder().appender("stdout");
	if config.log_file {
		let file = FileAppender::builder()
			.encoder(Box::new(PatternEncoder::new(pattern)))
			.build("goatshed.log")?;
		builder = builder.appender(Appender::builder().build("file", Box::new(file)));
		root = root.appender("file");
	}

	let log_config = builder
		.logger(Logger::builder().build("goatshed", LevelFilter::Debug))
		.logger(Logger::builder().build("discord_async::voice", LevelFilter::Info))
		.logger(Logger::builder().build("sqlx", LevelFilter::Warn))
		.build(root.build(LevelFilter::Info))?;
	log4rs::init_config(log_config)?;

	warn!("Starting..");

	// Global configuration
	if env {
		info!("Loaded .env file");
	}
	let guild_id = config.guild_id;

	let (ev_send, mut ev_recv) = mpsc::unbounded();
	let mut ev_send = Some(ev_send);
	let mut discord = Builder::new(config.discord_api_token.clone(), move |ev| {
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

	let shutdown = discord.shutdown().expect("shutdown signal missing");
	tokio::spawn(async move {
		let _ = signal::ctrl_c().await;
		warn!("Received interrupt signal, shutting down..");
		shutdown.send();
	});

	let mut guild = match gc {
		Some(gc) => discord.guild(ev_recv, gc).await?,
		None => bail!("Connection closed before receiving our guild"),
	};

	let mut chain = radio::Radio::new(
		&guild,
		config.broadcast_channel_id,
		config.announce_channel_id,
		config.broadcast_bitrate,
	)?;

	while let Some(event) = guild.next().await {
		chain.guild_event(&guild, &event);
	}
	let _ = discord.handle().await;

	warn!("Goodbye");
	Ok(())
}
