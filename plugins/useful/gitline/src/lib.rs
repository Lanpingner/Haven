//! The branch, and what state it is in, on the status bar.
//!
//! This is the segment people put in their prompt and then want back
//! when the prompt is gone: which branch, whether there is anything
//! uncommitted, and whether the branch has drifted from its upstream.
//! `main`, `main*`, `feat/x* ↑3↓1`.
//!
//! # The symbols
//!
//! | Shown | Means |
//! |---|---|
//! | `main` | the branch, or the short sha when `HEAD` is detached |
//! | `*` | the tree is dirty: something tracked is modified, staged or untracked |
//! | `↑3` | three commits ahead of the upstream |
//! | `↓1` | one commit behind it |
//!
//! No arrows at all means either level with the upstream or no upstream
//! configured, which are both ordinary states and neither is an error.
//! Counts of *lines* are deliberately not here — that is `git-wip`'s
//! job, and two plugins both claiming `+2-1` would be unreadable.
//!
//! # Why it caches
//!
//! `render` is called every time Haven draws, which is every frame it
//! has a reason to redraw. Shelling out to `git` from there would mean
//! several processes per frame, so `render` only formats what is already
//! known: the four `git` calls happen in `on_event`, on the events that
//! can actually change the answer — the directory changed, a command
//! finished (that is where `git commit` lands), the focus moved to
//! another pane, or the tick came round to catch work done in some other
//! window. This is the whole design of the plugin; everything else is
//! string formatting.
//!
//! # Why `git -C`
//!
//! `host::run` has no working-directory argument: the program runs in
//! the daemon's cwd, which is wherever the user happened to start
//! Haven. A plugin that simply ran `git status` would therefore report
//! *Haven's own* repository in every pane, confidently and wrongly. So
//! the directory comes from the focused pane's `cwd` (`OSC 7`) and is
//! passed explicitly as `git -C <cwd> …`. When no pane reports a cwd
//! there is nothing to be right about, and the plugin shows nothing.
//!
//! Every command here is plumbing rather than porcelain, because the
//! host kills a program after five seconds and `git status` on a large
//! working tree can take longer than that: `rev-parse`, `symbolic-ref`,
//! `status --porcelain` and `rev-list --count` all do the least work
//! git can do to answer the question.

use std::cell::RefCell;
use std::fmt::Write as _;

use haven_plugin_api::{bold, dim, focused, host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(Gitline);

struct Gitline;

/// What the last refresh found. `head` empty means "say nothing": no
/// pane directory, or a directory that is not in a repository at all.
#[derive(Default)]
struct State {
    head: String,
    dirty: bool,
    ahead: u32,
    behind: u32,
    /// A refusal from the host, which is shown rather than swallowed: a
    /// segment that is blank because a permission is missing is a
    /// segment nobody can diagnose.
    refused: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

impl Plugin for Gitline {
    fn on_event(event: Event) -> Vec<Command> {
        // Exactly the events that can change the answer. Anything else
        // would be `git` processes spent on nothing.
        match event {
            Event::Started
            | Event::Tick
            | Event::PaneFocus(_)
            | Event::CwdChanged(_)
            | Event::CommandEnded(_) => refresh(),
            _ => {}
        }
        Vec::new()
    }

    /// Pure formatting of what `refresh` already worked out.
    fn render() -> Vec<Run> {
        STATE.with_borrow(|state| {
            if let Some(why) = &state.refused {
                return vec![plain(why.clone())];
            }
            if state.head.is_empty() {
                // Not a repository, or no directory to ask about.
                // Silence is the right answer: most directories are not
                // repositories and a status bar should not say so.
                return Vec::new();
            }
            let mut runs = vec![plain(state.head.clone())];
            if state.dirty {
                runs.push(bold("*"));
            }
            if state.ahead > 0 || state.behind > 0 {
                let mut arrows = String::from(" ");
                if state.ahead > 0 {
                    let _ = write!(arrows, "↑{}", state.ahead);
                }
                if state.behind > 0 {
                    let _ = write!(arrows, "↓{}", state.behind);
                }
                runs.push(dim(arrows));
            }
            runs
        })
    }
}

fn refresh() {
    let state = match look() {
        Ok(state) => state,
        Err(refused) => State { refused: Some(refused), ..State::default() },
    };
    STATE.set(state);
}

/// Ask `git` four cheap questions about the focused pane's directory.
fn look() -> Result<State, String> {
    let Some(cwd) = focused().map(|pane| pane.cwd).filter(|cwd| !cwd.is_empty()) else {
        return Ok(State::default());
    };
    let cwd = cwd.as_str();

    // Is this a repository at all? `rev-parse --git-dir` is the cheapest
    // question git answers, and a `None` here is the common case rather
    // than a problem.
    if git(&["git", "-C", cwd, "rev-parse", "--git-dir"])?.is_none() {
        return Ok(State::default());
    }

    // `symbolic-ref` rather than `rev-parse --abbrev-ref HEAD`, because
    // it still gives the branch in a repository with no commits yet,
    // where `rev-parse HEAD` fails outright. A fresh `git init` is a
    // real state and should read `main`, not an error.
    let head = match git(&["git", "-C", cwd, "symbolic-ref", "--quiet", "--short", "HEAD"])? {
        Some(branch) if !branch.is_empty() => branch,
        // No symbolic `HEAD`: detached, so show what a person can act
        // on, which is the short sha.
        _ => git(&["git", "-C", cwd, "rev-parse", "--short", "HEAD"])?.unwrap_or_default(),
    };

    // `--porcelain` is the stable, machine-readable form and skips all
    // the formatting and renaming work the human output does. It is the
    // one command here that touches every tracked file, and the reason
    // this is not run from `render`.
    let dirty = git(&["git", "-C", cwd, "status", "--porcelain"])?
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false);

    // One command for both directions. It fails when there is no
    // upstream and when there are no commits; both come back as `None`
    // and mean "no arrows", not "something is wrong".
    let (mut ahead, mut behind) = (0, 0);
    if let Some(counts) =
        git(&["git", "-C", cwd, "rev-list", "--count", "--left-right", "@{upstream}...HEAD"])?
    {
        // `left<TAB>right`: left is the upstream's own commits, so
        // behind; right is ours, so ahead.
        let mut fields = counts.split_whitespace();
        behind = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        ahead = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    }

    Ok(State { head, dirty, ahead, behind, refused: None })
}

/// Run `git`, keeping the host's refusal distinct from git's own answer.
///
/// `output_of` folds both into `Err`, and here they mean opposite
/// things: a refusal has to be shown, while `git` exiting non-zero is
/// usually the ordinary answer "no, there is no such thing" — not a
/// repository, no upstream, no commits — and has to be silent. So this
/// goes to `host::run` directly: `Err` is the host refusing or failing
/// to start the program, `Ok(None)` is git saying no.
fn git(argv: &[&str]) -> Result<Option<String>, String> {
    let owned: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let out = host::run(&owned)?;
    if out.status != 0 {
        return Ok(None);
    }
    Ok(Some(out.stdout.trim_end().to_string()))
}
