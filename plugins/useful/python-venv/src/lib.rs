//! Which Python virtual environment the focused pane is probably in.
//!
//! `venv:haven` when the pane's working directory is inside
//! `~/Projects/haven/.venv/...`, and nothing at all when there is no
//! sign of one — a status segment that is always present but usually
//! empty is clutter, and an empty one costs the bar nothing.
//!
//! # Why this is a guess
//!
//! The honest way to answer this is `$VIRTUAL_ENV`, and a plugin cannot
//! read it: there is no environment in the sandbox, and the variable
//! belongs to the shell in the pane rather than to Haven anyway. So this
//! works from the one fact it does have, the `cwd` that `OSC 7` reports,
//! and infers from its shape. That means:
//!
//!   * It is right when you `cd` into the environment or into a tree
//!     whose path names it, which is the common case for
//!     `.venv/lib/python3.12/site-packages` spelunking and for
//!     `~/.virtualenvs/<name>`.
//!   * It says nothing when you activate an environment and then work in
//!     a directory that does not mention it — which is the *normal* way
//!     to use one. A missing segment here does not mean no environment
//!     is active.
//!   * It never says anything false about a different environment, which
//!     is the failure that would matter: the worst it does is stay quiet.
//!
//! When the host grows a way to ask a pane for an environment variable,
//! or this plugin is granted `exec`, it should read `$VIRTUAL_ENV` and
//! use the path only as a fallback. Until then the limitation is the
//! design, not a bug, and the README says so too.
//!
//! All the work is in `render`, from `host::panes()`, which needs no
//! capability. Nothing is cached in `on_event` because the facts come
//! from the host each time it draws and a cache could only be staler.

use haven_plugin_api::{dim, focused, plain, Plugin, Run};

haven_plugin_api::plugin!(Venv);

/// Path components that mean "a virtual environment lives here", and
/// whose *parent* is the project it belongs to. `env` is deliberately
/// absent: far too many directories are called that for innocent
/// reasons.
const HERE: [&str; 4] = [".venv", "venv", "virtualenv", ".conda"];

/// Components under which environments are kept side by side, so the
/// name is the component *after* the marker rather than before it:
/// `~/.virtualenvs/haven`, `~/.conda/envs/haven`.
const INSIDE: [&str; 3] = [".virtualenvs", "virtualenvs", "envs"];

/// How much of a name to show. A long one is truncated rather than
/// allowed to push the rest of the bar off the screen.
const NAME: usize = 16;

struct Venv;

impl Plugin for Venv {
    fn render() -> Vec<Run> {
        let Some(pane) = focused() else { return Vec::new() };
        let Some(name) = guess(&pane.cwd) else { return Vec::new() };
        vec![dim("venv:"), plain(name)]
    }
}

/// The environment the path implies, if it implies one.
fn guess(cwd: &str) -> Option<String> {
    let parts: Vec<&str> = cwd.split('/').filter(|p| !p.is_empty()).collect();

    // The last marker, not the first: a path can hold several, and the
    // innermost is the one being worked in.
    let at = parts.iter().rposition(|p| HERE.contains(p) || INSIDE.contains(p))?;
    let marker = parts[at];

    let name = if INSIDE.contains(&marker) {
        // `~/.virtualenvs/haven/lib` — the child names it. Without a
        // child the path names nothing yet, so say nothing.
        *parts.get(at + 1)?
    } else {
        // `~/Projects/haven/.venv` — the parent is the project. A marker
        // with no parent, as in a bare `/venv`, leaves only the marker,
        // and `venv:venv` tells nobody anything.
        match at.checked_sub(1).and_then(|i| parts.get(i)) {
            Some(parent) => parent,
            None => return None,
        }
    };

    // A marker in the name position is not a name: `.venv/venv` and
    // `envs/.conda` are both somebody's unusual layout, not a project.
    if name.is_empty() || HERE.contains(&name) || INSIDE.contains(&name) {
        return None;
    }
    Some(shorten(name))
}

fn shorten(name: &str) -> String {
    if name.chars().count() <= NAME {
        return name.to_string();
    }
    let mut out: String = name.chars().take(NAME - 1).collect();
    out.push('…');
    out
}
