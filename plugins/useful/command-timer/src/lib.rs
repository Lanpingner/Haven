//! How long the last command took, when it took long enough to matter.
//!
//! The whole plugin is one judgement: a duration is worth saying out
//! loud only when the person was not watching it happen. `ls` in eight
//! milliseconds is not news, and a notice for it is noise that teaches
//! people to ignore notices. `cargo build` in two minutes and fourteen
//! seconds is news, because by then they had gone to make tea and came
//! back without knowing whether it was quick or slow.
//!
//! So there is a threshold, and it is ten seconds. Ten is defensible on
//! two grounds. It is roughly where attention goes elsewhere — the
//! usability literature's figure for the limit of keeping a person's
//! attention on a response is ten seconds, and past it they task-switch.
//! And it is well clear of the things a shell does all day: a `git
//! status`, a `cargo check` on a warm cache, a `grep` over a repository
//! all land under it, so the plugin stays quiet during ordinary work and
//! only speaks for the builds, the test runs and the downloads.
//!
//! The last duration is also kept for the status bar, and that is
//! deliberate rather than free. A notice is shown for a moment; the
//! question "how long did that actually take" is usually asked after the
//! moment has passed, which is exactly when a notice is no longer there.
//! A small segment answers it without anyone having to time anything. It
//! is only updated for commands that passed the threshold, for the same
//! reason the notice is: a segment that rewrote itself to `0.0s` after
//! every `cd` would be a flicker in the corner of the eye rather than
//! information.
//!
//! State goes through `Command::Remember` rather than a `static`. A
//! `static` with interior mutability does survive between calls within
//! one loaded instance, which would be enough for a cache, but this is
//! not a cache: the last duration is worth having across a reload of the
//! plugin and across a restart of the daemon, and the store is the only
//! thing that gives that.

use haven_plugin_api::{dim, host, plain, Command, Event, Plugin, Remembered, Run};

haven_plugin_api::plugin!(CommandTimer);

struct CommandTimer;

/// Below this, the person watched it happen and already knows.
const WORTH_SAYING: u64 = 10_000;

/// Where the last noteworthy duration is kept, for `render`.
const LAST: &str = "last";

impl Plugin for CommandTimer {
    fn on_event(event: Event) -> Vec<Command> {
        let Event::CommandEnded(end) = event else { return Vec::new() };
        if end.millis < WORTH_SAYING {
            return Vec::new();
        }
        let took = duration(end.millis);
        vec![
            Command::Notice(format!("{} took {took}", short(&end.command))),
            Command::Remember(Remembered { key: LAST.into(), value: took }),
        ]
    }

    fn render() -> Vec<Run> {
        match host::recall(LAST) {
            // Dim, because it is a fact about the past rather than about
            // now, and the status bar belongs to the present.
            Some(took) => vec![dim("last "), plain(took)],
            None => Vec::new(),
        }
    }
}

/// A duration the way a person says one: `8.2s`, `2m14s`, `1h03m`.
///
/// Precision falls away as the number grows, which is how people talk
/// about time. Tenths matter at ten seconds and are absurd at an hour,
/// so an hour-long command is reported to the minute and nobody feels
/// short-changed.
fn duration(millis: u64) -> String {
    let secs = millis / 1000;
    if secs < 60 {
        return format!("{}.{}s", secs, (millis % 1000) / 100);
    }
    if secs < 3600 {
        return format!("{}m{:02}s", secs / 60, secs % 60);
    }
    format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
}

/// The command, short enough for a notice.
///
/// A shell reports whatever was typed, which can be a pipeline with a
/// path in it. The first two words are what identifies it — `cargo
/// build`, `npm test` — and the rest is detail the person who typed it
/// does not need read back.
fn short(command: &str) -> String {
    let mut words = command.split_whitespace();
    let mut out = String::new();
    for word in words.by_ref().take(2) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word.rsplit('/').next().unwrap_or(word));
    }
    if words.next().is_some() {
        out.push('…');
    }
    if out.is_empty() {
        "a command".into()
    } else {
        out
    }
}
