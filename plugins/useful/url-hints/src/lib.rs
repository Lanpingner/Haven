//! How many links are on the screen, and the last one of them.
//!
//! A terminal prints URLs constantly — a crate's documentation, a pull
//! request, a CI run, a "see <https://…> for details" from some tool's
//! error path — and then scrolls them away. The question this answers is
//! the small one people solve by scrolling back and squinting: is there
//! a link up there at all, and what was the most recent one. A segment
//! reading `3 links  docs.rs/…` says yes and roughly where to look,
//! which is enough to decide whether to go and find it.
//!
//! # What it reads, and why it cannot do without it
//!
//! It reads the text of the focused pane: the last 200 rows, which is a
//! screenful and a little scrollback. A URL is a piece of a pane's
//! *contents*, so there is no weaker permission that would do. `panes()`
//! gives titles and working directories and no characters of output, and
//! nothing else in the world a plugin inhabits can see a line that was
//! printed. Counting links therefore requires `read-pane-content`, and
//! `read-pane-content` is all of the output, including the command lines
//! people type and any token that happened to be echoed. That is the
//! honest trade and it is in the README in those words.
//!
//! What it does with what it read is as narrow as the plugin can make
//! it. Nothing is sent anywhere — there is no `net` capability and the
//! host refuses one — and **nothing is written down**. In particular the
//! links are deliberately *not* kept with `Command::Remember`, which
//! persists to disk: a remembered URL is a line of somebody's screen
//! copied into a file that outlives the session, and a URL can carry a
//! session token in its query string. The count and the latest link live
//! in a `static` for as long as the plugin is loaded and go away with
//! it.
//!
//! # Why the scan is on an event and not in `render`
//!
//! `render` is called whenever Haven draws, which is whenever anything
//! on the screen moves. Scanning two hundred rows for URLs there would
//! mean doing it tens of times a second for an answer that changes only
//! when new output arrives, and the host would be copying the pane's
//! text across the component boundary each time. So the scan happens on
//! `command-ended`, where the output has just finished arriving and the
//! answer has just become stale, and on `pane-focus`, because a count
//! for the pane you are no longer looking at is worse than no count.
//! `render` formats two numbers that are already known.
//!
//! # The scanning rules
//!
//! There is no regex crate here and a regex would be the wrong tool
//! anyway, because the hard part is not matching a URL, it is deciding
//! where one stops in prose. The rules, in the order they apply:
//!
//! * A link starts at `http://`, `https://` or a bare `www.`, and only
//!   where the character before it is not part of a word — so the
//!   `https://` inside a longer URL's path is part of that URL and not a
//!   second one.
//! * It runs to the first character a URL cannot contain: whitespace, or
//!   one of `<>"'`\|^{}`, which is what wraps a URL when a tool wraps
//!   one.
//! * Then trailing punctuation comes off: `.,;:!?*_~'"` are sentence,
//!   not URL. A closing bracket comes off only when it is unbalanced, so
//!   `(https://example.com/a_(b))` keeps its inner brackets and loses
//!   the outer one.
//! * `www.` with no scheme is accepted, because tools print it and
//!   people read it as a link, but it needs two dots (`www.foo.org`, not
//!   `www.foo`) to stand apart from a sentence that happens to begin a
//!   word with `www.`. It is reported exactly as printed; the plugin
//!   does not invent a scheme it did not see.
//! * What is left must still look like a host: a dotted name ending in
//!   letters, or `localhost`, or a dotted quad. `http://` on its own, or
//!   `www.`, is not a link.
//! * The same URL ten times is one link. Duplicates are the normal case
//!   — a build prints the same documentation URL once per warning — and
//!   `47 links` for one link is a lie. The latest is the last *distinct*
//!   one in the rows, oldest first.
//!
//! **A URL wrapped across two rows is deliberately not reassembled.** A
//! terminal hard-wraps at the pane width without leaving a mark, and
//! `pane-text` hands back the wrapped rows with no flag to say which of
//! them were one line: a row ending in a URL-ish character followed by a
//! row beginning with one is exactly as likely to be two separate pieces
//! of output as one broken link. Joining them would manufacture URLs
//! that were never printed, and a wrong URL is worse than a missing one,
//! because the only thing anybody does with one of these is paste it
//! into a browser. So a wrapped link is counted up to the wrap and may
//! be shown short. If the WIT world ever says which rows continue a
//! logical line, this is the first plugin that would use it.

use std::cell::RefCell;

