//! Reacts to what another plugin said, and draws it.
//!
//! Also proves the host stamps the sender: the plugin reports who told
//! it, and nothing it was given could have set that.
use haven_plugin_api::{host, plain, Command, Event, Plugin, Run};
haven_plugin_api::plugin!(P);
struct P;

thread_local! {
    static HEARD: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

impl Plugin for P {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::Told(told) => {
                HEARD.with(|h| {
                    *h.borrow_mut() = format!("{}:{}:{}", told.sender, told.name, told.data)
                });
                host::log("heard something");
                vec![Command::Notice(format!("{} said {}", told.sender, told.name))]
            }
            _ => Vec::new(),
        }
    }
    fn render() -> Vec<Run> {
        HEARD.with(|h| {
            let heard = h.borrow().clone();
            if heard.is_empty() {
                Vec::new()
            } else {
                vec![plain(heard)]
            }
        })
    }
}
