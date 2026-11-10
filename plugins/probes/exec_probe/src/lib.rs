//! Runs a program, or reports being refused one.
use haven_plugin_api::{output_of, plain, Plugin, Run};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn render() -> Vec<Run> {
        match output_of(&["/bin/echo", "haven"]) {
            Ok(out) => vec![plain(format!("ran: {out}"))],
            Err(why) => vec![plain(why)],
        }
    }
}
