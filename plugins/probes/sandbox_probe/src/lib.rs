//! Tries to leave the sandbox, and reports what happened.
//!
//! Nothing here is granted, and nothing here should work: the WASI
//! context the host builds has no filesystem, no network and no
//! environment in it. If this ever prints the contents of a file, a
//! capability was handed out that should not have been.
use haven_plugin_api::{plain, Plugin, Run};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn render() -> Vec<Run> {
        match std::fs::read_to_string("/etc/passwd") {
            Ok(text) => vec![plain(text.chars().take(40).collect::<String>())],
            Err(e) => vec![plain(format!("denied: {}", e.kind()))],
        }
    }
}
