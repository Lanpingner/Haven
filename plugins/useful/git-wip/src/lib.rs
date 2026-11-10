//! How much uncommitted work is sitting in the working tree, in lines.
//!
//! `+127-14`: a hundred and twenty-seven lines added, fourteen removed,
//! none of it committed. The purpose is narrow and worth stating,
//! because it is what decides every question below: to notice that a
//! day's work is uncommitted *before* the laptop sleeps, the branch is
//! switched, or `git checkout .` is typed in the wrong pane.
//!
//! A clean tree shows nothing. There is no point in a segment that says
//! `+0-0` all morning.
//!
//! # Staged changes count
//!
//! The single command is `git diff --shortstat HEAD`, which compares the
//! working tree to the last commit and therefore counts staged and
//! unstaged changes together. That is deliberate: "staged" is a step on
//! the way to a commit, not a commit, and work sitting in the index is
//! exactly as lost as work sitting in the file if the tree is thrown
//! away. Counting only unstaged changes would make the number *fall*
//! when you `git add`, which would be an alarm that switches itself off.
//!
//! In a repository with no commits yet there is no `HEAD` to compare
//! with, so the fallback compares the index with git's empty tree
//! object. A fresh `git init` with the day's work staged in it is
//! precisely the case this plugin exists for, and it would be a poor
//! joke to show nothing there.
//!
//! # The threshold
//!
//! Past 200 changed lines, added and removed together, the segment is
//! `bold`. The number is a guess, but a considered one: a couple of
//! hundred lines is about the point where the diff stops being one
//! change you are holding in your head and starts being a morning you
//! would be sorry to lose. Nothing lets a user configure it yet (see
//! `plugins/README.md`), so it is a default rather than a setting.
//!
//! # Why it caches
//!
//! `render` is called whenever Haven draws. Running `git` there would
//! be a process per frame, so `render` only formats a number that is
//! already known, and the one `git diff` runs from `on_event`: the
//! directory changed, a command finished — which is where `git commit`,
//! `git add` and `git stash` land — the focus moved, or the tick came
//! round to catch an edit made somewhere else. `--shortstat` is also
//! the cheap form on purpose: it makes git count lines without
//! formatting a single hunk, and the host kills any program after five
//! seconds.
//!
//! # Why `git -C`
//!
//! `host::run` has no working-directory argument; the program runs in
//! the daemon's cwd, which is wherever Haven was started. A plugin that
//! ran a bare `git diff` would report Haven's own repository in every
//! pane. So the directory comes from the focused pane's `cwd` (`OSC 7`)
//! and is passed as `git -C <cwd> …`. With no cwd there is nothing to
//! be right about, and the plugin says nothing.

use std::cell::RefCell;

use haven_plugin_api::{bold, focused, host, plain, Command, Event, Plugin, Run};

haven_plugin_api::plugin!(GitWip);

struct GitWip;

/// Past this many changed lines the segment is bold. See the module
/// docs for why this number and not another.
const SHOUT_AT: u32 = 200;

#[derive(Default)]
struct State {
    added: u32,
    removed: u32,
    /// A refusal from the host, shown rather than swallowed.
    refused: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

impl Plugin for GitWip {
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

    /// Pure formatting of the counts `refresh` already found.
    fn render() -> Vec<Run> {
        STATE.with_borrow(|state| {
            if let Some(why) = &state.refused {
                return vec![plain(why.clone())];
            }
            if state.added == 0 && state.removed == 0 {
                // A clean tree, or not a repository. Either way there is
                // no uncommitted work to warn about.
                return Vec::new();
            }
            let text = format!("+{}-{}", state.added, state.removed);
            if state.added + state.removed > SHOUT_AT {
                vec![bold(text)]
            } else {
                vec![plain(text)]
            }
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
    let cwd = cwd.as_str();

    // The working tree against the last commit, which is staged and
    // unstaged changes together.
    let mut stat = git(&["git", "-C", cwd, "diff", "--shortstat", "HEAD"])?;
    if stat.is_none() {
        // Either this is not a repository, or it has no commits yet. The
        // empty tree — git's own well-known hash for "nothing" — tells
        // the two apart: in a repository with no commits this gives the
        // staged work, and outside one it fails again and we show
        // nothing.
        stat = git(&[
            "git",
            "-C",
            cwd,
            "diff",
            "--shortstat",
            "--cached",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        ])?;
    }
    let Some(stat) = stat else { return Ok(State::default()) };

    let (added, removed) = shortstat(&stat);
    Ok(State { added, removed, refused: None })
}

/// The two numbers out of ` 3 files changed, 127 insertions(+), 14 deletions(-)`.
///
/// Parsed by hand and by position rather than with a regular
/// expression: a plugin is downloaded by whoever installs it, and this
/// is six lines against a dependency's worth of kilobytes. Either half
/// is absent when it is zero, so each number is found by the word that
/// follows it rather than by counting commas.
fn shortstat(line: &str) -> (u32, u32) {
    let mut added = 0;
    let mut removed = 0;
    let mut words = line.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let Ok(count) = word.parse::<u32>() else { continue };
        match words.peek() {
            Some(next) if next.starts_with("insertion") => added = count,
            Some(next) if next.starts_with("deletion") => removed = count,
            _ => {}
        }
    }
    (added, removed)
}

/// Run `git`, keeping the host's refusal distinct from git's own answer.
///
/// `output_of` folds both into `Err`, and here they mean opposite
/// things: a refusal has to be shown, while `git` exiting non-zero is
/// the ordinary answer "there is no such thing" — not a repository, no
/// commits — and has to be silent. So this calls `host::run` directly:
/// `Err` is the host refusing or failing to start the program,
/// `Ok(None)` is git saying no.
fn git(argv: &[&str]) -> Result<Option<String>, String> {
    let owned: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let out = host::run(&owned)?;
    if out.status != 0 {
        return Ok(None);
    }
    Ok(Some(out.stdout.trim_end().to_string()))
}
