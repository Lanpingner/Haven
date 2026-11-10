//! The three plugins about the lifecycle of commands and time:
//! `command-timer`, `fail-watch` and `pomodoro`.
//!
//! All three are driven by events and none of them draws anything it had
//! to ask the host for, so "what does this plugin do" is answered here
//! exactly as the design intends: load the real component, hand it an
//! event, and compare the commands that come back.
//!
//! The interesting tests are the negative ones. A plugin that notices
//! things is only as good as the things it declines to notice, so most of
//! what follows asserts that nothing came back: a fast command, a Ctrl-C,
//! a `grep` that found no matches.
//!
//! Each test begins by loading and returns if that gave nothing, which
//! means the suite passes for a contributor without the `wasm32-wasip2`
//! target. CI has it, which is where it has to hold.

use haven_plugin::index::Capabilities;
use haven_plugin_host::testing::{component, plain_load, Session};
use haven_plugin_host::{
    text_of, Command as PluginCommand, CommandEnd, Engine, Event, Load, Loaded,
};

/// One pane running something, which is all any of these three needs.
fn session() -> Session {
    Session::new().running(1, "fish", "/Projects/HavenTerminal", "cargo build")
}

/// A finished command, as shell integration would report one.
fn ended(command: &str, status: i32, millis: u64) -> Event {
    Event::CommandEnded(CommandEnd { pane: 1, command: command.into(), status, millis })
}

