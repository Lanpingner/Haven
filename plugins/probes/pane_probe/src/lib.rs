//! Looks at the panes, and at what one is showing.
use haven_plugin_api::{focused, host, plain, Command, Event, Plugin, Run};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn on_event(_event: Event) -> Vec<Command> {
        let panes = host::panes();
        let here = focused().map(|p| p.title).unwrap_or_else(|| "nothing".into());
        vec![Command::Notice(format!("{} panes, focus on {here}", panes.len()))]
    }
    fn render() -> Vec<Run> {
        match host::pane_text(1, 5) {
            Ok(text) => vec![plain(text.lines().next().unwrap_or("").to_string())],
            Err(why) => vec![plain(why)],
        }
    }
}
