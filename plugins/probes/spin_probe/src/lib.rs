//! Never returns. Exists so the fuel limit has something to stop.
use haven_plugin_api::{Plugin, Run};
haven_plugin_api::plugin!(P);
struct P;
impl Plugin for P {
    fn render() -> Vec<Run> {
        let mut n: u64 = 0;
        loop {
            n = n.wrapping_add(1);
            std::hint::black_box(n);
        }
    }
}
