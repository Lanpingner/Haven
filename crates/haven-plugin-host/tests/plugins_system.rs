//! The three plugins that report on the machine: `load-average`,
//! `disk-free` and `docker-status`.
//!
//! All three are nothing but a program, a parser and a cache, so there
//! are three kinds of test here and each plugin gets all three.
//!
//! * **Refused.** Loaded with no capabilities at all, a plugin must come
//!   back saying which program it wanted. This is the test that matters
//!   most, because the failure it prevents is silent: a plugin that draws
//!   nothing when it lacks a permission is one nobody can diagnose.
//! * **Real.** Granted the program, against this machine, asserting on
//!   the *shape* of what comes back. A load average is a number that
//!   changes between two calls, so the assertion is that it parses as
//!   one, not that it equals anything. Each of these returns quietly
//!   when the program is not installed, which for `docker` is most
//!   machines.
//! * **Recorded.** The parsing is the part worth testing hardest and it
//!   lives inside the component, where a native unit test cannot reach
//!   it: the plugin's only way in is `host::run`, and `run` allowlists
//!   `argv[0]`, so there is no way to hand it a string. What there *is*
//!   is `PATH`: a directory of three-line stubs called `uptime`, `df`
//!   and `podman` turns "what does this plugin do with macOS's `uptime`
//!   output" into a test that can be written on Linux. That is how the
//!   cases no live machine can produce — BSD's block-count `df`, an
//!   `autofs` mount with a space in its device name, a Docker daemon
//!   that will not answer — are covered.
//!
//! `PATH` is process-global, so every test in this file takes the same
//! lock and they run one at a time. Cheap: the plugins are built once
//! for the whole binary and each test is one instantiation.
//!
//! Every test begins with `let Some(mut p) = … else { return }` so that
//! the suite passes on a machine without the `wasm32-wasip2` target.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use haven_plugin_host::testing::{plain_load, with_exec, Session};
use haven_plugin_host::{text_of, Event, Run};

/// Crate names have underscores where the plugin's directory has a
/// hyphen, and the component is named after the crate.
const LOAD: &str = "load_average";
const DISK: &str = "disk_free";
const DOCKER: &str = "docker_status";

/// A session whose focused pane is somewhere that certainly exists.
fn session() -> Session {
    Session::new().running(1, "fish", env!("CARGO_MANIFEST_DIR"), "cargo test").pane(2, "vim", "/")
}

