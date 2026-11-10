//! How long loading costs, measured rather than assumed.
//!
//! Not a benchmark: it asserts an order of magnitude, because the point
//! is to notice if loading a plugin ever becomes slow enough to be felt
//! when a terminal opens. Run it with `--release`; a debug build
//! measures cranelift compiled without optimisation, which is five to
//! ten times slower than anything a user would run and was the reason a
//! first attempt at this put the cost at seven hundred milliseconds per
//! plugin instead of fifty.

use std::time::Instant;

use haven_plugin::index::Capabilities;
use haven_plugin_host::testing::{built_plugins, component, Session};
use haven_plugin_host::{Engine, Load, Loaded};

#[test]
fn loading_every_plugin_is_not_something_you_would_wait_for() {
    let Some(dir) = built_plugins() else { return };
    let names: Vec<String> = std::fs::read_dir(dir)
        .expect("built plugins")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".wasm").map(str::to_string)
        })
        // The probes are fixtures; one of them never returns.
        .filter(|name| !name.ends_with("_probe"))
        .collect();
    assert!(names.len() >= 10, "expected the shipped plugins, found {names:?}");

    let cache = std::env::temp_dir().join(format!("haven-timing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).expect("cache dir");

    let load = |engine: &Engine, name: &str, cache: Option<&std::path::Path>| {
        let wasm = component(name).expect("built");
        Loaded::new(
            engine,
            Load {
                name,
                wasm: &wasm,
                granted: &Capabilities::default(),
                store_path: None,
                world: Session::new().pane(1, "sh", "/tmp").into(),
                cache_dir: cache.map(|c| c.to_path_buf()),
            },
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"))
    };

    // Cold: one engine, nothing cached, so every component is compiled.
    let engine = Engine::new().expect("engine");
    let started = Instant::now();
    for name in &names {
        let _ = load(&engine, name, Some(&cache));
    }
    let cold = started.elapsed();

    // Warm: a new engine, so nothing is cached in memory, but the
    // compiled artifacts are on disk from the pass above. This is the
    // path a daemon takes on every start after the first.
    let engine = Engine::new().expect("engine");
    let started = Instant::now();
    for name in &names {
        let _ = load(&engine, name, Some(&cache));
    }
    let warm = started.elapsed();

    println!(
        "{} plugins: {}ms compiling, {}ms from the precompiled artifacts \
         ({}ms and {}ms each)",
        names.len(),
        cold.as_millis(),
        warm.as_millis(),
        cold.as_millis() / names.len() as u128,
        warm.as_millis() / names.len() as u128,
    );

    // The numbers that matter. Generous, because this runs on whatever
    // CI is given, and the thing being guarded against is an order of
    // magnitude rather than a few milliseconds.
    assert!(
        warm < cold,
        "loading a precompiled artifact ({warm:?}) should beat compiling one ({cold:?})"
    );
    assert!(
        warm.as_millis() < 1_500,
        "loading {} precompiled plugins took {warm:?}, which a terminal would be waiting for",
        names.len()
    );

    let _ = std::fs::remove_dir_all(&cache);
}
