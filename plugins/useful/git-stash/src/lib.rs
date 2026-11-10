//! How many stashes are hiding, shown only when there are any.
//!
//! `⚑2`. A stash you remember is a tool; a stash you have forgotten is
//! a bug you will spend an afternoon on in three weeks' time, and the
//! only reason it is forgotten is that nothing ever mentions it. So this
//! mentions it, and says nothing at all when the stash is empty — which
//! is most of the time, and is the state that needs no words.
//!
//! # Count, not age
//!
//! Showing the age of the oldest stash was the tempting alternative:
//! `⚑3w` says more about whether you have forgotten it than `⚑1` does.
//! It is not what this shows, for two reasons. The number is the thing
//! that changes behaviour — one stash is a thing you are in the middle
//! of, four is a drawer nobody has opened — whereas an age tells you
//! something is old without telling you how much is in there. And the
//! age is dearer: it needs `git log -g` with a format, a clock, and the
//! plugin doing date arithmetic by hand, against one `rev-list --count`
//! that git answers from the reflog alone. The count is the cheaper
//! signal and the more actionable one.
//!
//! # Why `rev-list` and not `git stash list`
//!
//! `git stash list` is the obvious command and the wrong one: it is
//! porcelain, so it walks each entry and formats a line for it, only
//! for a plugin to throw the lines away and count them. The stash is a
//! reflog, and its length is
//!
//! ```text
//! git rev-list --count --walk-reflogs refs/stash
//! ```
//!
//! which reads the reflog file and counts. When there has never been a
//! stash, `refs/stash` does not exist and the command fails — which is
//! the answer "none", not an error, and the plugin shows nothing. This
//! matters more than it looks: the host kills a program after five
//! seconds, so every command in a plugin should be plumbing.
//!
//! # Why it caches
//!
//! `render` is called whenever Haven draws, and one `git` process per
//! frame would be indefensible. The count is taken in `on_event` — the
//! directory changed, a command finished (which is where `git stash`
//! itself lands), the focus moved, or the tick came round — and
//! `render` only formats the number it already has.
//!
//! # Why `git -C`
//!
//! `host::run` has no working-directory argument: the program runs in
//! the daemon's cwd, wherever the user started Haven. A plugin that ran
//! a bare `git rev-list` would be reporting Haven's own stash in every
//! pane, which is worse than useless. So the directory comes from the
//! focused pane's `cwd` (`OSC 7`) and is passed explicitly as
//! `git -C <cwd> …`. With no cwd known, the plugin shows nothing.

use std::cell::RefCell;

use haven_plugin_api::{focused, host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(GitStash);

struct GitStash;

#[derive(Default)]
struct State {
    stashes: u32,
    /// A refusal from the host, shown rather than swallowed.
    refused: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

impl Plugin for GitStash {
    fn on_event(event: Event) -> Vec<Command> {
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

    /// Pure formatting of the number `refresh` already found.
    fn render() -> Vec<Run> {
        STATE.with_borrow(|state| {
            if let Some(why) = &state.refused {
                return vec![plain(why.clone())];
            }
            if state.stashes == 0 {
                // Nothing stashed, or not a repository. Either way the
                // status bar has better things to say.
                return Vec::new();
            }
            // A flag, because a stash is something put aside and marked
            // to come back to. Theme colours, not a hard-coded yellow.
            vec![plain(format!("⚑{}", state.stashes))]
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

fn look() -> Result<State, String> {
    let Some(cwd) = focused().map(|pane| pane.cwd).filter(|cwd| !cwd.is_empty()) else {
        return Ok(State::default());
    };
    // One command, and it answers "not a repository", "never stashed"
    // and "three stashed" all three: the first two fail, and failing is
    // how the plugin learns to stay quiet.
    let counted =
        git(&["git", "-C", cwd.as_str(), "rev-list", "--count", "--walk-reflogs", "refs/stash"])?;
    let stashes = counted.and_then(|out| out.trim().parse().ok()).unwrap_or(0);
    Ok(State { stashes, refused: None })
}

/// Run `git`, keeping the host's refusal distinct from git's own answer.
///
/// `output_of` folds both into `Err`, and here they mean opposite
/// things: a refusal has to be shown, while `git` exiting non-zero is
/// the ordinary answer "there is no such thing" — no repository, no
/// `refs/stash` — and has to be silent. So this calls `host::run`
/// directly: `Err` is the host refusing or failing to start the
/// program, `Ok(None)` is git saying no.
fn git(argv: &[&str]) -> Result<Option<String>, String> {
    let owned: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let out = host::run(&owned)?;
    if out.status != 0 {
        return Ok(None);
    }
    Ok(Some(out.stdout.trim_end().to_string()))
}