/// One test at a time, because the recorded-output tests put a stub
/// directory on `PATH` and `PATH` belongs to the process.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // A test that fails while holding this should not turn every later
    // test into a poisoned-mutex panic that hides it.
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a program is on this machine, so a test about it can be
/// skipped rather than failed. `docker` very likely is not.
fn installed(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// What the plugin drew, as text.
fn drawn(runs: &[Run]) -> String {
    text_of(runs)
}

// ---------------------------------------------------------------- load

#[test]
fn load_average_refused_says_which_program_it_wanted() {
    let _lock = one_at_a_time();
    let Some(mut p) = plain_load(LOAD, session()) else { return };
    // The refresh happens on the event, not in `render`, so the refusal
    // only exists once an event has been delivered — which is itself the
    // caching contract: `render` alone never runs anything.
    assert_eq!(drawn(&p.render().expect("render")), "load …", "nothing has been asked yet");

    p.on_event(&Event::Started).expect("started");
    let shown = drawn(&p.render().expect("render"));
    assert!(shown.contains("not granted"), "expected a refusal, got {shown:?}");
    assert!(shown.contains("uptime"), "the refusal should name the program: {shown:?}");
}

#[test]
fn load_average_reads_this_machine_s_load() {
    let _lock = one_at_a_time();
    if !installed("uptime") {
        return;
    }
    let Some(mut p) = with_exec(LOAD, &["uptime", "nproc"], session()) else { return };
    p.on_event(&Event::Started).expect("started");
    let shown = drawn(&p.render().expect("render"));
    // A load average is a number that is different by the time you read
    // it, so the assertion is that it is a number at all.
    let figure: f64 = shown.parse().unwrap_or_else(|_| panic!("expected a figure, got {shown:?}"));
    assert!((0.0..10_000.0).contains(&figure), "{shown:?}");
    assert_eq!(shown.matches('.').count(), 1, "two decimal places, as uptime prints: {shown:?}");

    // And a tick refreshes it rather than appending to it.
    p.on_event(&Event::Tick).expect("tick");
    let again = drawn(&p.render().expect("render"));
    assert!(again.parse::<f64>().is_ok(), "{again:?}");
}

#[test]
fn load_average_without_nproc_still_shows_the_figure() {
    let _lock = one_at_a_time();
    if !installed("uptime") {
        return;
    }
    // The second capability is optional by design: refused `nproc`, the
    // plugin has no threshold to compare against and so never emphasises
    // the figure, but it still reports it.
    let Some(mut p) = with_exec(LOAD, &["uptime"], session()) else { return };
    p.on_event(&Event::Started).expect("started");
    let runs = p.render().expect("render");
    assert!(drawn(&runs).parse::<f64>().is_ok(), "{:?}", drawn(&runs));
    assert!(!runs[0].style.bold, "with no core count there is nothing to be bold about");
}

#[cfg(unix)]
#[test]
fn load_average_parses_what_each_platform_s_uptime_prints() {
    let _lock = one_at_a_time();
    // (what `uptime` printed, how many cores, what should be drawn, bold)
    let cases = [
        (" 18:03:44 up 22:30,  2 users,  load average: 0.52, 3.62, 3.13", "16", "0.52", false),
        // macOS: "averages", no commas between the figures, and a
        // different prefix — which is why the parser looks for the words
        // rather than counting fields.
        ("18:03  up 3 days,  2 users, load averages: 1.41 3.62 3.13", "16", "1.41", false),
        // A locale that writes a decimal comma.
        (" 18:03:44 up 1:02,  1 user,  load average: 1,41, 3,62, 3,13", "16", "1.41", false),
        // Busier than the machine has cores for: the one case that is
        // drawn in a weight the eye catches.
        (" 18:03:44 up 1:02,  1 user,  load average: 9.87, 8.00, 7.00", "4", "9.87", true),
        // Exactly the core count is not over it.
        (" 18:03:44 up 1:02,  1 user,  load average: 4.00, 1.00, 1.00", "4", "4.00", false),
        // Something that is not an `uptime` at all, and is said to be so
        // rather than drawn as a zero.
        ("something else entirely", "4", "load ?", false),
    ];
    for (recorded, cores, expected, bold) in cases {
        let dir = stubs(&format!("uptime-{expected}-{cores}"));
        stub(&dir, "uptime", &format!("echo {}", shell_quoted(recorded)));
        stub(&dir, "nproc", &format!("echo {cores}"));
        let Some(mut p) = with_exec(LOAD, &["uptime", "nproc"], session()) else { return };
        let runs = on_path(&dir, || {
            p.on_event(&Event::Started).expect("started");
            p.render().expect("render")
        });
        assert_eq!(drawn(&runs), expected, "from {recorded:?}");
        assert_eq!(runs[0].style.bold, bold, "from {recorded:?}");
    }
}

// ---------------------------------------------------------------- disk

#[test]
fn disk_free_refused_says_which_program_it_wanted() {
    let _lock = one_at_a_time();
    let Some(mut p) = plain_load(DISK, session()) else { return };
    p.on_event(&Event::Started).expect("started");
    let shown = drawn(&p.render().expect("render"));
    assert!(shown.contains("not granted"), "expected a refusal, got {shown:?}");
    assert!(shown.contains("df"), "the refusal should name the program: {shown:?}");
}

#[test]
fn disk_free_reports_on_the_focused_pane_s_filesystem() {
    let _lock = one_at_a_time();
    if !installed("df") {
        return;
    }
    let Some(mut p) = with_exec(DISK, &["df"], session()) else { return };
    p.on_event(&Event::Started).expect("started");
    let runs = p.render().expect("render");
    let shown = drawn(&runs);
    // `<free> <used>%`, whatever this machine's disk happens to hold.
    let (free, used) =
        shown.split_once(' ').unwrap_or_else(|| panic!("expected two parts: {shown:?}"));
    assert!(free.ends_with(['K', 'M', 'G', 'T', 'P']), "free space should carry a unit: {shown:?}");
    assert!(free[..free.len() - 1].parse::<f64>().is_ok(), "{shown:?}");
    let percent = used.trim().trim_end_matches('%');
    assert!(used.ends_with('%'), "{shown:?}");
    assert!(percent.parse::<u8>().is_ok(), "{shown:?}");
    // Two runs, and the percentage is the dim one: it is context, not
    // the number being reported.
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[1].style.dim, "{runs:?}");
}

