//! Emits one event when a directory changes, so something else can hear it.
use haven_plugin_api::{emit, Command, Event, Plugin};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::CwdChanged(change) => vec![emit("env-activated", change.cwd)],
            _ => Vec::new(),
        }
    }
}
