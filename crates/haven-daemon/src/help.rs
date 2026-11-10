//! What a command is, for the panel beside the history.
//!
//! Two rules decide everything here.
//!
//! **Nothing is run to describe it.** `whatis` reads an index and `man`
//! reads a file; neither executes the program being asked about. Running
//! `<command> --help` would cover far more — including every tool that
//! ships no man page — and it would also run a program that may not have
//! heard of `--help` and may do its job instead. A terminal that deletes
//! something because you moved a selection down one line is not a terminal
//! anybody should use.
//!
//! **It happens on a thread.** `man` decompresses a file and runs a
//! formatter; tens of milliseconds, sometimes more. The daemon's loop is
//! the thing that moves every pane's output, so a lookup inside it would
//! stop every session in order to describe `grep`.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

/// The word a help request is about: the program, not the command line.
///
/// `sudo` and friends are looked through, because `sudo pacman -Syu` is a
/// question about pacman. `VAR=x cmd` likewise: an assignment is not a
/// program.
pub fn subject(command: &str) -> String {
    const TRANSPARENT: &[&str] = &["sudo", "doas", "env", "command", "nohup", "time", "nice"];
    for word in command.split_whitespace() {
        if word.contains('=') || word.starts_with('-') {
            continue;
        }
        if TRANSPARENT.contains(&word) {
            continue;
        }
        // A path is named by its last component: `/usr/bin/git` is git.
        return word.rsplit('/').next().unwrap_or(word).to_string();
    }
    command.trim().to_string()
}

/// A help lookup in flight, and everything already looked up.
#[derive(Debug)]
pub struct Helper {
    tx: Sender<(String, String)>,
    rx: Receiver<(String, String)>,
    /// Asked for and not yet answered, so a selection moved up and back
    /// does not start the same lookup twice.
    pending: Vec<String>,
}

impl Default for Helper {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, pending: Vec::new() }
    }
}

impl Helper {
    /// Start a lookup, unless one is already running for this word.
    pub fn ask(&mut self, word: String) {
        if word.is_empty() || self.pending.contains(&word) {
            return;
        }
        self.pending.push(word.clone());
        let tx = self.tx.clone();
        // Detached: nothing waits on it, and a `man` that never returns
        // costs one thread rather than the session.
        std::thread::spawn(move || {
            let text = look_up(&word);
            let _ = tx.send((word, text));
        });
    }

    /// Lookups that have finished since the last call.
    pub fn drain(&mut self) -> Vec<(String, String)> {
        let done: Vec<(String, String)> = self.rx.try_iter().collect();
        for (word, _) in &done {
            self.pending.retain(|w| w != word);
        }
        done
    }
}

/// `whatis` first, then the opening of the man page.
fn look_up(word: &str) -> String {
    let mut out = String::new();
    if let Some(line) = whatis(word) {
        out.push_str(&line);
    }
    if let Some(body) = man_opening(word) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&body);
    }
    out
}

