//! How many containers are running, when anything is.
//!
//! `docker ps` with `--format '{{.Names}}'` rather than bare: one name
//! per line is a count, and a table is a thing to parse. The plugin asks
//! for `docker` — and, if the user grants it, `podman` — in its `exec`
//! allowlist. Both are narrow in the sense that matters: the allowlist
//! covers `argv[0]` only, so whatever is granted can be run with any
//! arguments at all, and a plugin that asked for a general-purpose tool
//! would have quietly taken the whole machine with it. `docker` is not
//! exactly harmless, since a daemon that can start containers can do a
//! great deal, but it is a program a user can reason about: granting it
//! means "this plugin may talk to Docker", which is a sentence with a
//! meaning. `sh` is not.
//!
//! # Why the count is cached
//!
//! `render` is called on every frame Haven draws, and `docker ps` is a
//! round trip to a daemon over a socket — slow when the daemon is busy,
//! and five seconds and then killed when it is wedged. None of that
//! belongs in the draw path. So the command runs on `Event::Started` and
//! on `Event::Tick`, and `render` prints the number that is already
//! known.
//!
//! # Why it usually draws nothing
//!
//! Most machines have no Docker, and a status bar reading "cannot
//! connect to the Docker daemon" for the rest of time is clutter that
//! cannot be acted upon. So the three outcomes are told apart:
//!
//!   * not installed, or a daemon that will not answer — nothing at all;
//!   * installed, answering, nothing running — a dim `docker 0`, which
//!     is worth the four characters because it says the daemon is up;
//!   * containers running — `docker 3`.
//!
//! A refusal is the exception and *is* shown, because being refused a
//! capability is a thing the user can fix and silence would leave them
//! with no way to know. The host's refusal has the program's name in it
//! and that sentence is shown as it stands.
//!
//! # Podman
//!
//! Handled, by trying `podman ps` when `docker` is absent. Its `ps`
//! takes the same `--format` and prints the same one-name-per-line, so
//! there is nothing else to do. A user who has only one of the two
//! grants the one they have; the other comes back as a refusal, which is
//! why a refusal is only shown when *neither* engine produced an answer.

use std::sync::Mutex;

use haven_plugin_api::{dim, output_of, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(DockerStatus);

struct DockerStatus;

static STATE: Mutex<Option<Reading>> = Mutex::new(None);

enum Reading {
    /// An engine answered. The engine's own name, so the segment says
    /// `podman 2` on a machine that runs podman.
    Running { engine: &'static str, count: usize },
    /// Nothing to say: no engine installed, or none that would answer.
    Quiet,
    /// Not granted. The one case that is worth drawing.
    Why(String),
}

/// In order of how likely they are to be the one the user meant.
const ENGINES: [&str; 2] = ["docker", "podman"];

impl Plugin for DockerStatus {
    fn on_event(event: Event) -> Vec<Command> {
        if matches!(event, Event::Started | Event::Tick) {
            *STATE.lock().unwrap() = Some(ask());
        }
        Vec::new()
    }

    fn render() -> Vec<Run> {
        match &*STATE.lock().unwrap() {
            // Before the first event, and on a machine with no engine:
            // the same nothing. This segment is absent far more often
            // than it is present.
            None | Some(Reading::Quiet) => Vec::new(),
            Some(Reading::Why(why)) => vec![plain(why.clone())],
            Some(Reading::Running { engine, count }) => {
                let text = format!("{engine} {count}");
                // Zero containers with a daemon that answered is true
                // but not news, so it is dim.
                if *count == 0 {
                    vec![dim(text)]
                } else {
                    vec![plain(text)]
                }
            }
        }
    }
}

/// Ask each engine in turn, and keep the first answer.
fn ask() -> Reading {
    let mut refusal = None;
    for engine in ENGINES {
        match output_of(&[engine, "ps", "--format", "{{.Names}}"]) {
            Ok(text) => return Reading::Running { engine, count: names(&text) },
            Err(why) => {
                // A capability we were not granted is worth telling the
                // user about; a program that is not installed, or a
                // daemon that is not listening, is not. The host gives
                // us one string for both, so this is a match on its
                // wording — see the README.
                if refusal.is_none() && why.contains("not granted") {
                    refusal = Some(why);
                }
            }
        }
    }
    match refusal {
        Some(why) => Reading::Why(why),
        None => Reading::Quiet,
    }
}

/// How many container names came back.
///
/// `output_of` has already trimmed the trailing newline, so an empty
/// string is no containers rather than one with no name — but a blank
/// line is skipped anyway, because an engine that prints one would
/// otherwise count as a container.
fn names(text: &str) -> usize {
    text.lines().filter(|line| !line.trim().is_empty()).count()
}