#[test]
fn disk_free_admits_when_nobody_has_said_where_the_pane_is() {
    let _lock = one_at_a_time();
    // `cwd` is empty until the shell reports it with OSC 7, which is the
    // common case on a shell with no integration installed. Reporting on
    // `/` instead would be wrong in a way nobody would notice.
    let Some(mut p) = with_exec(DISK, &["df"], Session::new().pane(1, "sh", "")) else { return };
    p.on_event(&Event::Started).expect("started");
    assert_eq!(drawn(&p.render().expect("render")), "df ?");
}

#[cfg(unix)]
#[test]
fn disk_free_parses_what_each_platform_s_df_prints() {
    let _lock = one_at_a_time();
    let cases = [
        // GNU `df -P -h`: already human, and its own rounding is kept.
        (
            "Filesystem      Size  Used Avail Use% Mounted on\n\
             /dev/nvme1n1p1  469G  239G  207G  54% /Projects",
            "207G 54%",
            false,
        ),
        // Nearly full is the one case worth the eye.
        (
            "Filesystem     Size  Used Avail Use% Mounted on\n\
             /dev/sda1       16G   15G  9.4G  96% /",
            "9.4G 96%",
            true,
        ),
        // BSD gives `-P` precedence over `-h` and prints 1K blocks, so
        // the figure is converted here.
        (
            "Filesystem    1024-blocks      Used Available Capacity Mounted on\n\
             /dev/disk1s5s1  976490576  21453456 345673920       6% /",
            "330G 6%",
            false,
        ),
        // A device name with a space in it, and a filesystem with no
        // size worth printing. Both in one line, because macOS prints
        // them together.
        (
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
             map auto_home 0 0 0 100% /System/Volumes/Data/home",
            "df —",
            false,
        ),
        // Under ten gets a decimal place, as `df -h` does.
        (
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
             /dev/sdb1 20971520 15728640 5242880 75% /data",
            "5.0G 75%",
            false,
        ),
        // Not a `df` at all.
        ("not a df", "df ?", false),
    ];
    for (recorded, expected, bold) in cases {
        let dir = stubs(&format!("df-{}", expected.replace([' ', '%', '/'], "_")));
        stub(&dir, "df", &format!("printf '%s\\n' {}", shell_quoted(recorded)));
        let Some(mut p) = with_exec(DISK, &["df"], session()) else { return };
        let runs = on_path(&dir, || {
            p.on_event(&Event::Started).expect("started");
            p.render().expect("render")
        });
        assert_eq!(drawn(&runs), expected, "from {recorded:?}");
        assert_eq!(runs[0].style.bold, bold, "from {recorded:?}");
    }
}

// -------------------------------------------------------------- docker

#[test]
fn docker_status_refused_says_which_program_it_wanted() {
    let _lock = one_at_a_time();
    let Some(mut p) = plain_load(DOCKER, session()) else { return };
    // Nothing until it has asked: this segment is absent far more often
    // than it is present, so the default is to draw nothing.
    assert!(p.render().expect("render").is_empty());

    p.on_event(&Event::Started).expect("started");
    let shown = drawn(&p.render().expect("render"));
    assert!(shown.contains("not granted"), "expected a refusal, got {shown:?}");
    assert!(shown.contains("docker"), "the refusal should name the program: {shown:?}");
}

