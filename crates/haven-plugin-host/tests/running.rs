//! Real plugins, compiled and run.
//!
//! The components are built from `plugins/` on demand rather than
//! committed: the WIT world and the SDK are both in this repository, so a
//! committed `.wasm` would go stale the first time either changed and
//! nothing would notice. Building takes a minute the first time and is
//! cached after.
//!
//! If the `wasm32-wasip2` target is not installed these tests say so and
//! pass, because a contributor without it should still be able to run
//! the suite — CI has it, which is where it has to hold.

use haven_plugin::index::Capabilities;
use haven_plugin_host::testing::{component, plain_load, with_exec, with_pane_text, Session};
use haven_plugin_host::{text_of, Command as PluginCommand, Engine, Event, Load, Loaded};

/// A session with two panes, which is enough for every probe here.
fn session() -> Session {
    Session::new()
        .running(1, "fish", "/Projects/HavenTerminal", "cargo test")
        .pane(2, "vim", "/tmp")
}

#[test]
fn a_plugin_with_no_capabilities_loads_and_draws() {
    // `event_probe` asks for nothing at all: it reads the panes, which
    // needs no grant, keeps a count and formats it. The plugins people
    // install live in their own repository and are tested there; what has
    // to hold here is that the runtime loads and runs one.
    let Some(mut probe) = plain_load("event_probe", session()) else { return };
    assert_eq!(probe.name(), "event_probe");
    assert_eq!(text_of(&probe.render().expect("render")), "ticks: 0");

    // `render` is pure, so calling it again is allowed and is not an
    // error — the host calls it whenever it draws.
    assert_eq!(text_of(&probe.render().expect("render")), "ticks: 0");
    // And being started asks the host for nothing.
    assert_eq!(probe.on_event(&Event::Started).expect("started"), Vec::new());
}

#[test]
fn a_plugin_is_refused_what_it_was_not_granted() {
    // The whole capability model in one test: the component's world has
    // `run` in it whether or not the user allowed anything, so calling it
    // must come back as a refusal the plugin can report — not a trap, and
    // certainly not a process.
    let Some(mut runner) = plain_load("exec_probe", session()) else { return };
    let drawn = text_of(&runner.render().expect("render"));
    assert!(drawn.contains("not granted"), "expected a refusal, got {drawn:?}");
    assert!(drawn.contains("/bin/echo"), "the refusal should name the program: {drawn:?}");

    // Granted, the same plugin runs it.
    let Some(mut runner) = with_exec("exec_probe", &["/bin/echo"], session()) else { return };
    let drawn = text_of(&runner.render().expect("render"));
    assert_eq!(drawn, "ran: haven", "{drawn:?}");
}

#[test]
fn reading_a_pane_needs_the_capability_that_says_so() {
    let Some(mut reader) = plain_load("pane_probe", session()) else { return };
    let drawn = text_of(&reader.render().expect("render"));
    assert!(drawn.contains("read-pane-content"), "{drawn:?}");

    // With the grant, it sees what the pane holds.
    let Some(mut reader) =
        with_pane_text("pane_probe", session().showing(1, "a line of output\n$ "))
    else {
        return;
    };
    let drawn = text_of(&reader.render().expect("render"));
    assert!(drawn.contains("a line of output"), "{drawn:?}");
}

#[test]
fn a_plugin_sees_the_panes_without_asking_permission() {
    // Pane *titles* and directories are not sensitive; their contents
    // are. So `panes` needs no capability and `pane-text` does.
    let Some(mut probe) = plain_load("pane_probe", session()) else { return };
    let commands = probe.on_event(&Event::PaneFocus(1)).expect("event");
    let said: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            PluginCommand::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, vec!["2 panes, focus on fish"], "{commands:?}");
}

