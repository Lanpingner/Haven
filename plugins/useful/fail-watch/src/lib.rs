//! Say when a command failed, in words, and only when it failed.
//!
//! An exit status is a number that means something, and almost nothing
//! in a terminal tells you which something. `127` is the shell saying it
//! could not find what you typed, which is usually a typo or a missing
//! package; `126` is the file being there but not executable, which is a
//! forgotten `chmod`; `128+n` is the kernel having killed it with signal
//! `n`, and `137` in particular is the out-of-memory killer often enough
//! to be worth naming. A notice that reads `make: killed (SIGKILL)` has
//! told you where to look. One that reads `exited 137` has told you to
//! go and search for what 137 means, which is the work this plugin
//! exists to save.
//!
//! The harder half is silence. A plugin that cries wolf gets uninstalled
//! within the day, so two large classes of non-zero exit are deliberately
//! not failures here:
//!
//!   * **You interrupted it.** `130` is `SIGINT`, which is Ctrl-C, and
//!     `131` is `SIGQUIT`, which is Ctrl-\. Being told that the thing you
//!     just stopped has stopped is worse than being told nothing, and it
//!     happens many times an hour.
//!   * **One means "no".** For `grep`, `rg`, `diff`, `cmp` and `test`,
//!     exit 1 is the answer to a question — no matches, no difference,
//!     not true — and not a fault. `grep TODO src/` finding nothing is a
//!     successful search. Those commands are allowlisted for status 1
//!     only: `grep` exiting 2 is a real error (a bad pattern, an
//!     unreadable file) and is still reported.
//!
//! A status of `-1` means the shell did not report one, which is not the
//! same as a failure and is also passed over.
//!
//! The plugin keeps nothing at all: an event is enough to decide on by
//! itself, so there is no state to lose on a reload and no `Remember` and
//! no `render`. That is the right shape for it — a watcher that also drew
//! a segment would be two plugins in one directory.

use haven_plugin_api::{Command, Event, Plugin};

haven_plugin_api::plugin!(FailWatch);

struct FailWatch;

/// Commands for which exit 1 is an answer rather than a fault.
const ONE_MEANS_NO: &[&str] =
    &["grep", "egrep", "fgrep", "rg", "ag", "ack", "diff", "cmp", "test", "[", "git-diff"];

impl Plugin for FailWatch {
    fn on_event(event: Event) -> Vec<Command> {
        let Event::CommandEnded(end) = event else { return Vec::new() };
        let name = program(&end.command);
        match why(end.status, &name) {
            Some(reason) => vec![Command::Notice(format!("{name}: {reason}"))],
            None => Vec::new(),
        }
    }
}

/// What to say about a status, or `None` to say nothing.
fn why(status: i32, name: &str) -> Option<String> {
    match status {
        // Success, and the shell not having told us, which is not a
        // failure and must not be reported as one.
        0 | i32::MIN..=-1 => None,
        // Ctrl-C and Ctrl-\: the user did this on purpose.
        130 | 131 => None,
        1 if ONE_MEANS_NO.contains(&name) => None,
        1 => Some("failed".into()),
        126 => Some("not executable".into()),
        127 => Some("not found".into()),
        // 128+n is a signal. Worth naming where the name tells you
        // something: a segfault is a bug, a SIGKILL on a build is very
        // often the out-of-memory killer.
        n if n > 128 && n < 160 => Some(match n - 128 {
            6 => "aborted (SIGABRT)".into(),
            9 => "killed (SIGKILL)".into(),
            11 => "crashed (SIGSEGV)".into(),
            13 => "broken pipe (SIGPIPE)".into(),
            14 => "timed out (SIGALRM)".into(),
            15 => "terminated (SIGTERM)".into(),
            signal => format!("killed by signal {signal}"),
        }),
        n => Some(format!("exited {n}")),
    }
}

/// The name of the program that failed, from whatever the shell reported.
///
/// Leading `VAR=value` assignments are skipped and a path is reduced to
/// its last component, so `/usr/bin/grep` and `LC_ALL=C grep` both come
/// out as `grep` — which is what the allowlist above has to match
/// against for the silence to work at all.
fn program(command: &str) -> String {
    for word in command.split_whitespace() {
        if word.contains('=') {
            continue;
        }
        return word.rsplit('/').next().unwrap_or(word).to_string();
    }
    "a command".into()
}
