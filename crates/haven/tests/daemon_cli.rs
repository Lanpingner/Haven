//! The daemon through its real CLI: start a server, attach a TUI client over a
//! socket, lose the client, reattach and find the shell exactly as it was.
//!
//! This is M5's acceptance criterion from `docs/PLAN.md` §16 — "`pkill -9
//! haven` loses no shell state" — run end to end with two processes and a real
//! unix socket.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use haven::haven_pty::{PtyConfig, PtySize};
use haven::pump::{run_to_screen, PumpLimits};

const HAVEN: &str = env!("CARGO_BIN_EXE_haven");

/// A daemon that is killed when the test ends, whatever happens.
struct Server {
    child: Child,
    socket: PathBuf,
}

impl Server {
    fn start(tag: &str) -> Self {
        let socket =
            std::env::temp_dir().join(format!("haven-test-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let child = Command::new(HAVEN)
            .args(["server", "run", "--socket"])
            .arg(&socket)
            .args(["--no-integrate", "--", "/bin/sh", "-i"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start daemon");

        // Wait for the socket rather than sleeping a fixed amount.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(socket.exists(), "daemon never created its socket");
        Self { child, socket }
    }

    /// Poll `ls` until it says what we are waiting for, or give up.
    fn wait_for_listing(&self, mut done: impl FnMut(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            last = self.cli(&["ls"]);
            if done(&last) {
                return last;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        last
    }

    fn cli(&self, args: &[&str]) -> String {
        let out = Command::new(HAVEN)
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .output()
            .expect("run cli");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Attach a client inside a pty we own, type at it, and return the screen.
    fn attach(&self, send: &str, cols: u16, rows: u16) -> String {
        let cfg = PtyConfig {
            program: Some(Path::new(HAVEN).to_path_buf()),
            args: vec!["attach".into(), "--socket".into(), self.socket.clone().into()],
            env: vec![("TERM".into(), "xterm-256color".into())],
            env_remove: vec!["LINES".into(), "COLUMNS".into()],
            size: PtySize { cols, rows, ..Default::default() },
            ..Default::default()
        };
        let limits =
            PumpLimits { idle: Duration::from_millis(2000), total: Duration::from_secs(25) };
        let (emu, _, _) = run_to_screen(&cfg, limits, send.as_bytes()).expect("attach");
        emu.dump()
    }

    /// As [`Self::attach`], but keeps the emulator so a test can look at the
    /// styles the client actually wrote, not just the characters.
    fn attach_emu(&self, send: &str, cols: u16, rows: u16) -> haven::haven_core::Emulator {
        let cfg = PtyConfig {
            program: Some(Path::new(HAVEN).to_path_buf()),
            args: vec!["attach".into(), "--socket".into(), self.socket.clone().into()],
            env: vec![("TERM".into(), "xterm-256color".into())],
            env_remove: vec!["LINES".into(), "COLUMNS".into()],
            size: PtySize { cols, rows, ..Default::default() },
            ..Default::default()
        };
        let limits =
            PumpLimits { idle: Duration::from_millis(2000), total: Duration::from_secs(25) };
        let (emu, _, _) = run_to_screen(&cfg, limits, send.as_bytes()).expect("attach");
        emu
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[test]
fn a_session_outlives_the_client_that_created_it() {
    let server = Server::start("persist");
    assert!(server.cli(&["ls"]).contains("no sessions"), "a fresh daemon has no sessions");

    let first = server.attach("echo PERSISTED\n", 60, 8);
    assert!(first.contains("PERSISTED"), "the shell never ran the command:\n{first}");

    // The client is gone, but the session and its shell are not. Reaping is
    // noticed on the daemon's next tick, so give it one.
    let listing = server.wait_for_listing(|l| l.contains("0 clients"));
    assert!(listing.contains("1 panes"), "session vanished with its client: {listing:?}");
    assert!(listing.contains("0 clients"), "client was not reaped: {listing:?}");

    // Reattaching shows the same screen, scrollback and all.
    let second = server.attach("", 60, 8);
    assert!(
        second.contains("PERSISTED"),
        "reattach lost the screen:\nfirst:\n{first}\nsecond:\n{second}"
    );
}

#[test]
fn splits_made_over_the_socket_are_visible_to_the_next_client() {
    let server = Server::start("splits");
    // Ctrl+A % splits; each pane is its own shell on the daemon's side.
    let screen = server.attach("echo LEFTSIDE\n\x01%echo RIGHTSIDE\n", 70, 10);
    assert!(screen.contains("LEFTSIDE") && screen.contains("RIGHTSIDE"), "{screen}");

    let listing = server.cli(&["ls"]);
    assert!(listing.contains("2 panes"), "the daemon forgot the split: {listing:?}");

    // A second client sees the layout the first one built.
    let again = server.attach("", 70, 10);
    assert!(again.contains('│'), "reattach lost the split:\n{again}");
    assert!(
        again.contains("LEFTSIDE") && again.contains("RIGHTSIDE"),
        "reattach lost pane content:\n{again}"
    );
}

#[test]
fn killing_a_session_ends_it() {
    let server = Server::start("kill");
    server.attach("echo TRANSIENT\n", 50, 6);
    assert!(server.cli(&["ls"]).contains("haven-1"));

    let left = server.cli(&["kill-session", "-t", "1"]);
    assert!(left.contains("0 session"), "kill-session did not report: {left:?}");
    assert!(server.cli(&["ls"]).contains("no sessions"), "the session survived being killed");
}

#[test]
fn attaching_starts_a_daemon_when_none_is_running() {
    // No server started here: `attach` has to bring one up by itself.
    let socket = std::env::temp_dir().join(format!("haven-autostart-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);

    let cfg = PtyConfig {
        program: Some(Path::new(HAVEN).to_path_buf()),
        args: vec![
            "attach".into(),
            "--socket".into(),
            socket.clone().into(),
            "--no-integrate".into(),
        ],
        env: vec![("TERM".into(), "xterm-256color".into())],
        size: PtySize { cols: 50, rows: 6, ..Default::default() },
        ..Default::default()
    };
    let limits = PumpLimits { idle: Duration::from_millis(2500), total: Duration::from_secs(25) };
    let (emu, _, _) = run_to_screen(&cfg, limits, b"echo AUTOSTARTED\n").expect("attach");
    let screen = emu.dump();

    assert!(socket.exists(), "attach did not start a daemon");
    assert!(screen.contains("AUTOSTARTED"), "autostarted daemon gave no shell:\n{screen}");

    // Clean up the daemon we implicitly started.
    let _ = Command::new(HAVEN).args(["server", "stop", "--socket"]).arg(&socket).status();
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn xtmodkeys_does_not_underline_the_session() {
    // `CSI > 4 ; 1 m` is modifyOtherKeys. fish sends it before every prompt.
    // Parsed as `SGR 4;1` it sets underline on the pen and nothing resets it,
    // so every cell erased afterwards is drawn with a rule through it.
    //
    // This drives the whole daemon path — pty, socket, client, serializer —
    // rather than the direct render path, because that is where it was seen.
    let server = Server::start("xtmodkeys");
    let emu = server.attach_emu("printf '\\033[>4;1mUNDERTEST\\n'\n", 60, 8);

    let underlined: Vec<(usize, usize)> = emu
        .term
        .grid
        .rows()
        .iter()
        .enumerate()
        .flat_map(|(r, row)| {
            row.cells.iter().enumerate().filter_map(move |(c, cell)| Some((r, c)).zip(Some(cell)))
        })
        .filter(|((_, _), cell)| {
            emu.term.styles.get(cell.style).underline != haven::haven_core::Underline::None
        })
        .map(|(pos, _)| pos)
        .collect();

    assert!(
        underlined.is_empty(),
        "XTMODKEYS underlined {} cells (first at {:?}); screen:\n{}",
        underlined.len(),
        underlined.first(),
        emu.dump()
    );
}
