//! The time, on the status bar.
//!
//! The smallest plugin that is still worth having, and the one that
//! proves the shape: no capabilities at all, nothing but `render`.

use haven_plugin_api::{plain, Plugin, Run};

haven_plugin_api::plugin!(Clock);

struct Clock;

impl Plugin for Clock {
    fn render() -> Vec<Run> {
        vec![plain(now())]
    }
}

/// `HH:MM`, from the one clock a sandboxed component is allowed.
///
/// WASI's clock is granted because `std::time` needs it and nothing
/// about the time is sensitive. Formatting it by hand rather than
/// pulling in a date library: a plugin is downloaded by whoever installs
/// it, and `chrono` is a lot of kilobytes to learn the hour.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs % 86_400;
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}