use haven_plugin_api::{dim, focused, host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(UrlHints);

struct UrlHints;

/// A screenful and a little scrollback. Enough that a link printed just
/// before the last command still counts, small enough that the scan is
/// not a visible cost on a slow machine.
const ROWS: u32 = 200;

thread_local! {
    /// What the last scan found. A `static`, not the store: see the note
    /// about not writing anybody's screen to disk at the top of this
    /// file.
    static FOUND: RefCell<Found> = RefCell::new(Found::default());
}

#[derive(Default)]
struct Found {
    links: usize,
    latest: String,
    /// The host's refusal, kept so `render` can say why it is empty.
    refused: Option<String>,
}

impl Plugin for UrlHints {
    fn on_event(event: Event) -> Vec<Command> {
        match event {
            // The three moments the answer changes: output arrived, or
            // the question is now about a different pane.
            Event::Started | Event::CommandEnded(_) | Event::PaneFocus(_) => scan(),
            _ => {}
        }
        // Nothing to ask of Haven. This plugin only ever draws.
        Vec::new()
    }

    fn render() -> Vec<Run> {
        FOUND.with(|cell| {
            let found = cell.borrow();
            // A plugin that silently draws nothing because of a missing
            // permission is one nobody can diagnose.
            if let Some(why) = &found.refused {
                return vec![plain(why.clone())];
            }
            if found.links == 0 {
                return Vec::new();
            }
            let plural = if found.links == 1 { "link" } else { "links" };
            let mut runs = vec![plain(format!("{} {plural}", found.links))];
            let short = shortened(&found.latest);
            if !short.is_empty() {
                runs.push(dim(format!(" {short}")));
            }
            runs
        })
    }
}

/// Look at the focused pane and remember what is there.
fn scan() {
    let Some(pane) = focused() else { return };
    let text = match host::pane_text(pane.id, ROWS) {
        Ok(text) => text,
        Err(why) => {
            FOUND
                .with(|cell| *cell.borrow_mut() = Found { refused: Some(why), ..Found::default() });
            return;
        }
    };
    let urls = urls_in(&text);
    FOUND.with(|cell| {
        *cell.borrow_mut() = Found {
            links: urls.len(),
            latest: urls.last().cloned().unwrap_or_default(),
            refused: None,
        }
    });
}

/// Every distinct URL in some rows of output, in the order they appear.
fn urls_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut at = 0;
        while at < line.len() {
            if !line.is_char_boundary(at) {
                at += 1;
                continue;
            }
            let rest = &line[at..];
            let scheme = starts_with_ci(rest, "https://")
                || starts_with_ci(rest, "http://")
                || starts_with_ci(rest, "ftp://");
            let bare = starts_with_ci(rest, "www.");
            if (scheme || bare) && starts_a_word(line, at) {
                let url = trimmed(span(rest));
                if plausible(url, bare && !scheme) {
                    if !found.iter().any(|seen| seen == url) {
                        found.push(url.to_string());
                    }
                    at += url.len();
                    continue;
                }
            }
            at += 1;
        }
    }
    found
}

/// Whether a link may begin here, which it may not in the middle of a
/// word or inside another URL's path.
fn starts_a_word(line: &str, at: usize) -> bool {
    match line[..at].chars().last() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | '@' | '%')),
    }
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// How far a URL runs before it hits something a URL cannot contain.
fn span(rest: &str) -> &str {
    let end =
        rest.char_indices().find(|(_, c)| !is_url_char(*c)).map(|(i, _)| i).unwrap_or(rest.len());
    &rest[..end]
}

/// What a URL may be made of, as it sits in terminal output. Printable
/// ASCII, less the characters tools put *around* a URL rather than in
/// one. Non-ASCII ends the span: an internationalised domain in a pane
/// is rare and the bytes are too ambiguous to guess at.
fn is_url_char(c: char) -> bool {
    c.is_ascii_graphic()
        && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '|' | '\\' | '^' | '{' | '}')
}

/// Take the sentence's punctuation back off the end of the URL.
fn trimmed(mut url: &str) -> &str {
    loop {
        let Some(last) = url.chars().last() else { return url };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' | '~' => true,
            // A bracket that was never opened inside the URL was closing
            // something the URL was written inside.
            ')' => count(url, ')') > count(url, '('),
            ']' => count(url, ']') > count(url, '['),
            _ => false,
        };
        if !drop {
            return url;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
}

fn count(text: &str, c: char) -> usize {
    text.chars().filter(|x| *x == c).count()
}

/// Whether what is left still looks like something a browser could open.
///
/// `bare` is a `www.` with no scheme, which is held to the stricter rule
/// — two dots — because it has no `://` to vouch for it.
fn plausible(url: &str, bare: bool) -> bool {
    let after = match url.find("://") {
        Some(at) => &url[at + 3..],
        None => url,
    };
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    // Strip any `user:pass@`, then the port.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let name = host.split(':').next().unwrap_or(host);
    if name.is_empty() {
        return false;
    }
    if bare && count(name, '.') < 2 {
        return false;
    }
    if name == "localhost" {
        return true;
    }
    // A dotted quad, for the ports people serve things on locally.
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() == 4
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // Otherwise a hostname: at least two labels, the last of them
    // letters, which is what keeps `version.1.2` from being a link.
    match name.rsplit_once('.') {
        Some((left, tld)) => {
            !left.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// The latest link, short enough for a status bar: the host, and a mark
/// to say there was a path after it. A full URL would take the whole bar
/// and the bar is shared with the tabs, the session name and the clock.
fn shortened(url: &str) -> String {
    let rest = match url.find("://") {
        Some(at) => &url[at + 3..],
        None => url,
    };
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut out: String = host.chars().take(22).collect();
    if host.chars().count() > 22 {
        out.push('…');
    } else if !path.is_empty() {
        out.push_str("/…");
    }
    out
}