#[test]
fn an_event_comes_back_as_commands_rather_than_as_changes() {
    // The rule the whole design rests on: a plugin cannot do anything,
    // it can only ask. What comes back is data.
    let Some(mut probe) = plain_load("event_probe", session()) else { return };
    let commands = probe
        .on_event(&Event::CommandEnded(haven_plugin_host::CommandEnd {
            pane: 1,
            command: "cargo build".into(),
            status: 1,
            millis: 90_000,
        }))
        .expect("event");
    assert_eq!(
        commands,
        vec![
            PluginCommand::Notice("cargo build failed after 90s".into()),
            PluginCommand::Act("display-panes".into()),
        ],
        "{commands:?}"
    );
}

#[test]
fn what_a_plugin_remembers_outlives_the_instance() {
    let Some(wasm) = component("event_probe") else { return };
    let engine = Engine::new().expect("engine");
    let home = std::env::temp_dir().join(format!("haven-plugin-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let store_path = home.join("event_probe.store");

    let open = |engine: &Engine| {
        Loaded::new(
            engine,
            Load {
                name: "event_probe",
                wasm: &wasm,
                granted: &Capabilities::default(),
                store_path: Some(store_path.clone()),
                world: session().into(),
                cache_dir: None,
            },
        )
        .expect("load")
    };

    let mut first = open(&engine);
    let commands = first.on_event(&Event::Tick).expect("tick");
    // The plugin counts ticks, and asks the host to remember the count.
    assert!(
        commands.iter().any(|c| matches!(c, PluginCommand::Remember(r) if r.key == "ticks")),
        "{commands:?}"
    );
    // Applying the command is the host's job, as it is for every command.
    for command in &commands {
        if let PluginCommand::Remember(r) = command {
            first.remember(&r.key, Some(&r.value));
        }
    }
    drop(first);

    // A new instance, and the plugin finds what it left behind.
    let mut second = open(&engine);
    let drawn = text_of(&second.render().expect("render"));
    assert_eq!(drawn, "ticks: 1", "what it remembered did not survive: {drawn:?}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_runaway_plugin_is_stopped_and_named() {
    // Fuel, not a timer: a plugin that loops forever stops after a fixed
    // number of instructions, deterministically, whatever else the
    // machine is doing.
    let Some(mut spinner) = plain_load("spin_probe", session()) else { return };
    let e = spinner.render().expect_err("an endless loop must not be tolerated");
    let shown = e.to_string();
    assert!(shown.contains("spin_probe"), "the plugin has to be named: {shown}");
    assert!(shown.to_lowercase().contains("fuel"), "the reason should be the fuel: {shown}");
}

#[test]
fn a_plugin_that_refuses_to_start_is_a_load_failure_with_a_reason() {
    let Some(wasm) = component("fail_probe") else { return };
    let engine = Engine::new().expect("engine");
    let e = Loaded::new(
        &engine,
        Load {
            name: "fail_probe",
            wasm: &wasm,
            granted: &Capabilities::default(),
            store_path: None,
            world: session().into(),
            cache_dir: None,
        },
    )
    .expect_err("init returned an error");
    let shown = e.to_string();
    assert!(shown.contains("fail_probe"), "{shown}");
    assert!(shown.contains("on purpose"), "the plugin's own reason: {shown}");
}

#[test]
fn a_plugin_cannot_reach_the_filesystem_or_the_network() {
    // The sandbox is not a list of things Haven forbids; it is a WASI
    // context with nothing in it. A plugin that reads a file gets an
    // error from an empty filesystem, and this is the test that would
    // fail if someone granted a preopened directory "for convenience".
    let Some(mut sneak) = plain_load("sandbox_probe", session()) else { return };
    let drawn = text_of(&sneak.render().expect("render"));
    assert!(drawn.starts_with("denied:"), "something got through: {drawn:?}");
    assert!(!drawn.contains("root:"), "it read /etc/passwd: {drawn:?}");
}

#[test]
fn what_a_plugin_logs_is_kept_for_whoever_has_to_fix_it() {
    let Some(mut probe) = plain_load("event_probe", session()) else { return };
    probe.on_event(&Event::Started).expect("started");
    assert!(
        probe.log().iter().any(|line| line.contains("started")),
        "nothing logged: {:?}",
        probe.log()
    );
}
