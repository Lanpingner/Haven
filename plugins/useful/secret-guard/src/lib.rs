//! Says so when a credential has just been printed to the screen.
//!
//! This is the plugin that justifies the capability it asks for. A token
//! that appears in a terminal is in the scrollback, and the scrollback
//! is in the session, and the session is in whatever screenshot gets
//! pasted into a bug report an hour later. The moment it happened is the
//! only moment anybody can act — rotate the key, clear the buffer, think
//! twice about the screen share — and it is exactly the moment nobody
//! notices, because the token went past in the middle of two hundred
//! rows of a tool's debug output. So: a notice, immediately, saying what
//! kind of thing was seen and in which pane.
//!
//! # What it reads, and why it cannot do without it
//!
//! It reads the text of a pane — the last 200 rows, once, when a command
//! finishes. There is no way to do this job with less. The whole
//! question is "what characters are now on that screen", and the only
//! function in the plugin world that answers it is `pane-text`, which
//! needs `read-pane-content`. `panes()` would tell it the title and the
//! working directory, which cannot contain the answer.
//!
//! The capability is genuinely the dangerous one and this plugin is the
//! reason to grant it anyway: it is the one that looks at everything
//! scrolling past *in order to warn you about it*. Granting it is a
//! judgement that this plugin's handling of what it sees is trustworthy,
//! so that handling is as small as it can be made:
//!
//! * **The secret never appears in the output.** The notice names the
//!   kind — "an AWS access key id" — and the pane, and nothing else. A
//!   notice quoting the token would print it a second time, on the
//!   status bar, where it may be logged, screenshotted or copied, which
//!   would make the plugin the leak it is warning about.
//! * **Nothing is stored.** No `Command::Remember`, deliberately: that
//!   persists to disk, and a credential — or a fingerprint of one —
//!   written into a file that outlives the session is the opposite of
//!   this plugin's purpose. The only thing kept between events is the
//!   text of the last notice per pane, held in a `static` that dies with
//!   the plugin, so that the same find does not warn twice. That text is
//!   the kind and the pane number; it never contains the secret.
//! * **Nothing is sent anywhere.** There is no `net` capability, the
//!   host refuses one, and this plugin would be the worst possible place
//!   to want it.
//! * Nothing is logged either. `host::log` writes to Haven's log file,
//!   which is a file.
//!
//! # Why the scan is on an event and not in `render`
//!
//! `render` is called whenever Haven draws. Searching two hundred rows
//! for credential shapes on every frame would be the most expensive
//! thing in the status bar, for an answer that can only change when new
//! output arrives. It happens on `command-ended`: output has just
//! stopped arriving, which is both the cheapest and the most useful
//! moment, since that is when the thing that printed the token has
//! finished printing it. `render` draws nothing at all in the normal
//! case — a permanent "secrets!" segment would be a worse habit than no
//! segment — and exists only to report a missing permission, because a
//! guard that is silently not guarding is worse than no guard.
//!
//! # What it looks for, and what it refuses to guess at
//!
//! Only shapes that cannot plausibly be anything else. A false positive
//! a day gets the plugin uninstalled, and an uninstalled guard catches
//! nothing, so the bar for a rule is: would this fire on ordinary build
//! output, documentation, or a tool's `--help`?
//!
//! * `AKIA` followed by sixteen upper-case alphanumerics — an AWS access
//!   key id. The prefix is assigned and the length is fixed.
//! * `ghp_` or `github_pat_` followed by twenty or more token
//!   characters — a GitHub personal access token.
//! * `sk-` followed by at least thirty-two token characters, at least
//!   one of them a digit — an OpenAI-style API key. The length and the
//!   digit are the rules that matter: `sk-` on its own is two letters
//!   and a dash, `sk-test` is a placeholder everybody writes, and
//!   `sk-some-long-hyphenated-filename` is a filename. A real one is
//!   around fifty mixed characters.
//! * `-----BEGIN … PRIVATE KEY-----`, which has exactly one meaning.
//! * `Bearer ` followed by sixteen or more token characters, as it
//!   appears in an `Authorization` header that something echoed. Matched
//!   case-sensitively, because `Bearer` capitalised and followed by a
//!   long opaque string is a header, whereas "bearer" in a sentence is a
//!   sentence.
//! * `export SOMETHING_TOKEN=value`, where the name is
//!   `SCREAMING_SNAKE_CASE` ending in `_TOKEN`, `_SECRET`, `_KEY`,
//!   `_PASSWORD`, `_PASSWD`, `_CREDENTIALS` or `_API_KEY`, and the value
//!   is at least twelve characters, is not a `$REFERENCE` to another
//!   variable, and contains no `/` — the last two rules are what keeps
//!   `export SSH_KEY=~/.ssh/id_ed25519` and `export
//!   GITHUB_TOKEN=$GH_PAT`, which are paths and indirections rather than
//!   secrets, from firing.
//!
//! What it deliberately does not attempt:
//!
//! * **Entropy heuristics.** "This string looks random" is true of every
//!   git object id, every content hash, every UUID, every docker image
//!   digest and every line of minified JavaScript, and build output is
//!   made of those. A rule that fires on `cargo build` once is a rule
//!   nobody keeps enabled, so there is no entropy rule here at all.
//! * **Generic `password=`, `token=`, `secret=`.** These appear in
//!   `--help` text, in connection strings in documentation, in URLs in
//!   error messages and in the plugin's own source. Without a shape for
//!   the value there is nothing to distinguish `password=<your
//!   password>` in a usage message from the real thing, and the real
//!   thing in that form is usually also caught by one of the rules
//!   above.
//! * **JWTs** (`eyJ…`), even though the prefix is distinctive, because
//!   example tokens are printed in documentation, in test fixtures and
//!   in tutorials constantly, and an expired example JWT is not a
//!   credential.
//! * **Anything about what the secret is worth.** The plugin cannot know
//!   whether a key is live, revoked or made up for a test, so it says
//!   what shape it saw and leaves the judgement to the person reading.
//!
//! The consequence of being conservative is that it misses things: an
//! in-house API key with no prefix, a password typed at a prompt that
//! echoed, a `.env` file printed with `cat`. That is the intended
//! direction to be wrong in. This plugin is a smoke alarm, not an
//! auditor.

