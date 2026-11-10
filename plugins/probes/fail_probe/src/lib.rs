//! Refuses to start, so a load failure has something to report.
use haven_plugin_api::Plugin;
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn init() -> Result<(), String> {
        Err("on purpose".into())
    }
}
