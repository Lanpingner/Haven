//! Plugins telling each other things.
//!
//! The probes here are a pair and a pest: one emits when a directory
//! changes, one reacts to whatever it hears, and one answers every
//! event with another one forever.

use haven_plugin::index::Capabilities;
use haven_plugin_host::testing::{component, Session};
use haven_plugin_host::{
    route_emits, text_of, Command, CwdChange, Engine, Event, Load, Loaded, Snapshot,
    MAX_EMIT_ROUNDS,
};

fn session() -> Session {
    Session::new().pane(1, "sh", "/Projects/myapp")
}

/// Several plugins in one engine, which is what the daemon has.
fn loaded(names: &[&str]) -> Option<Vec<Loaded>> {
    let engine: &'static Engine = Box::leak(Box::new(Engine::new().expect("engine")));
    let mut out = Vec::new();
    for name in names {
        let wasm = component(name)?;
        out.push(
            Loaded::new(
                engine,
                Load {
                    name,
                    wasm: &wasm,
                    granted: &Capabilities::default(),
                    store_path: None,
                    world: session().into(),
                    cache_dir: None,
                },
            )
            .unwrap_or_else(|e| panic!("{name}: {e}")),
        );
    }
    Some(out)
}

/// What a tick does: deliver an event, then route whatever came back.
fn tick(plugins: &mut [Loaded], event: &Event) -> Vec<(String, Command)> {
    let world: Snapshot = session().into();
    let mut asked = Vec::new();
    for plugin in plugins.iter_mut() {
        plugin.refresh(world.clone());
        let name = plugin.name().to_string();
        for command in plugin.on_event(event).expect("event") {
            asked.push((name.clone(), command));
        }
    }
    route_emits(plugins, asked, &world, None)
}

#[test]
fn one_plugin_tells_another_and_it_reacts_in_the_same_tick() {
    let Some(mut plugins) = loaded(&["emit_probe", "hear_probe"]) else { return };
    let out = tick(
        &mut plugins,
        &Event::CwdChanged(CwdChange { pane: 1, cwd: "/Projects/myapp".into() }),
    );

    // The listener's notice comes back addressed to the listener, not to
    // the plugin that started it.
    let notices: Vec<(&str, &str)> = out
        .iter()
        .filter_map(|(who, c)| match c {
            Command::Notice(text) => Some((who.as_str(), text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1, "{out:?}");
    assert_eq!(notices[0].0, "hear_probe");
    assert!(notices[0].1.contains("emit_probe said env-activated"), "{notices:?}");

    // And it drew what it heard, sender included.
    let heard = plugins.iter_mut().find(|p| p.name() == "hear_probe").unwrap();
    assert_eq!(
        text_of(&heard.render().expect("render")),
        "emit_probe:env-activated:/Projects/myapp"
    );

    // Nothing emitted survives to the host: an emission is for plugins.
    assert!(!out.iter().any(|(_, c)| matches!(c, Command::Emit(_))), "{out:?}");
}

#[test]
fn the_sender_is_the_hosts_word_and_not_the_plugins() {
    // `hear_probe` reports who told it. Nothing it was given could have
    // set that field — the host stamps it from its own record of which
    // plugin returned the command, so one plugin cannot speak as another.
    let Some(mut plugins) = loaded(&["emit_probe", "hear_probe"]) else { return };
    tick(&mut plugins, &Event::CwdChanged(CwdChange { pane: 1, cwd: "/x".into() }));
    let heard = plugins.iter_mut().find(|p| p.name() == "hear_probe").unwrap();
    assert!(
        text_of(&heard.render().expect("render")).starts_with("emit_probe:"),
        "the sender was not the emitting plugin"
    );
}

#[test]
fn a_plugin_is_not_told_what_it_said_itself() {
    // The shortest loop there is. `loop_probe` answers anything it hears
    // with another emission; alone, it must hear nothing back.
    let Some(mut plugins) = loaded(&["loop_probe"]) else { return };
    let out = tick(&mut plugins, &Event::CwdChanged(CwdChange { pane: 1, cwd: "/x".into() }));
    assert!(out.is_empty(), "it heard its own emission: {out:?}");
}

#[test]
fn two_plugins_answering_each_other_are_cut_off() {
    // Two pests. Left alone they would talk until the machine stopped,
    // so the chain is bounded rather than detected — the names are the
    // plugins' own and Haven does not read them.
    let Some(mut plugins) = loaded(&["loop_probe", "hear_probe"]) else { return };
    let started = std::time::Instant::now();
    let out = tick(&mut plugins, &Event::CwdChanged(CwdChange { pane: 1, cwd: "/x".into() }));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the bus did not stop: {:?}",
        started.elapsed()
    );
    // `hear_probe` answers every `told` with a notice, so the rounds are
    // countable: one per round that ran.
    let notices = out.iter().filter(|(_, c)| matches!(c, Command::Notice(_))).count();
    assert!(notices >= 1, "nothing was delivered at all: {out:?}");
    assert!(
        notices <= MAX_EMIT_ROUNDS,
        "{notices} rounds got through, the bound is {MAX_EMIT_ROUNDS}"
    );
}

#[test]
fn a_plugin_that_hears_nothing_is_undisturbed() {
    // `clock`-like: no interest in events at all. It must not be broken
    // by being handed one.
    let Some(mut plugins) = loaded(&["hear_probe", "event_probe"]) else { return };
    let out = tick(&mut plugins, &Event::Started);
    assert!(
        !out.iter().any(|(who, _)| who == "hear_probe"),
        "it reacted to something nobody said: {out:?}"
    );
}