use std::cell::RefCell;

use haven_plugin_api::{host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(SecretGuard);

struct SecretGuard;

/// A screenful and a little more: what a command just printed, plus
/// enough above it that a token in the line before the prompt still
/// counts.
const ROWS: u32 = 200;

thread_local! {
    /// The last notice given, so an unchanged screen is not warned
    /// about again on the next command. The kind and the pane number
    /// only — never anything read from the pane.
    static LAST: RefCell<String> = const { RefCell::new(String::new()) };
    /// The host's refusal, so `render` can say the guard is not
    /// guarding.
    static REFUSED: RefCell<Option<String>> = const { RefCell::new(None) };
}

impl Plugin for SecretGuard {
    fn on_event(event: Event) -> Vec<Command> {
        let Event::CommandEnded(end) = event else { return Vec::new() };
        let text = match host::pane_text(end.pane, ROWS) {
            Ok(text) => text,
            Err(why) => {
                REFUSED.with(|cell| *cell.borrow_mut() = Some(why));
                return Vec::new();
            }
        };
        REFUSED.with(|cell| *cell.borrow_mut() = None);

        let kinds = kinds_in(&text);
        if kinds.is_empty() {
            LAST.with(|cell| cell.borrow_mut().clear());
            return Vec::new();
        }
        let notice =
            format!("{} in pane {} — it is in the scrollback now", listed(&kinds), end.pane);
        // The same find, again, on a screen that has not changed is not
        // news. Comparing the notice rather than the secret is the point:
        // there is nothing sensitive in this comparison.
        if LAST.with(|cell| *cell.borrow() == notice) {
            return Vec::new();
        }
        LAST.with(|cell| *cell.borrow_mut() = notice.clone());
        vec![Command::Notice(notice)]
    }

    fn render() -> Vec<Run> {
        // Nothing in the ordinary case. The only thing worth a permanent
        // segment is the plugin being unable to do its job at all.
        REFUSED.with(|cell| match cell.borrow().as_ref() {
            Some(why) => vec![plain(why.clone())],
            None => Vec::new(),
        })
    }
}

/// What kinds of credential are visible, named, in a stable order and
/// without repeats.
fn kinds_in(text: &str) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = Vec::new();
    for line in text.lines() {
        for kind in kinds_on(line) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

fn kinds_on(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    if tokens_after(line, "AKIA", |c| c.is_ascii_uppercase() || c.is_ascii_digit())
        .iter()
        .any(|token| token.chars().count() >= 16)
    {
        found.push("an AWS access key id");
    }
    let github =
        |prefix| tokens_after(line, prefix, word).iter().any(|token| token.chars().count() >= 20);
    if github("ghp_") || github("github_pat_") {
        found.push("a GitHub token");
    }
    if tokens_after(line, "sk-", |c| word(c) || c == '-').iter().any(|token| {
        // Length and a digit together: long hyphenated words are
        // filenames and branch names, and this is the one prefix short
        // enough to meet them by accident.
        token.chars().count() >= 32 && token.chars().any(|c| c.is_ascii_digit())
    }) {
        found.push("an API key");
    }
    if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
        found.push("a private key");
    }
    if tokens_after(line, "Bearer ", |c| word(c) || matches!(c, '-' | '.' | '+' | '/' | '='))
        .iter()
        .any(|token| token.chars().count() >= 16)
    {
        found.push("a bearer token");
    }
    if exported_secret(line) {
        found.push("a secret in an `export`");
    }
    found
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every run of token characters that follows `prefix` where `prefix`
/// begins a word.
///
/// The word-start rule is what stops `ghp_` matching inside a longer
/// identifier; what the caller does with the lengths is the rule that
/// stops a short placeholder being reported as a credential.
fn tokens_after<'a>(line: &'a str, prefix: &str, allowed: fn(char) -> bool) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = line[from..].find(prefix) {
        let at = from + at;
        let starts_a_word = match line[..at].chars().last() {
            None => true,
            Some(c) => !(word(c) || c == '-'),
        };
        let rest = &line[at + prefix.len()..];
        let end =
            rest.char_indices().find(|(_, c)| !allowed(*c)).map(|(i, _)| i).unwrap_or(rest.len());
        if starts_a_word {
            found.push(&rest[..end]);
        }
        from = at + prefix.len();
    }
    found
}

/// `export SOMETHING_TOKEN=value`, with a value that looks like one.
fn exported_secret(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("export ") else { return false };
    let Some((name, value)) = rest.split_once('=') else { return false };
    let name = name.trim();
    if name.is_empty()
        || !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    let named_like_a_secret = ["_TOKEN", "_SECRET", "_KEY", "_PASSWORD", "_PASSWD", "_CREDENTIALS"]
        .iter()
        .any(|suffix| name.ends_with(suffix));
    if !named_like_a_secret {
        return false;
    }
    let value = value.trim().trim_matches(['"', '\'']);
    // A reference to another variable is not the secret, and a path is a
    // path: `export SSH_KEY=~/.ssh/id_ed25519` is where a key lives,
    // not a key.
    value.chars().count() >= 12
        && !value.starts_with('$')
        && !value.contains('/')
        && !value.starts_with('~')
}

/// "a and b", "a, b and c" — so the notice reads as a sentence.
fn listed(kinds: &[&str]) -> String {
    match kinds {
        [] => String::new(),
        [one] => one.to_string(),
        [most @ .., last] => format!("{} and {last}", most.join(", ")),
    }
}
