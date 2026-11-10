//! A pomodoro timer on the status bar: twenty-five minutes of work, five
//! of break, and a longer break every fourth.
//!
//! # The clock is the tick
//!
//! A plugin has no timer of its own and cannot ask to be woken. The only
//! heartbeat is the `tick` event, which the manifest asks for as
//! `timer:2s`, so this plugin counts ticks and multiplies. Two seconds is
//! a compromise in both directions: a one-second tick would be eighty-six
//! thousand calls into a sandbox per day to move a counter that a person
//! glances at a few times an hour, and a ten-second tick would make the
//! displayed seconds lurch in tens and would overshoot the end of a phase
//! by up to ten seconds. At two seconds the display is honest to the
//! second it shows, and a whole work phase costs seven hundred and fifty
//! calls.
//!
//! **Ticks will be late, and some will be lost** — the daemon coalesces
//! and rate-limits events per plugin, the machine sleeps, and a plugin is
//! reloaded on update. This plugin counts ticks rather than consulting a
//! wall clock, which has a consequence worth stating plainly: a lost tick
//! makes the phase two seconds longer, and the error accumulates. Over a
//! laptop suspended for an hour, the work phase simply did not advance,
//! and the timer resumes where it was rather than announcing four phases
//! at once. That is the behaviour wanted here. A pomodoro is a nudge to
//! stop working, not a stopwatch; a timer that catches up by firing three
//! notices in a row is worse than one that runs a little long, and one
//! that says "break over" for a break you slept through is nonsense. The
//! cost is that the timer drifts slow, and the README says so rather
//! than pretending otherwise.
//!
//! # Surviving a reload
//!
//! Everything is in the store, through `Command::Remember`, because a
//! timer that forgot which phase it was in whenever the plugin was
//! reloaded would be useless — the reload is invisible to the person
//! watching the bar, so the timer must be too. A `static` would survive
//! calls within one instance and nothing beyond it, which is exactly the
//! wrong lifetime for this.
//!
//! All of it is packed into one key, as `phase:ticks:completed`, rather
//! than three keys. `Command::Remember` carries a single key and value,
//! so three keys would be three commands the host applies one at a time,
//! and a crash or an unload between them would leave a phase that does
//! not match its count. One key is one command and cannot half-apply.

use haven_plugin_api::{bold, dim, host, plain, Command, Event, Plugin, Remembered, Run};

haven_plugin_api::plugin!(Pomodoro);

struct Pomodoro;

/// What the manifest asks for, and what every count below is measured in.
const TICK_SECS: u32 = 2;

/// The classic lengths, in ticks.
const WORK: u32 = 25 * 60 / TICK_SECS;
const SHORT_BREAK: u32 = 5 * 60 / TICK_SECS;
const LONG_BREAK: u32 = 15 * 60 / TICK_SECS;

/// How many work phases earn the long break.
const BEFORE_LONG: u32 = 4;

const STATE: &str = "state";

/// Which half of the cycle we are in. Three rather than two, because a
/// long break is shown differently and is not followed by another one.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Work,
    Break,
    Long,
}

impl Phase {
    fn ticks(self) -> u32 {
        match self {
            Phase::Work => WORK,
            Phase::Break => SHORT_BREAK,
            Phase::Long => LONG_BREAK,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Work => "work",
            Phase::Break => "break",
            Phase::Long => "long",
        }
    }
}

/// The whole of what this plugin knows.
struct State {
    phase: Phase,
    ticks: u32,
    /// Work phases finished, which is what decides the fourth break.
    done: u32,
}

impl Plugin for Pomodoro {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            // A first run has nothing in the store, and writing the
            // starting state here means `render` never has to invent one.
            Event::Started => vec![save(&State { phase: Phase::Work, ticks: 0, done: 0 })],
            Event::Tick => {
                let mut state = load();
                state.ticks += 1;
                if state.ticks < state.phase.ticks() {
                    return vec![save(&state)];
                }
                let (next, said) = advance(&state);
                vec![Command::Notice(said), save(&next)]
            }
            _ => Vec::new(),
        }
    }

    fn render() -> Vec<Run> {
        let state = load();
        let left = state.phase.ticks().saturating_sub(state.ticks) * TICK_SECS;
        let clock = format!("{}:{:02}", left / 60, left % 60);
        // Work is the phase to notice; a break is a phase to ignore until
        // it ends, which is what `dim` is for.
        match state.phase {
            Phase::Work => vec![bold("work "), plain(clock)],
            _ => vec![dim(format!("{} ", state.phase.label())), dim(clock)],
        }
    }
}

/// The next phase, and what to say about the one that ended.
fn advance(state: &State) -> (State, String) {
    match state.phase {
        Phase::Work => {
            let done = state.done + 1;
            if done % BEFORE_LONG == 0 {
                (State { phase: Phase::Long, ticks: 0, done }, "four done — take 15".into())
            } else {
                (State { phase: Phase::Break, ticks: 0, done }, "time's up — take 5".into())
            }
        }
        // A break ending always leads back to work, long or not.
        Phase::Break | Phase::Long => (
            State { phase: Phase::Work, ticks: 0, done: state.done },
            "break over — back to it".into(),
        ),
    }
}

/// `phase:ticks:done`, as one value, for the reason in the module note.
fn save(state: &State) -> Command {
    Command::Remember(Remembered {
        key: STATE.into(),
        value: format!("{}:{}:{}", state.phase.label(), state.ticks, state.done),
    })
}

/// What was remembered, or a fresh work phase.
///
/// Anything unparseable is treated as absent rather than as an error. The
/// store is a string map that a future version of this plugin may have
/// written, and starting a new pomodoro is a better answer to a value we
/// do not understand than a blank status bar.
fn load() -> State {
    let fresh = State { phase: Phase::Work, ticks: 0, done: 0 };
    let Some(value) = host::recall(STATE) else { return fresh };
    let mut parts = value.split(':');
    let phase = match parts.next() {
        Some("work") => Phase::Work,
        Some("break") => Phase::Break,
        Some("long") => Phase::Long,
        _ => return fresh,
    };
    let mut number = || parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    State { phase, ticks: number(), done: number() }
}