/// Just the notices, which is what a reader of these tests cares about.
fn notices(commands: &[PluginCommand]) -> Vec<&str> {
    commands
        .iter()
        .filter_map(|c| match c {
            PluginCommand::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Apply every `remember` a plugin asked for, as the daemon would.
fn apply(plugin: &mut Loaded, commands: &[PluginCommand]) {
    for command in commands {
        match command {
            PluginCommand::Remember(r) => plugin.remember(&r.key, Some(&r.value)),
            PluginCommand::Forget(key) => plugin.remember(key, None),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- timer

#[test]
fn a_long_command_is_worth_saying_how_long_it_took() {
    let Some(mut p) = plain_load("command_timer", session()) else { return };
    let commands = p.on_event(&ended("cargo build", 0, 134_000)).expect("ended");
    assert_eq!(notices(&commands), vec!["cargo build took 2m14s"], "{commands:?}");
    // And the duration is kept, so the bar can show it after the notice
    // has gone.
    assert!(
        commands.iter().any(|c| matches!(c, PluginCommand::Remember(r) if r.value == "2m14s")),
        "{commands:?}"
    );
}

#[test]
fn a_fast_command_is_not_worth_saying_anything_about() {
    // The judgement the plugin exists for: `ls` in eight milliseconds is
    // not news, and a notice for it teaches people to ignore notices.
    let Some(mut p) = plain_load("command_timer", session()) else { return };
    for millis in [0, 8, 900, 4_000, 9_999] {
        let commands = p.on_event(&ended("ls", 0, millis)).expect("ended");
        assert_eq!(commands, Vec::new(), "{millis}ms should be silent: {commands:?}");
    }
    // Nothing was remembered either, so the status bar stays as it was.
    assert_eq!(text_of(&p.render().expect("render")), "", "a fast command moved the bar");
}

#[test]
fn durations_are_formatted_the_way_a_person_says_them() {
    let Some(mut p) = plain_load("command_timer", session()) else { return };
    // Exactly on the threshold counts as long enough: the boundary has to
    // belong to one side and the one that speaks is the useful side.
    for (millis, expected) in [
        (10_000, "10.0s"),
        (48_600, "48.6s"),
        (59_900, "59.9s"),
        (60_000, "1m00s"),
        (134_000, "2m14s"),
        (3_780_000, "1h03m"),
    ] {
        let commands = p.on_event(&ended("sleep", 0, millis)).expect("ended");
        assert_eq!(notices(&commands), vec![format!("sleep took {expected}")], "{millis}ms");
    }
}

#[test]
fn the_last_duration_is_on_the_bar_after_the_notice_has_gone() {
    let Some(mut p) = plain_load("command_timer", session()) else { return };
    // Nothing yet, and nothing drawn: a plugin with nothing to say draws
    // no segment rather than an empty one.
    assert_eq!(text_of(&p.render().expect("render")), "");

    let commands = p.on_event(&ended("cargo test", 0, 95_000)).expect("ended");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "last 1m35s");

    // A failing command still took the time it took, so it is still timed.
    let commands = p.on_event(&ended("cargo test", 101, 12_300)).expect("ended");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "last 12.3s");
}

#[test]
fn the_timer_only_cares_about_commands_ending() {
    let Some(mut p) = plain_load("command_timer", session()) else { return };
    for event in [Event::Started, Event::Tick, Event::PaneFocus(1), Event::PaneClosed(1)] {
        let commands = p.on_event(&event).expect("event");
        assert_eq!(commands, Vec::new(), "{event:?} should be ignored: {commands:?}");
    }
}

// ----------------------------------------------------------- fail-watch

#[test]
fn a_failed_command_is_named_with_what_went_wrong() {
    let Some(mut p) = plain_load("fail_watch", session()) else { return };
    for (command, status, expected) in [
        ("cargo build", 1, "cargo: failed"),
        ("mke2fs /dev/null", 127, "mke2fs: not found"),
        ("./deploy.sh", 126, "deploy.sh: not executable"),
        ("make -j16", 137, "make: killed (SIGKILL)"),
        ("ffmpeg -i in.mp4", 139, "ffmpeg: crashed (SIGSEGV)"),
        ("cc main.c", 134, "cc: aborted (SIGABRT)"),
        ("curl https://example.invalid", 28, "curl: exited 28"),
        ("weird", 150, "weird: killed by signal 22"),
    ] {
        let commands = p.on_event(&ended(command, status, 1_000)).expect("ended");
        assert_eq!(notices(&commands), vec![expected], "{command} exiting {status}");
    }
}

#[test]
fn pressing_ctrl_c_is_not_a_failure() {
    // The one that decides whether this plugin survives its first day.
    // Being told that the thing you just stopped has stopped is worse
    // than being told nothing, and it happens many times an hour.
    let Some(mut p) = plain_load("fail_watch", session()) else { return };
    for status in [130, 131] {
        let commands = p.on_event(&ended("cargo build", status, 4_000)).expect("ended");
        assert_eq!(commands, Vec::new(), "status {status} should be silent: {commands:?}");
    }
}

#[test]
fn grep_finding_nothing_is_an_answer_and_not_a_fault() {
    let Some(mut p) = plain_load("fail_watch", session()) else { return };
    for command in [
        "grep TODO src/",
        "LC_ALL=C /usr/bin/grep -r x .",
        "rg haven",
        "diff a b",
        "cmp a b",
        "test -f missing",
    ] {
        let commands = p.on_event(&ended(command, 1, 40)).expect("ended");
        assert_eq!(commands, Vec::new(), "{command} exiting 1 should be silent: {commands:?}");
    }
    // Only status 1 is excused. `grep` exiting 2 is a bad pattern or an
    // unreadable file, which is a real error and is still reported.
    let commands = p.on_event(&ended("grep '[' src/", 2, 40)).expect("ended");
    assert_eq!(notices(&commands), vec!["grep: exited 2"], "{commands:?}");
}

#[test]
fn a_command_that_succeeded_or_said_nothing_is_passed_over() {
    let Some(mut p) = plain_load("fail_watch", session()) else { return };
    // 0 is success, and -1 is the shell not having reported a status —
    // which is not the same thing as a failure and must not read as one.
    for status in [0, -1] {
        let commands = p.on_event(&ended("cargo build", status, 2_000)).expect("ended");
        assert_eq!(commands, Vec::new(), "status {status}: {commands:?}");
    }
}

#[test]
fn the_watcher_keeps_nothing_and_draws_nothing() {
    // It is a pure function of the event, which is why it has no state to
    // lose on a reload. This test is what would fail if it grew some.
    let Some(mut p) = plain_load("fail_watch", session()) else { return };
    let commands = p.on_event(&ended("cargo build", 1, 2_000)).expect("ended");
    assert_eq!(commands.len(), 1, "a notice and nothing else: {commands:?}");
    assert_eq!(text_of(&p.render().expect("render")), "");
}

// ------------------------------------------------------------- pomodoro

#[test]
fn a_pomodoro_starts_a_work_phase_when_it_loads() {
    let Some(mut p) = plain_load("pomodoro", session()) else { return };
    let commands = p.on_event(&Event::Started).expect("started");
    assert!(
        commands.iter().any(
            |c| matches!(c, PluginCommand::Remember(r) if r.key == "state" && r.value == "work:0:0")
        ),
        "{commands:?}"
    );
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "work 25:00");
}

#[test]
fn a_tick_moves_the_clock_by_the_interval_the_manifest_asked_for() {
    let Some(mut p) = plain_load("pomodoro", session()) else { return };
    let started = p.on_event(&Event::Started).expect("started");
    apply(&mut p, &started);
    let commands = p.on_event(&Event::Tick).expect("tick");
    // Two seconds a tick, as `timer:2s` in the manifest says, and no
    // notice until a phase actually ends.
    assert_eq!(notices(&commands), Vec::<&str>::new(), "{commands:?}");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "work 24:58");
}

#[test]
fn the_end_of_a_phase_is_the_notice_the_plugin_exists_for() {
    let Some(mut p) = plain_load("pomodoro", session()) else { return };
    // Twenty-five minutes is 750 ticks of two seconds, so the 750th ends
    // it. Setting the count rather than delivering 749 ticks: the host
    // applying a `remember` is the same path the plugin's own commands
    // take.
    p.remember("state", Some("work:749:0"));
    let commands = p.on_event(&Event::Tick).expect("tick");
    assert_eq!(notices(&commands), vec!["time's up — take 5"], "{commands:?}");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "break 5:00");

    // Five minutes is 150 ticks, and a break ending sends you back.
    p.remember("state", Some("break:149:1"));
    let commands = p.on_event(&Event::Tick).expect("tick");
    assert_eq!(notices(&commands), vec!["break over — back to it"], "{commands:?}");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "work 25:00");
}

