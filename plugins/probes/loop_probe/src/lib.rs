//! Answers every event with another one, forever.
//!
//! Two of these would talk to each other until the machine stopped, so
//! the host bounds how many rounds an emission may cause. This exists to
//! be bounded.
use haven_plugin_api::{emit, Command, Event, Plugin};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::Told(told) => vec![emit("pong", told.data)],
            Event::CwdChanged(_) => vec![emit("ping", "start".to_string())],
            _ => Vec::new(),
        }
    }
}
