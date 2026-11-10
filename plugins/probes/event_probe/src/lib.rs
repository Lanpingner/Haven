//! Turns events into commands, counts ticks, and logs.
use haven_plugin_api::{host, plain, Command, Event, Plugin, Remembered, Run};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::Started => {
                host::log("started");
                Vec::new()
            }
            Event::CommandEnded(end) if end.status != 0 => vec![
                Command::Notice(format!("{} failed after {}s", end.command, end.millis / 1000)),
                Command::Act("display-panes".into()),
            ],
            Event::Tick => {
                let seen: u64 = host::recall("ticks").and_then(|v| v.parse().ok()).unwrap_or(0);
                vec![Command::Remember(Remembered {
                    key: "ticks".into(),
                    value: (seen + 1).to_string(),
                })]
            }
            _ => Vec::new(),
        }
    }
    fn render() -> Vec<Run> {
        vec![plain(format!("ticks: {}", host::recall("ticks").unwrap_or_else(|| "0".into())))]
    }
}