#[test]
fn every_fourth_work_phase_earns_the_long_break() {
    let Some(mut p) = plain_load("pomodoro", session()) else { return };
    // Three already done, so this one is the fourth.
    p.remember("state", Some("work:749:3"));
    let commands = p.on_event(&Event::Tick).expect("tick");
    assert_eq!(notices(&commands), vec!["four done — take 15"], "{commands:?}");
    apply(&mut p, &commands);
    assert_eq!(text_of(&p.render().expect("render")), "long 15:00");

    // And the long break leads back to work like any other.
    p.remember("state", Some("long:449:4"));
    let commands = p.on_event(&Event::Tick).expect("tick");
    assert_eq!(notices(&commands), vec!["break over — back to it"], "{commands:?}");
}

#[test]
fn a_mid_break_clock_reads_as_minutes_and_seconds() {
    let Some(mut p) = plain_load("pomodoro", session()) else { return };
    p.remember("state", Some("break:29:1"));
    assert_eq!(text_of(&p.render().expect("render")), "break 4:02");
    // Nonsense in the store starts a fresh pomodoro rather than drawing a
    // blank bar: the store is a string map some other version of this
    // plugin may have written.
    p.remember("state", Some("elevenses:x:y"));
    assert_eq!(text_of(&p.render().expect("render")), "work 25:00");
}

#[test]
fn a_pomodoro_survives_the_plugin_being_reloaded() {
    // The reason the phase and the count go through `remember` rather
    // than a `static`: a reload is invisible to whoever is watching the
    // bar, so the timer has to be invisible too.
    let Some(wasm) = component("pomodoro") else { return };
    let engine = Engine::new().expect("engine");
    let home = std::env::temp_dir().join(format!("haven-pomodoro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let store_path = home.join("pomodoro.store");

    let open = |engine: &Engine| {
        Loaded::new(
            engine,
            Load {
                name: "pomodoro",
                wasm: &wasm,
                granted: &Capabilities::default(),
                store_path: Some(store_path.clone()),
                world: session().into(),
            },
        )
        .expect("load")
    };

    let mut first = open(&engine);
    let commands = first.on_event(&Event::Started).expect("started");
    apply(&mut first, &commands);
    for _ in 0..30 {
        let commands = first.on_event(&Event::Tick).expect("tick");
        apply(&mut first, &commands);
    }
    // Thirty ticks of two seconds is a minute of the twenty-five.
    assert_eq!(text_of(&first.render().expect("render")), "work 24:00");
    drop(first);

    let mut second = open(&engine);
    assert_eq!(
        text_of(&second.render().expect("render")),
        "work 24:00",
        "the phase did not survive the reload"
    );

    let _ = std::fs::remove_dir_all(&home);
}
