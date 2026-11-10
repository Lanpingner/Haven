//! The focused pane's working directory, short enough for a status bar.
//!
//! `/home/romeo/Projects/HavenTerminal/crates/haven-client` becomes
//! `~/P/H/c/haven-client`: every component but the last cut to its first
//! character, the last one left whole. The leaf is what a person is
//! actually in, and the ancestors only have to be enough to recognise a
//! path you already know — which one initial per component is, and which
//! is why fish and zsh have both shipped this for years.
//!
//! The rules, all of them, because a path shortener that surprises you is
//! worse than none:
//!
//!   * A path under a home directory starts `~`. There is no `$HOME` to
//!     read — a plugin has no environment — so home is recognised by
//!     shape: `/home/<user>`, `/Users/<user>`, `/var/home/<user>` (the
//!     ostree layout) and `/root`. A system with homes somewhere else
//!     gets the plain abbreviation, which is correct, just longer.
//!   * `/` stays `/`, and a path with one component stays whole: `/tmp`
//!     is already as short as it goes.
//!   * A component beginning with `.` keeps two characters, so `.config`
//!     is `.c` rather than a lone dot that says nothing.
//!   * A final component longer than `LEAF` is cut with an ellipsis. The
//!     point of this plugin is a bounded width, and one directory named
//!     after a Java package should not be allowed to take the bar.
//!   * An empty `cwd` — no `OSC 7`, so Haven does not know — draws
//!     nothing. A segment that says `?` when the shell has no shell
//!     integration is a segment that is wrong all day.
//!
//! The ancestors are drawn `dim` and the leaf `plain`, so the eye lands
//! on the part that changes.
//!
//! All of this happens in `render`, from `host::panes()`, which needs no
//! capability and is a fact about now. There is nothing worth caching in
//! `on_event`: the cache could only ever be a copy of something the host
//! will hand over anyway, with one more way to be stale.

use haven_plugin_api::{dim, focused, plain, Plugin, Run};

haven_plugin_api::plugin!(Crumbs);

/// How many characters of the final component to keep.
const LEAF: usize = 20;

struct Crumbs;

impl Plugin for Crumbs {
    fn render() -> Vec<Run> {
        let Some(pane) = focused() else { return Vec::new() };
        if pane.cwd.trim().is_empty() {
            return Vec::new();
        }
        let (ancestors, leaf) = crumbs(pane.cwd.trim());
        let mut runs = Vec::with_capacity(2);
        if !ancestors.is_empty() {
            runs.push(dim(ancestors));
        }
        if !leaf.is_empty() {
            runs.push(plain(leaf));
        }
        runs
    }
}

/// The abbreviated ancestors, including the separator that joins them to
/// the leaf, and the leaf itself. Two pieces because they are drawn in
/// two styles.
fn crumbs(cwd: &str) -> (String, String) {
    let (prefix, rest) = strip_home(cwd);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();

    let Some((leaf, ancestors)) = parts.split_last() else {
        // Nothing but the prefix: `/` itself, or home exactly.
        return (String::new(), if prefix.is_empty() { "/".into() } else { prefix.into() });
    };

    let mut head = String::from(prefix);
    if prefix.is_empty() && rest.starts_with('/') {
        head.push('/');
    }
    // A separator before every component except the first, and `~` or
    // `/` already counts as one — so a relative `cwd`, which `OSC 7`
    // should never send but might, does not grow a leading slash.
    let sep = |head: &mut String| {
        if !head.is_empty() && !head.ends_with('/') {
            head.push('/');
        }
    };
    for part in ancestors {
        sep(&mut head);
        head.push_str(&initial(part));
    }
    sep(&mut head);
    (head, shorten(leaf))
}

/// `~` and what follows it, when the path looks like a home directory;
/// otherwise the path as it came, keeping its leading `/`.
fn strip_home(cwd: &str) -> (&'static str, &str) {
    if cwd == "/root" || cwd.starts_with("/root/") {
        return ("~", &cwd["/root".len()..]);
    }
    for root in ["/home/", "/Users/", "/var/home/"] {
        if let Some(under) = cwd.strip_prefix(root) {
            // Exactly one component for the user, then it is their home.
            let user_len = under.find('/').unwrap_or(under.len());
            if user_len > 0 {
                return ("~", &under[user_len..]);
            }
        }
    }
    ("", cwd)
}

/// One component, cut down: its first character, or its first two when it
/// starts with a dot, since `.` alone distinguishes nothing.
fn initial(part: &str) -> String {
    let keep = if part.starts_with('.') { 2 } else { 1 };
    part.chars().take(keep).collect()
}

/// The leaf, cut to [`LEAF`] characters with an ellipsis if it has to be.
fn shorten(leaf: &str) -> String {
    if leaf.chars().count() <= LEAF {
        return leaf.to_string();
    }
    let mut out: String = leaf.chars().take(LEAF - 1).collect();
    out.push('…');
    out
}
