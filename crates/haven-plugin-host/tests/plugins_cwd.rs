//! The three plugins that read where the focused pane is: `cwd-crumbs`,
//! `python-venv` and `ssh-marker`.
//!
//! All three are pure functions of the pane facts, so a test is a
//! made-up session and a comparison with what `render` draws. None of
//! them is granted anything, which is the other thing these tests hold:
//! a plugin that only needs `host::panes()` must work with no
//! capabilities at all.
//!
//! Every test returns early when the `wasm32-wasip2` target is missing,
//! so the suite still passes for a contributor who has not installed it.

use haven_plugin_host::testing::{plain_load, Session};
use haven_plugin_host::text_of;

/// One focused pane with a directory and nothing else, which is all
/// `cwd-crumbs` and `python-venv` look at.
fn at(cwd: &str) -> Session {
    Session::new().pane(1, "fish", cwd)
}

/// One focused pane running something, for `ssh-marker`.
fn running(command: &str) -> Session {
    Session::new().running(1, "fish", "/Projects/HavenTerminal", command)
}

#[test]
fn a_deep_path_under_home_becomes_one_letter_a_component_and_the_leaf() {
    let Some(mut p) =
        plain_load("cwd_crumbs", at("/home/romeo/Projects/HavenTerminal/crates/haven-client"))
    else {
        return;
    };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "~/P/H/c/haven-client", "{drawn:?}");
}

#[test]
fn a_path_that_is_not_under_home_keeps_its_leading_slash() {
    let Some(mut p) = plain_load("cwd_crumbs", at("/usr/local/share/fonts")) else { return };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "/u/l/s/fonts", "{drawn:?}");
}

#[test]
fn the_root_directory_and_a_single_component_are_already_short() {
    // The two paths with no ancestors to abbreviate. `/` is the one that
    // would divide by zero in a shortener written carelessly.
    let Some(mut p) = plain_load("cwd_crumbs", at("/")) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "/", "the root directory");

    let Some(mut p) = plain_load("cwd_crumbs", at("/tmp")) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "/tmp", "one component");

    // Home exactly: the prefix is the whole answer.
    let Some(mut p) = plain_load("cwd_crumbs", at("/home/romeo")) else { return };
    assert_eq!(text_of(&p.render().expect("render")), "~", "home itself");
}

#[test]
fn a_dotted_component_keeps_two_characters_because_a_dot_says_nothing() {
    let Some(mut p) = plain_load("cwd_crumbs", at("/home/romeo/.config/fish")) else { return };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "~/.c/fish", "{drawn:?}");
}

#[test]
fn a_very_long_final_component_is_cut_rather_than_allowed_to_take_the_bar() {
    let long = "/home/romeo/com.example.deeply.named.module";
    let Some(mut p) = plain_load("cwd_crumbs", at(long)) else { return };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "~/com.example.deeply.…", "{drawn:?}");
    assert!(drawn.chars().count() <= 22, "the whole segment stays short: {drawn:?}");
}

#[test]
fn an_unknown_directory_draws_nothing_at_all() {
    // No `OSC 7`, so Haven does not know. A segment that says `?` all
    // day is worse than one that is absent.
    let Some(mut p) = plain_load("cwd_crumbs", at("")) else { return };
    let runs = p.render().expect("render");
    assert!(runs.is_empty(), "expected nothing drawn, got {:?}", text_of(&runs));
}

#[test]
fn the_crumbs_follow_the_focused_pane_and_not_the_first_one() {
    let session = Session::new().pane(1, "fish", "/tmp").pane(2, "vim", "/home/romeo/src").focus(2);
    let Some(mut p) = plain_load("cwd_crumbs", session) else { return };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "~/src", "{drawn:?}");
}

#[test]
fn an_environment_inside_a_project_is_named_after_the_project() {
    let Some(mut p) =
        plain_load("python_venv", at("/home/romeo/Projects/haven/.venv/lib/python3.12"))
    else {
        return;
    };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "venv:haven", "{drawn:?}");
}

#[test]
fn an_environment_kept_beside_others_is_named_after_itself() {
    // `~/.virtualenvs/<name>` and `~/.conda/envs/<name>`: the name is
    // the component after the marker, not the one before it.
    for (cwd, want) in [
        ("/home/romeo/.virtualenvs/haven/bin", "venv:haven"),
        ("/home/romeo/.conda/envs/scratch", "venv:scratch"),
    ] {
        let Some(mut p) = plain_load("python_venv", at(cwd)) else { return };
        let drawn = text_of(&p.render().expect("render"));
        assert_eq!(drawn, want, "{cwd}: {drawn:?}");
    }
}

