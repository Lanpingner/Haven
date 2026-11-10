//! A marker when the focused pane is somewhere else.
//!
//! `⇥ box` when the pane is running `ssh user@box.example.com`, a bare
//! `⇥` when something remote is running but the destination cannot be
//! read out of its argv. In `bold`, because the entire point is that it
//! is seen before the next command is typed: `rm -rf` in the wrong
//! machine is the mistake this exists to prevent, and a marker you have
//! to look for does not prevent it.
//!
//! That is also why it prefers a visible marker to a precise one. Every
//! branch below that cannot work out a host still returns the marker. A
//! segment that is sometimes vague is useful; one that silently gives up
//! on an argv it did not expect is a trap.
//!
//! # How it knows
//!
//! `pane.command`, which shell integration fills from `OSC 133`, and
//! `pane.title` only as a fallback. The title is a weaker signal: a
//! local shell commonly titles itself `romeo@arch: ~`, so an `@` in a
//! title is not evidence of anything, and this plugin looks for the name
//! of a launcher in it rather than for a shape that resembles one.
//!
//! The launchers are the ones that put a shell somewhere other than this
//! machine: `ssh`, `mosh`, `et`, `autossh`, and the container and cluster
//! ones — `kubectl exec`, `docker exec`, `podman exec`,
//! `distrobox enter`, `toolbox enter` — which need their second word
//! too, since `docker ps` is not a destination.
//!
//! A host is shortened to its first label (`box.example.com` → `box`)
//! unless every label is numeric, when it is kept whole, because `10` is
//! not a useful way to say `10.0.0.5`.
//!
//! The work is in `render`, from `host::panes()`, which needs no
//! capability.

use haven_plugin_api::{bold, focused, Plugin, Run};

haven_plugin_api::plugin!(Marker);

/// What the marker is, in both of its forms.
const MARKER: &str = "⇥";

/// Launchers that take a destination as their first non-option word.
const DIRECT: [&str; 4] = ["ssh", "mosh", "et", "autossh"];

/// Launchers that are only remote in one of their subcommands, paired
/// with the subcommand: `docker exec` is, `docker build` is not.
const SUB: [(&str, &str); 5] = [
    ("kubectl", "exec"),
    ("docker", "exec"),
    ("podman", "exec"),
    ("distrobox", "enter"),
    ("toolbox", "enter"),
];

/// Single-letter `ssh` options that swallow the word after them, so a
/// `-p 2222` is not mistaken for the destination.
const TAKES_VALUE: &str = "bcDEeFIiJLlmOopQRSWw";

/// How much of a host name to show.
const HOST: usize = 14;

struct Marker;

impl Plugin for Marker {
    fn render() -> Vec<Run> {
        let Some(pane) = focused() else { return Vec::new() };
        let found = remote(&pane.command).or_else(|| remote(&pane.title));
        match found {
            None => Vec::new(),
            Some(None) => vec![bold(MARKER)],
            Some(Some(where_to)) => vec![bold(format!("{MARKER} {where_to}"))],
        }
    }
}

/// `None` when this looks local. `Some(None)` when it is remote and the
/// destination is not in the words. `Some(Some(host))` when it is.
fn remote(text: &str) -> Option<Option<String>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let program = program_of(word);
        if DIRECT.contains(&program) {
            return Some(destination(&words[i + 1..]).map(host_of));
        }
        if let Some((_, sub)) = SUB.iter().find(|(name, _)| *name == program) {
            // The subcommand has to be there, and it may sit behind the
            // tool's own options: `kubectl -n prod exec pod`.
            let rest = &words[i + 1..];
            let at = rest.iter().position(|w| w == sub)?;
            return Some(destination(&rest[at + 1..]).map(shorten));
        }
    }
    None
}

/// A word as a program name: the last path component, so
/// `/usr/bin/ssh` counts and `rsync -e ssh` does not accidentally match
/// on something that merely contains the letters.
fn program_of(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The first word that is not an option or an option's value.
fn destination<'a>(words: &[&'a str]) -> Option<&'a str> {
    let mut skip = false;
    for word in words {
        if skip {
            skip = false;
            continue;
        }
        if let Some(flags) = word.strip_prefix('-') {
            // `-p 22` takes the next word; `-p22` and `-v` do not. The
            // value-taking letter is the last one in a cluster, which is
            // how `getopt` reads it too.
            if let Some(last) = flags.chars().last() {
                skip = TAKES_VALUE.contains(last) && flags.chars().count() == 1;
            }
            continue;
        }
        return Some(word);
    }
    None
}

/// `user@box.example.com` → `box`. The user is noise on a status bar,
/// and so is the domain when every machine shares it; an address made
/// only of numbers is left alone, since its first label says nothing.
fn host_of(destination: &str) -> String {
    let host = destination.rsplit('@').next().unwrap_or(destination);
    // `ssh://user@host:port` arrives occasionally.
    let host = host.trim_start_matches('/').split(':').next().unwrap_or(host);
    let numeric =
        host.split('.').all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_digit()));
    if numeric || !host.contains('.') {
        return shorten(host);
    }
    shorten(host.split('.').next().unwrap_or(host))
}

fn shorten(name: &str) -> String {
    if name.chars().count() <= HOST {
        return name.to_string();
    }
    let mut out: String = name.chars().take(HOST - 1).collect();
    out.push('…');
    out
}
