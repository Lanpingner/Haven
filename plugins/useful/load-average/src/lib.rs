//! The machine's one-minute load average, on the status bar.
//!
//! There is no filesystem in a plugin's sandbox, so `/proc/loadavg` is
//! not an option: the only way out is `host::run` with an allowlisted
//! program. `uptime` is the right one to ask for. It does a single thing,
//! takes no interesting arguments, and reads nothing the user could be
//! surprised by, which is what a program in an `exec` allowlist should
//! look like. The allowlist matches `argv[0]` and nothing else, so a
//! plugin that asked for `cat` or `sh` instead would have been granted
//! the whole disk; the narrowness of the program *is* the permission.
//!
//! # Why the reading is cached
//!
//! `render` is called every time Haven draws a frame, and the host gives
//! a program five seconds before it kills it. Shelling out from `render`
//! would put a process spawn — and in the bad case a five-second stall —
//! inside the draw path, which is the one place in a terminal emulator
//! that must never block. So `uptime` runs on `Event::Started` and on
//! each `Event::Tick`, the figures are kept in a `static`, and `render`
//! only formats what is already known. That is also why this plugin
//! subscribes to `tick`: without it the reading would be whatever it was
//! when the plugin loaded, forever.
//!
//! # Why a second capability
//!
//! A load of 8 is idle on a sixteen-core machine and desperate on a
//! two-core one, so the figure alone does not say anything. The core
//! count comes from `nproc`, which is a second program in the allowlist
//! and so a second thing the user has to agree to. It is worth it: the
//! whole point of the segment is to say *whether the machine is busy*,
//! and `nproc` is about as small a capability as exists — it prints one
//! integer, ignores its arguments for our purposes, and opens no files
//! the plugin could name. It runs once, on `Started`, because the number
//! of cores does not change while Haven is running.
//!
//! Without `nproc` the plugin still works and simply shows the figure
//! plain, with no threshold to compare it against.

use std::sync::Mutex;

use haven_plugin_api::{bold, dim, host, output_of, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(LoadAverage);

struct LoadAverage;

/// What the last refresh learned. `None` until the first one.
static STATE: Mutex<Option<Reading>> = Mutex::new(None);
/// Cores, looked up once. `None` when `nproc` was refused or missing.
static CORES: Mutex<Option<u32>> = Mutex::new(None);

/// Either a load figure or the reason there is not one.
enum Reading {
    /// The figure as `uptime` printed it, and the same figure in
    /// hundredths so it can be compared with the core count. Kept as
    /// integers throughout: a plugin is downloaded by whoever installs
    /// it, and `f64` parsing and formatting is tens of kilobytes to
    /// learn something two divisions answer.
    Load {
        shown: String,
        hundredths: u32,
    },
    Why(String),
}

impl Plugin for LoadAverage {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            Event::Started => {
                // Cores first, so the very first frame can already
                // compare against them.
                *CORES.lock().unwrap() = cores();
                refresh();
            }
            Event::Tick => refresh(),
            _ => {}
        }
        Vec::new()
    }

    fn render() -> Vec<Run> {
        let state = STATE.lock().unwrap();
        match &*state {
            // Drawn before the first event reached us. Honest and short.
            None => vec![dim("load …")],
            Some(Reading::Why(why)) => vec![plain(why.clone())],
            Some(Reading::Load { shown, hundredths }) => {
                let text = shown.clone();
                match *CORES.lock().unwrap() {
                    // Busier than the machine has cores for: say so in a
                    // weight the eye catches, rather than in a colour,
                    // which would fight the theme.
                    Some(cores) if *hundredths > cores * 100 => vec![bold(text)],
                    _ => vec![plain(text)],
                }
            }
        }
    }
}

/// Run `uptime` and keep what it said, including why it could not.
fn refresh() {
    let reading = match output_of(&["uptime"]) {
        Ok(text) => match one_minute(&text) {
            Some((shown, hundredths)) => Reading::Load { shown, hundredths },
            None => {
                host::log(&format!("load-average: could not parse `{text}`"));
                Reading::Why("load ?".into())
            }
        },
        // A refusal, or `uptime` not being installed. Either way the
        // sentence the host gave us is the most useful thing to show.
        Err(why) => Reading::Why(why),
    };
    *STATE.lock().unwrap() = Some(reading);
}

/// The core count, or `None` if we were not allowed to ask.
fn cores() -> Option<u32> {
    output_of(&["nproc"]).ok()?.trim().parse().ok()
}

/// The one-minute figure out of a line of `uptime`.
///
/// The two shapes this has to cope with:
///
/// ```text
/// 18:03:44 up 22:30,  2 users,  load average: 1.41, 3.62, 3.13   (Linux)
/// 18:03  up 3 days,  2 users, load averages: 1.41 3.62 3.13      (macOS)
/// ```
///
/// So: find the colon that follows the words `load average`, which both
/// have, and read the first number after it. Splitting on commas would
/// work on Linux and not on macOS, and counting fields from the start
/// would break on a machine that has been up for a different length of
/// time — the tail is the stable part of the line.
fn one_minute(text: &str) -> Option<(String, u32)> {
    let at = text.find("load average")?;
    let rest = &text[at..];
    let figures = &rest[rest.find(':')? + 1..];
    let first: String = figures
        .trim_start()
        .chars()
        // A comma here is either the separator between the three figures
        // or, in a locale that writes `1,41`, the decimal point. Stopping
        // at the first non-numeric character and treating a comma as a
        // point gets both right.
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    let shown = first.trim_end_matches('.').to_string();
    Some((shown.clone(), hundredths(&shown)?))
}

/// `1.41` as 141, which is all the comparing against a core count needs.
fn hundredths(text: &str) -> Option<u32> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut digits = fraction.chars().filter(|c| c.is_ascii_digit());
    let tens = digits.next().and_then(|c| c.to_digit(10)).unwrap_or(0);
    let ones = digits.next().and_then(|c| c.to_digit(10)).unwrap_or(0);
    Some(whole.parse::<u32>().ok()?.saturating_mul(100) + tens * 10 + ones)
}
