//! The tests that need a real Docker, a real pty and a real container.
//!
//! Ignored by default: a unit test may not assume a container runtime, a
//! network or a few hundred megabytes of disk. They are here because the
//! interesting failures in this backend are not ones a pure test can see —
//! one of them (a killed pane leaking its container) was found by running
//! this and nothing else.
//!
//! ```sh
//! cargo test -p haven-backend --test live -- --ignored
//! ```

use std::io::{Read, Write};

/// Open a container pane for real, and wait until a shell answers in it.
fn open(spec: &str) -> (haven_pty::Pty, String) {
    let dir = std::env::temp_dir().join("haven-backend-live");
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = haven_backend::Context {
        settings: haven_backend::Settings::default(),
        cwd: Some(dir.clone()),
        dir,
    };
    let cfg = haven_backend::launch(spec, &ctx).expect("a spec should launch");
    let mut pty = haven_pty::Pty::spawn(&cfg).expect("spawn");
    // The marker is split so that the echo of the line we typed cannot be
    // mistaken for the answer to it. It was, and the test passed vacuously.
    pty.write_all(b". /etc/os-release 2>/dev/null; echo MA\"RK\"ER-$ID; pwd\n").unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    // Generous: the first run of an image may have to fetch it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline && !seen.contains("MARKER-") {
        match pty.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(_) => break,
        }
    }
    assert!(seen.contains("MARKER-"), "no shell answered in the container: {seen}");
    (pty, seen)
}

/// The pids every scratch container is labelled with. Identified by pid
/// rather than counted, because these tests run beside each other and a
/// total would be whatever the other one happened to be doing.
fn scratch_pids() -> Vec<String> {
    let out = std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=haven.scratch",
            "--format",
            "{{.Label \"haven.pane\"}}",
        ])
        .output()
        .expect("docker");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
#[ignore = "needs docker and the network"]
fn a_container_pane_is_a_shell_in_that_distro_with_the_directory_it_was_given() {
    let (pty, seen) = open("docker://run/alpine:latest?scratch&mount=cwd");
    assert!(seen.contains("MARKER-alpine"), "the shell is not alpine's: {seen}");
    let dir = std::env::temp_dir().join("haven-backend-live");
    assert!(
        seen.contains(dir.to_str().unwrap()),
        "the mounted directory should be the working directory: {seen}"
    );
    drop(pty);
}

#[test]
#[ignore = "needs docker and the network"]
fn a_killed_pane_leaves_a_container_that_the_next_pane_throws_away() {
    // `--rm` covers the pane closed by typing `exit`. This is the other
    // case, and the one that leaked: the CLI is killed, so nothing tells the
    // container anything — `SIGHUP` reaches a PID 1 that the kernel will not
    // deliver it to.
    //
    let (first, _) = open("docker://run/alpine:latest?scratch");
    // The container's label is the pid of the CLI the pane runs, which is
    // the pane's own process: `exec` kept the pid.
    let mine = first.pid().to_string();
    assert!(scratch_pids().contains(&mine), "the pane's container should be running");

    drop(first);
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(
        scratch_pids().contains(&mine),
        "a killed pane does leave its container behind, which is the point of the reaper"
    );

    // Opening the next container pane is where that is noticed and undone.
    let (second, _) = open("docker://run/alpine:latest?scratch");
    assert!(!scratch_pids().contains(&mine), "the orphan survived the reaper");
    let theirs = second.pid().to_string();
    assert!(scratch_pids().contains(&theirs), "the live pane's container was reaped too");

    drop(second);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &format!("haven-alpine-latest-scratch-{theirs}")])
        .output();
}