#[test]
fn a_path_with_no_sign_of_an_environment_draws_nothing() {
    // Including the case that matters most: an environment really is
    // active, and the path cannot say so. Quiet is the right answer.
    for cwd in ["/home/romeo/Projects/haven", "/", "", "/home/romeo/Projects/env/src"] {
        let Some(mut p) = plain_load("python_venv", at(cwd)) else { return };
        let runs = p.render().expect("render");
        assert!(runs.is_empty(), "{cwd}: expected nothing, got {:?}", text_of(&runs));
    }
}

#[test]
fn a_marker_directory_with_nothing_around_it_names_no_project() {
    // `/venv` has no parent to be named after, and `venv:venv` would
    // tell nobody anything.
    for cwd in ["/venv", "/home/romeo/.virtualenvs"] {
        let Some(mut p) = plain_load("python_venv", at(cwd)) else { return };
        let runs = p.render().expect("render");
        assert!(runs.is_empty(), "{cwd}: expected nothing, got {:?}", text_of(&runs));
    }
}

#[test]
fn an_ssh_command_shows_the_host_without_the_user_or_the_domain() {
    let Some(mut p) = plain_load("ssh_marker", running("ssh romeo@box.example.com")) else {
        return;
    };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "⇥ box", "{drawn:?}");
}

#[test]
fn the_marker_is_bold_because_it_has_to_be_seen_before_the_next_command() {
    let Some(mut p) = plain_load("ssh_marker", running("ssh box")) else { return };
    let runs = p.render().expect("render");
    assert!(!runs.is_empty(), "nothing drawn for a plain ssh");
    assert!(runs.iter().all(|r| r.style.bold), "expected bold runs, got {runs:?}");
}

#[test]
fn an_option_that_swallows_a_word_is_not_mistaken_for_the_destination() {
    for (command, want) in [
        ("ssh -p 2222 box", "⇥ box"),
        ("ssh -i /home/romeo/.ssh/id_ed25519 romeo@box", "⇥ box"),
        ("ssh -v box", "⇥ box"),
        ("mosh 10.0.0.5", "⇥ 10.0.0.5"),
        ("/usr/bin/ssh box", "⇥ box"),
    ] {
        let Some(mut p) = plain_load("ssh_marker", running(command)) else { return };
        let drawn = text_of(&p.render().expect("render"));
        assert_eq!(drawn, want, "{command}: {drawn:?}");
    }
}

#[test]
fn a_container_or_a_cluster_is_marked_only_in_the_subcommand_that_is_remote() {
    for (command, want) in [
        ("kubectl -n prod exec api-7 -- sh", "⇥ api-7"),
        ("docker exec -it db psql", "⇥ db"),
        ("distrobox enter arch", "⇥ arch"),
        ("toolbox enter fedora", "⇥ fedora"),
    ] {
        let Some(mut p) = plain_load("ssh_marker", running(command)) else { return };
        let drawn = text_of(&p.render().expect("render"));
        assert_eq!(drawn, want, "{command}: {drawn:?}");
    }
    // The same programs, locally.
    for command in ["docker ps", "kubectl get pods", "docker build ."] {
        let Some(mut p) = plain_load("ssh_marker", running(command)) else { return };
        let runs = p.render().expect("render");
        assert!(runs.is_empty(), "{command}: expected nothing, got {:?}", text_of(&runs));
    }
}

#[test]
fn something_remote_with_no_destination_in_it_still_gets_a_bare_marker() {
    // `ssh` alone reads a destination from `~/.ssh/config` or from a
    // prompt, and a vague marker beats no marker.
    let Some(mut p) = plain_load("ssh_marker", running("ssh")) else { return };
    let drawn = text_of(&p.render().expect("render"));
    assert_eq!(drawn, "⇥", "{drawn:?}");
}

#[test]
fn a_pane_that_is_not_somewhere_else_is_not_marked() {
    for command in ["cargo test", "", "vim src/lib.rs", "nvim /home/romeo/.ssh/config"] {
        let Some(mut p) = plain_load("ssh_marker", running(command)) else { return };
        let runs = p.render().expect("render");
        assert!(runs.is_empty(), "{command:?}: expected nothing, got {:?}", text_of(&runs));
    }
}

#[test]
fn a_title_that_merely_looks_like_a_login_is_not_evidence_of_one() {
    // A local shell commonly titles itself `romeo@arch: ~`. Marking that
    // would make the marker meaningless, which is worse than missing a
    // session whose shell integration is not loaded.
    let Some(mut p) = plain_load("ssh_marker", Session::new().pane(1, "romeo@arch: ~", "/tmp"))
    else {
        return;
    };
    let runs = p.render().expect("render");
    assert!(runs.is_empty(), "expected nothing, got {:?}", text_of(&runs));

    // A launcher named in the title does count, since `command` is empty
    // without shell integration and a marker is the safer mistake.
    let Some(mut p) = plain_load("ssh_marker", Session::new().pane(1, "ssh box", "/tmp")) else {
        return;
    };
    assert_eq!(text_of(&p.render().expect("render")), "⇥ box");
}