fn whatis(word: &str) -> Option<String> {
    let out = std::process::Command::new("whatis")
        .arg("--")
        .arg(word)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `git (1) - the stupid content tracker`, one line per section. The
    // first is the one a person means.
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The DESCRIPTION paragraph, or failing that the first prose in the page.
fn man_opening(word: &str) -> Option<String> {
    let out = std::process::Command::new("man")
        .arg("--")
        .arg(word)
        .env("LC_ALL", "C")
        // No pager, no backspace-overstrike bolding, a width we choose: the
        // panel is narrow and a man page formatted for 80 columns would be
        // re-wrapped badly rather than well.
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .env("MANWIDTH", "72")
        .env("GROFF_NO_SGR", "1")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(description(&text)).filter(|d| !d.is_empty())
}

/// Pull the DESCRIPTION out of a formatted man page.
///
/// Split out from the process call so the parsing can be tested against a
/// page checked into the test rather than against whatever this machine
/// happens to have installed.
pub fn description(page: &str) -> String {
    let mut lines = page.lines();
    // Headings in a formatted page are unindented and shouty.
    let mut taking = false;
    let mut out: Vec<String> = Vec::new();
    for line in lines.by_ref() {
        let heading = !line.starts_with(' ') && line.trim() == line.trim().to_uppercase();
        if heading && !line.trim().is_empty() {
            if taking {
                break;
            }
            taking = line.trim() == "DESCRIPTION";
            continue;
        }
        if taking {
            out.push(line.trim_end().to_string());
            // One paragraph is the right amount for a side panel: enough to
            // say what the thing is, not so much that it is a man page in a
            // box you cannot scroll.
            if out.len() > 2 && out.last().is_some_and(|l| l.trim().is_empty()) {
                break;
            }
        }
    }
    // Strip the common indent a man page puts on body text, and the blank
    // lines at either end.
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let indent = out
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    out.iter().map(|l| l.chars().skip(indent).collect::<String>()).collect::<Vec<_>>().join("\n")
}

/// What has been looked up, so the same word is never looked up twice.
pub type Cache = HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subject_is_the_program_not_the_command_line() {
        assert_eq!(subject("git status --short"), "git");
        assert_eq!(subject("/usr/bin/cargo test"), "cargo");
        // `sudo pacman -Syu` is a question about pacman.
        assert_eq!(subject("sudo pacman -Syu"), "pacman");
        assert_eq!(subject("env RUST_LOG=debug cargo run"), "cargo");
        assert_eq!(subject("RUST_LOG=debug cargo run"), "cargo");
        assert_eq!(subject("  ls  "), "ls");
    }

    #[test]
    fn the_description_is_lifted_out_of_a_formatted_page() {
        let page = "\
NAME
       grep - print lines that match patterns

SYNOPSIS
       grep [OPTION...] PATTERNS [FILE...]

DESCRIPTION
       grep searches for PATTERNS in each FILE.  PATTERNS is one or more
       patterns separated by newline characters.

       By default, grep prints the matching lines.

OPTIONS
       -E, --extended-regexp
";
        let got = description(page);
        assert!(got.starts_with("grep searches for PATTERNS"), "{got:?}");
        assert!(!got.contains("OPTIONS"), "it ran past the end of the section: {got:?}");
        assert!(!got.contains("SYNOPSIS"), "{got:?}");
        // The page's body indent is gone, so the panel can do its own.
        assert!(!got.lines().next().unwrap().starts_with(' '), "{got:?}");
    }

    #[test]
    fn a_page_with_no_description_gives_nothing_rather_than_the_whole_page() {
        let page = "NAME\n       x - a thing\n\nSYNOPSIS\n       x\n";
        assert_eq!(description(page), "");
    }

    #[test]
    fn a_lookup_in_flight_is_not_started_twice() {
        let mut helper = Helper::default();
        helper.ask("ls".into());
        helper.ask("ls".into());
        assert_eq!(helper.pending, vec!["ls".to_string()]);
        // And an empty word is not a lookup at all.
        helper.ask(String::new());
        assert_eq!(helper.pending.len(), 1);
    }

    #[test]
    fn a_finished_lookup_is_drained_once_and_stops_being_pending() {
        let mut helper = Helper::default();
        helper.ask("true".into());
        // Wait for the thread. This is the one test that touches the
        // machine's own man pages, and it asserts nothing about what they
        // say — only that an answer arrives, even an empty one, and that
        // the slot clears.
        //
        // Thirty seconds, because `man` is entitled to be slow the first
        // time it is asked anything: on a fresh macOS runner it may build
        // its index before answering, and ten seconds was not enough. The
        // test is about the plumbing, not about how fast `man` is.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut got = Vec::new();
        while got.is_empty() && std::time::Instant::now() < deadline {
            got = helper.drain();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(got.len(), 1, "the lookup never came back");
        assert_eq!(got[0].0, "true");
        assert!(helper.pending.is_empty());
        assert!(helper.drain().is_empty(), "the same answer came back twice");
    }
}
