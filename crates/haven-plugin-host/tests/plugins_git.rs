//! The three `git` plugins, run against a real `git` and a real
//! repository.
//!
//! These are the first plugins that use `exec` in earnest, so there are
//! two things worth testing and both are here. Without the grant, the
//! plugin has to *say* it was refused and name the program, because a
//! status segment that is blank because of a missing permission is one
//! nobody can diagnose. With the grant, the plugin has to describe the
//! **pane's** repository: `host::run` has no working-directory
//! argument, so a plugin that forgot `git -C <cwd>` would report the
//! daemon's own repository in every pane, confidently and wrongly.
//! Haven's own checkout — which is where the test process runs — is a
//! repository with commits and a clean tree, so a plugin with that bug
//! would quietly pass a lazily written test. Every test here builds a
//! throwaway repository in a temporary directory and asserts something
//! that is only true of *that* repository.
//!
//! Two reasons a test here returns early rather than failing: the
//! `wasm32-wasip2` target may not be installed, which is the
//! `let Some(mut p) = … else { return };` on the first line of each
//! test, and `git` itself may not be installed, which `Repo::new`
//! checks. CI has both, which is where it has to hold.
//!
//! On the names: cargo's library target for `git-wip` is `git_wip`, so
//! the built component is `git_wip.wasm` and that is the name the
//! loader asks for. The plugin is `git-wip` everywhere a person sees
//! it.

use std::path::{Path, PathBuf};
use std::process::Command;

use haven_plugin_host::testing::{plain_load, with_exec, Session};
use haven_plugin_host::{text_of, CwdChange, Event, Loaded, Run};

/// A throwaway git repository, removed when the test ends.
struct Repo {
    path: PathBuf,
}

