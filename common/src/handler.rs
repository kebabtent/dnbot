use discord::voice::Updater;
use discord::{types::Event, GatewayEvent};
use serde_json::Value;

// We would like to prevent generics bleeding up everywhere.
// Luckily all of our binaries use the same `S` in `Guild<S>`,
// which allows us to define our handlers in terms of that type.
// This might change in the future, forcing us to bite the bullet.
pub type Guild = discord::Guild<futures::channel::mpsc::UnboundedReceiver<GatewayEvent>>;

pub trait EventHandler {
	fn config(&mut self, _guild: &Guild, _name: &str, config: Value) -> Option<Value> {
		Some(config)
	}

	fn event(&mut self, _guild: &Guild, _event: &Event) -> bool {
		true
	}

	fn guild_online(&mut self, _guild: &Guild) {}

	fn guild_offline(&mut self, _guild: &Guild) {}

	fn session_invalidated(&mut self, _guild: &Guild) {}

	fn guild_event(&mut self, guild: &Guild, guild_event: &GatewayEvent) {
		match guild_event {
			GatewayEvent::Event(event) => {
				self.event(guild, event);
			}
			GatewayEvent::Online => self.guild_online(guild),
			GatewayEvent::Offline => self.guild_offline(guild),
			GatewayEvent::SessionInvalidated => self.session_invalidated(guild),
		}
	}

	fn chain<B>(self, b: B) -> Chain<Self, B>
	where
		Self: Sized,
		B: EventHandler,
	{
		Chain { a: self, b }
	}
}

pub struct Chain<A, B> {
	a: A,
	b: B,
}

impl<A, B> EventHandler for Chain<A, B>
where
	A: EventHandler,
	B: EventHandler,
{
	fn config(&mut self, guild: &Guild, name: &str, config: Value) -> Option<Value> {
		if let Some(config) = self.a.config(guild, name, config) {
			self.b.config(guild, name, config)
		} else {
			None
		}
	}

	fn event(&mut self, guild: &Guild, event: &Event) -> bool {
		if self.a.event(guild, event) {
			self.b.event(guild, event)
		} else {
			false
		}
	}

	fn guild_online(&mut self, guild: &Guild) {
		self.a.guild_online(guild);
		self.b.guild_online(guild);
	}

	fn guild_offline(&mut self, guild: &Guild) {
		self.a.guild_offline(guild);
		self.b.guild_offline(guild);
	}

	fn session_invalidated(&mut self, guild: &Guild) {
		self.a.session_invalidated(guild);
		self.b.session_invalidated(guild);
	}
}

pub trait HasUpdater {
	fn updater(&mut self) -> &mut Updater;
}

pub trait VoiceEventHandler {
	fn config(&mut self, _guild: &Guild, _name: &str, config: Value) -> Option<Value> {
		Some(config)
	}

	fn event(&mut self, _guild: &Guild, _event: &Event) -> bool {
		true
	}

	fn guild_online(&mut self, _guild: &Guild) {}

	fn guild_offline(&mut self, _guild: &Guild) {}

	fn session_invalidated(&mut self, _guild: &Guild) {}
}

impl<T> EventHandler for T
where
	T: VoiceEventHandler + HasUpdater,
{
	fn config(&mut self, guild: &Guild, name: &str, config: Value) -> Option<Value> {
		VoiceEventHandler::config(self, guild, name, config)
	}

	fn event(&mut self, guild: &Guild, event: &Event) -> bool {
		let _ = match event {
			Event::VoiceServerUpdate(u) => self.updater().server_update(u.clone()),
			Event::VoiceStateUpdate(u) => self.updater().state_update(u.clone()),
			_ => false,
		};
		VoiceEventHandler::event(self, guild, event)
	}

	fn guild_online(&mut self, guild: &Guild) {
		self.updater().guild_online();
		VoiceEventHandler::guild_online(self, guild)
	}

	fn guild_offline(&mut self, guild: &Guild) {
		self.updater().guild_offline();
		VoiceEventHandler::guild_offline(self, guild)
	}

	fn session_invalidated(&mut self, guild: &Guild) {
		self.updater().session_invalidated();
		VoiceEventHandler::session_invalidated(self, guild)
	}
}
