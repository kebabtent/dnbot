use common::discord::types::Event;
use common::{EventHandler, Guild};

pub struct Filter;

impl Filter {
	pub fn new() -> Self {
		Self {}
	}
}

impl EventHandler for Filter {
	fn event(&mut self, _guild: &Guild, event: &Event) -> bool {
		if let Event::MessageCreate(mc) = event {
			let m = &mc.message;
			if m.webhook_id.is_some() || !m.message_type.is_textual() {
				return false;
			}
		}
		true
	}
}