#[test]
fn docker_status_counts_what_this_machine_is_running() {
    let _lock = one_at_a_time();
    if !installed("docker") && !installed("podman") {
        return;
    }
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    p.on_event(&Event::Started).expect("started");
    let shown = drawn(&p.render().expect("render"));
    // Either engine answered with a count, or neither daemon would talk
    // and the segment is correctly empty. Both are passes: whether the
    // daemon is up is not something a test can insist on.
    if shown.is_empty() {
        return;
    }
    let (engine, count) =
        shown.split_once(' ').unwrap_or_else(|| panic!("expected `engine n`, got {shown:?}"));
    assert!(matches!(engine, "docker" | "podman"), "{shown:?}");
    assert!(count.parse::<usize>().is_ok(), "{shown:?}");
}

#[cfg(unix)]
#[test]
fn docker_status_tells_apart_absent_idle_and_busy() {
    let _lock = one_at_a_time();
    // Three containers.
    let dir = stubs("docker-three");
    stub(&dir, "docker", "printf 'a\\nb\\nc\\n'");
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    let runs = on_path(&dir, || {
        p.on_event(&Event::Started).expect("started");
        p.render().expect("render")
    });
    assert_eq!(drawn(&runs), "docker 3");
    assert!(!runs[0].style.dim, "a count worth reading is not dim");

    // Installed, answering, nothing running. Worth four characters,
    // because it says the daemon is alive — but dim.
    let dir = stubs("docker-idle");
    stub(&dir, "docker", "true");
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    let runs = on_path(&dir, || {
        p.on_event(&Event::Started).expect("started");
        p.render().expect("render")
    });
    assert_eq!(drawn(&runs), "docker 0");
    assert!(runs[0].style.dim, "{runs:?}");

    // A daemon that will not answer: nothing at all, rather than a
    // status bar that reads "cannot connect to the Docker daemon" for
    // the rest of the day.
    let dir = stubs("docker-wedged");
    stub(&dir, "docker", "echo 'Cannot connect to the Docker daemon' >&2; exit 1");
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    let runs = on_path(&dir, || {
        p.on_event(&Event::Started).expect("started");
        p.render().expect("render")
    });
    assert!(runs.is_empty(), "expected nothing, got {:?}", drawn(&runs));

    // No docker on the machine at all, and podman instead. The engine's
    // own name is shown, since "docker" would be the wrong word here.
    let dir = stubs("podman-two");
    stub(&dir, "podman", "printf 'one\\ntwo\\n'");
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    let runs = on_path(&dir, || {
        p.on_event(&Event::Started).expect("started");
        p.render().expect("render")
    });
    assert_eq!(drawn(&runs), "podman 2");

    // Neither engine installed: the segment is indistinguishable from
    // not having the plugin, which is the point.
    let dir = stubs("no-engine");
    let Some(mut p) = with_exec(DOCKER, &["docker", "podman"], session()) else { return };
    let runs = on_path(&dir, || {
        p.on_event(&Event::Started).expect("started");
        p.render().expect("render")
    });
    assert!(runs.is_empty(), "expected nothing, got {:?}", drawn(&runs));
}

// ------------------------------------------------------- recorded output

/// A fresh, empty directory to put stub programs in.
#[cfg(unix)]
fn stubs(what: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("haven-plugins-system-{}", std::process::id()))
        .join(what);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create a stub directory");
    dir
}

/// A program on `PATH` that prints recorded output.
///
/// This is the only way into the parser from outside the component: the
/// plugin asks for `uptime` by name and `run` allowlists `argv[0]`, so
/// there is nothing to inject except the program itself.
#[cfg(unix)]
fn stub(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write a stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod a stub");
}

/// Run `body` with `dir` as the whole of `PATH`, so only the stubs in it
/// can be found. Restored afterwards, and every test in this file holds
/// the same lock, so no other test sees it.
#[cfg(unix)]
fn on_path<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let was = std::env::var_os("PATH");
    std::env::set_var("PATH", dir);
    let out = body();
    match was {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    out
}

/// A string as a single `sh` word, for putting recorded output inside a
/// stub without having to think about what is in it.
#[cfg(unix)]
fn shell_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