impl Repo {
    /// A fresh repository with no commits, or `None` when there is no
    /// `git` on this machine to make one with.
    fn new(label: &str) -> Option<Self> {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping {label}: no git on this machine");
            return None;
        }
        let path =
            std::env::temp_dir().join(format!("haven-git-plugin-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("make a temporary directory");
        let repo = Self { path };
        // `-b main` so the branch name is the one asserted on, whatever
        // this machine's `init.defaultBranch` happens to be.
        repo.git(&["init", "--quiet", "-b", "main"]);
        Some(repo)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cwd(&self) -> String {
        self.path.to_str().expect("a utf-8 temporary path").to_string()
    }

    /// Run git in the repository, with an identity supplied on the
    /// command line, so the test does not depend on the configuration
    /// of whoever is running it.
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(&self.path)
            .args(["-c", "user.email=test@haven.invalid", "-c", "user.name=Haven Test"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path.join(name), text).expect("write a file in the repository");
    }

    /// `n` numbered lines, for the plugin that counts them: going from
    /// `n` to `n + 5` lines is five insertions and nothing else, which
    /// makes the assertions arithmetic rather than guesswork.
    fn lines(&self, name: &str, n: u32) {
        let text: String = (0..n).map(|i| format!("line {i}\n")).collect();
        self.write(name, &text);
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A session whose focused pane is in `cwd`, which is the only thing
/// these three plugins ask the host for.
fn session_in(cwd: &str) -> Session {
    Session::new().pane(1, "fish", cwd).pane(2, "vim", "/tmp")
}

/// Deliver the event that makes a plugin refresh, then draw.
///
/// `CwdChanged` is the event a person actually causes by `cd`-ing into
/// a repository, so it is the one the tests use.
fn refreshed(plugin: &mut Loaded, cwd: &str) -> Vec<Run> {
    plugin
        .on_event(&Event::CwdChanged(CwdChange { pane: 1, cwd: cwd.to_string() }))
        .expect("the refresh event");
    plugin.render().expect("render")
}

fn drawn(plugin: &mut Loaded, cwd: &str) -> String {
    text_of(&refreshed(plugin, cwd))
}

/// Whether the run holding `needle` is bold, which is how these plugins
/// say "look at this".
fn bold_around(runs: &[Run], needle: &str) -> bool {
    runs.iter().any(|run| run.text.contains(needle) && run.style.bold)
}

#[test]
fn a_git_plugin_without_the_grant_says_which_program_it_wanted() {
    // All three, because all three have to fail the same way: the
    // refusal goes on the bar rather than into an empty segment. No
    // repository is needed — the host refuses before anything runs.
    let Some(mut p) = plain_load("gitline", session_in("/tmp")) else { return };
    let shown = drawn(&mut p, "/tmp");
    assert!(shown.contains("not granted"), "gitline should report the refusal: {shown:?}");
    assert!(shown.contains("git"), "gitline should name the program: {shown:?}");
    // And the refusal survives a second draw: `render` shows what the
    // last refresh found, and that is what it found.
    assert!(text_of(&p.render().expect("render")).contains("not granted"));

    for name in ["git_wip", "git_stash"] {
        let mut p = plain_load(name, session_in("/tmp")).expect("built");
        let shown = drawn(&mut p, "/tmp");
        assert!(shown.contains("not granted"), "{name} should report the refusal: {shown:?}");
        assert!(shown.contains("git"), "{name} should name the program: {shown:?}");
    }
}

#[test]
fn gitline_shows_the_branch_and_whether_the_tree_is_dirty() {
    // With no panes there is no directory to ask about, and a plugin
    // that cannot know says nothing.
    let Some(mut p) = with_exec("gitline", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "", "no pane, no answer");

    let Some(repo) = Repo::new("gitline-branch") else { return };
    let cwd = repo.cwd();
    let mut p = with_exec("gitline", &["git"], session_in(&cwd)).expect("built");

    // A repository with no commits at all. `git rev-parse HEAD` fails
    // here, which is why the plugin asks `symbolic-ref` instead: a
    // fresh `git init` reads `main` rather than an error.
    assert_eq!(drawn(&mut p, &cwd), "main", "an empty new repository is still on a branch");

    // Untracked work makes it dirty, and the mark is bold because that
    // is the part people look for.
    repo.write("a.txt", "one\n");
    let runs = refreshed(&mut p, &cwd);
    assert_eq!(text_of(&runs), "main*", "{:?}", text_of(&runs));
    assert!(bold_around(&runs, "*"), "the dirty mark should catch the eye: {runs:?}");

    // Committed, and it is clean again.
    repo.commit("one");
    assert_eq!(drawn(&mut p, &cwd), "main", "a committed tree is not dirty");

    // On a branch of its own, by name.
    repo.git(&["checkout", "--quiet", "-b", "feat/x"]);
    assert_eq!(drawn(&mut p, &cwd), "feat/x");

    // Detached, and it shows the short sha instead — the thing a person
    // can act on.
    let sha = repo.git(&["rev-parse", "--short", "HEAD"]);
    repo.git(&["checkout", "--quiet", "--detach"]);
    assert_eq!(drawn(&mut p, &cwd), sha, "a detached HEAD should show its sha");
}

#[test]
fn gitline_counts_the_drift_from_an_upstream_and_is_quiet_without_one() {
    let Some(mut p) = with_exec("gitline", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "");

    let Some(upstream) = Repo::new("gitline-upstream") else { return };
    upstream.write("f", "1\n");
    upstream.commit("a");
    upstream.write("f", "1\n2\n");
    upstream.commit("b");

    // A clone, which has an upstream and starts level with it.
    let clone = upstream.path().with_extension("clone");
    let _ = std::fs::remove_dir_all(&clone);
    let out = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(upstream.path())
        .arg(&clone)
        .output()
        .expect("clone");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let cwd = clone.to_str().expect("a utf-8 temporary path").to_string();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&clone)
            .args(["-c", "user.email=test@haven.invalid", "-c", "user.name=Haven Test"])
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };

    let mut p = with_exec("gitline", &["git"], session_in(&cwd)).expect("built");
    let shown = drawn(&mut p, &cwd);
    assert!(!shown.contains('↑') && !shown.contains('↓'), "level means no arrows: {shown:?}");

    // One commit behind and one ahead: the branch has diverged.
    git(&["reset", "--quiet", "--hard", "HEAD~1"]);
    std::fs::write(clone.join("g"), "local\n").expect("write");
    git(&["add", "--all"]);
    git(&["commit", "--quiet", "-m", "local"]);
    let shown = drawn(&mut p, &cwd);
    assert!(shown.contains("↑1"), "one commit ahead: {shown:?}");
    assert!(shown.contains("↓1"), "one commit behind: {shown:?}");

    // A branch with no upstream at all shows no arrows rather than an
    // error, because that is an ordinary state and not a fault.
    git(&["checkout", "--quiet", "-b", "solo"]);
    assert_eq!(drawn(&mut p, &cwd), "solo", "no upstream is not an error");

    let _ = std::fs::remove_dir_all(&clone);
}

#[test]
fn gitline_says_nothing_outside_a_repository() {
    // Silence, not a message: most directories are not repositories and
    // a status bar should not keep saying so.
    let Some(mut p) = with_exec("gitline", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "");

    let elsewhere = std::env::temp_dir().join(format!("haven-git-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).expect("make a temporary directory");
    let cwd = elsewhere.to_str().expect("a utf-8 temporary path").to_string();

    let mut p = with_exec("gitline", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(drawn(&mut p, &cwd), "", "not a repository, so nothing at all");

    // And with no cwd reported — a shell without Haven's shell
    // integration — there is nothing to be right about.
    let mut p = with_exec("gitline", &["git"], Session::new().pane(1, "sh", "")).expect("built");
    assert_eq!(drawn(&mut p, ""), "", "no directory means no answer");

    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn git_wip_counts_staged_and_unstaged_work_together() {
    let Some(mut p) = with_exec("git_wip", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "", "no pane, no answer");

    let Some(repo) = Repo::new("wip") else { return };
    repo.lines("a.txt", 10);
    repo.commit("ten lines");

    let cwd = repo.cwd();
    let mut p = with_exec("git_wip", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(drawn(&mut p, &cwd), "", "a clean tree shows nothing");

    // Five more lines on the end of the file: five insertions, nothing
    // removed.
    repo.lines("a.txt", 15);
    assert_eq!(drawn(&mut p, &cwd), "+5-0");

    // Staging must not change the number. The work is just as
    // uncommitted, and a count that fell on `git add` would be an alarm
    // that switches itself off.
    repo.git(&["add", "--all"]);
    assert_eq!(drawn(&mut p, &cwd), "+5-0", "staged work still counts");

    // Deletions are counted too, and shown even when nothing is added.
    repo.commit("fifteen");
    repo.lines("a.txt", 13);
    assert_eq!(drawn(&mut p, &cwd), "+0-2");

    // Committed, and there is nothing left to warn about.
    repo.commit("thirteen");
    assert_eq!(drawn(&mut p, &cwd), "", "a committed tree shows nothing");
}

#[test]
fn git_wip_shouts_past_the_threshold_and_sees_a_repository_with_no_commits() {
    let Some(mut p) = with_exec("git_wip", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "");

    let Some(repo) = Repo::new("wip-loud") else { return };
    // No commits at all, so there is no `HEAD` to diff against. A day's
    // work staged in a fresh `git init` is exactly the case the plugin
    // exists for, so it must not be silent here.
    repo.lines("big.txt", 300);
    repo.git(&["add", "--all"]);

    let cwd = repo.cwd();
    let mut p = with_exec("git_wip", &["git"], session_in(&cwd)).expect("built");
    let runs = refreshed(&mut p, &cwd);
    assert_eq!(text_of(&runs), "+300-0", "{:?}", text_of(&runs));
    assert!(bold_around(&runs, "+300"), "300 lines is past the threshold: {runs:?}");

    // Under the threshold it is plain, so the bold means something.
    // Twelve more lines on a file that is already tracked: an untracked
    // file would show nothing at all, which is `git diff`'s own limit
    // and `gitline`'s `*` to report.
    repo.commit("three hundred");
    repo.lines("big.txt", 312);
    let runs = refreshed(&mut p, &cwd);
    assert_eq!(text_of(&runs), "+12-0", "{:?}", text_of(&runs));
    assert!(!bold_around(&runs, "+12"), "a small diff should not shout: {runs:?}");
}

#[test]
fn git_stash_is_silent_until_something_is_hidden() {
    let Some(mut p) = with_exec("git_stash", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "", "no pane, no answer");

    let Some(repo) = Repo::new("stash") else { return };
    repo.lines("a.txt", 3);
    repo.commit("three");

    let cwd = repo.cwd();
    let mut p = with_exec("git_stash", &["git"], session_in(&cwd)).expect("built");
    // `refs/stash` does not exist yet, so `rev-list` fails — which is
    // the answer "none" and not an error.
    assert_eq!(drawn(&mut p, &cwd), "", "nothing stashed, nothing shown");

    repo.write("a.txt", "changed\n");
    repo.git(&["stash", "--quiet"]);
    assert_eq!(drawn(&mut p, &cwd), "⚑1", "one stash");

    repo.write("a.txt", "changed again\n");
    repo.git(&["stash", "--quiet"]);
    assert_eq!(drawn(&mut p, &cwd), "⚑2", "two stashes");

    // And it goes quiet again when the drawer is emptied.
    repo.git(&["stash", "clear"]);
    assert_eq!(drawn(&mut p, &cwd), "", "an emptied stash is silent again");
}

#[test]
fn a_git_plugin_describes_the_pane_and_not_the_daemon() {
    // The bug this test exists for: `host::run` has no
    // working-directory argument, so a plugin that ran a bare `git`
    // would describe whatever repository the daemon was started in. The
    // pane here points at a repository on a branch that exists nowhere
    // else, holding a stash and some staged work that Haven's own
    // checkout does not have.
    let Some(mut p) = with_exec("gitline", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "");

    let Some(repo) = Repo::new("not-the-daemon") else { return };
    repo.lines("a.txt", 2);
    repo.commit("one");
    repo.git(&["checkout", "--quiet", "-b", "only-in-the-pane"]);

    let cwd = repo.cwd();
    let mut p = with_exec("gitline", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(drawn(&mut p, &cwd), "only-in-the-pane", "the pane's repository, not the daemon's");

    repo.lines("a.txt", 9);
    repo.git(&["stash", "--quiet"]);
    repo.lines("b.txt", 4);
    repo.git(&["add", "--all"]);

    let mut p = with_exec("git_stash", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(drawn(&mut p, &cwd), "⚑1", "the pane's stash");

    let mut p = with_exec("git_wip", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(drawn(&mut p, &cwd), "+4-0", "the pane's uncommitted work");
}

#[test]
fn render_does_not_run_git_and_so_draws_what_the_last_event_found() {
    // The design decision all three plugins rest on: `render` is called
    // whenever Haven draws, so it formats what is already known and
    // never shells out. Two consequences are testable. Drawing before
    // any event has arrived shows nothing, and drawing after an event
    // keeps showing what that event found even though the repository
    // has changed underneath.
    let Some(mut p) = with_exec("gitline", &["git"], Session::new()) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "");

    let Some(repo) = Repo::new("cached") else { return };
    repo.lines("a.txt", 2);
    repo.commit("one");

    let cwd = repo.cwd();
    let mut p = with_exec("gitline", &["git"], session_in(&cwd)).expect("built");
    assert_eq!(
        text_of(&p.render().expect("render")),
        "",
        "nothing is known before the first event"
    );

    assert_eq!(drawn(&mut p, &cwd), "main");
    // The repository is dirty now, and nothing has told the plugin.
    repo.write("a.txt", "changed\n");
    assert_eq!(
        text_of(&p.render().expect("render")),
        "main",
        "render must not notice: it is not allowed to run git"
    );
    // The next event is what notices.
    assert_eq!(drawn(&mut p, &cwd), "main*", "and the next refresh does");
}
