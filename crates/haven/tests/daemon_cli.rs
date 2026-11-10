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
        Self::start_env(tag, &[])
    }

    /// A daemon with extra environment. The plugin browser reads the
    /// repositories from the *daemon's* disk — over SSH they are on the
    /// far machine — so a test of it has to point the daemon's XDG
    /// directories somewhere it controls.
    fn start_env(tag: &str, env: &[(&str, &Path)]) -> Self {
        let socket =
            std::env::temp_dir().join(format!("haven-test-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let mut command = Command::new(HAVEN);
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
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

/// The plugin browser, drawn by a real client attached to a real daemon.
///
/// The repositories belong to the daemon: over SSH they are on the far
/// machine, and a browser showing the client's own would be showing the
/// wrong thing with no way to tell. So this points the daemon's XDG
/// directories at a temporary home, adds the fixture repository there
/// through the ordinary CLI, and then presses the key.
#[test]
fn the_plugin_browser_shows_the_daemons_repositories() {
    let home = std::env::temp_dir().join(format!("haven-browser-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let config = home.join("config");
    let data = home.join("data");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(&data).expect("data");

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../haven-plugin/tests/fixtures");
    let added = Command::new(HAVEN)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .args(["plugin", "repo", "add"])
        .arg(&fixture)
        .args(["--key", "BA205FB38B5FBA0FFFB59CC1E1E1F35F2B2ED307", "--name", "demo"])
        .output()
        .expect("repo add");
    assert!(
        added.status.success(),
        "repo add failed: {}{}",
        String::from_utf8_lossy(&added.stdout),
        String::from_utf8_lossy(&added.stderr)
    );
    // Install one, so the browser has an installed plugin to mark.
    let installed = Command::new(HAVEN)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .args(["plugin", "install", "gitline", "--yes"])
        .output()
        .expect("install");
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));

    let server = Server::start_env(
        "browser",
        &[("XDG_CONFIG_HOME", config.as_path()), ("XDG_DATA_HOME", data.as_path())],
    );
    server.wait_for_listing(|listing| !listing.trim().is_empty());

    // Ctrl+A, then `u` to open it, then `urls` typed into it. Typing is
    // the realistic thing to do in the round trip before the listing
    // arrives, and it lands the selection on a plugin — the box opens on
    // the repository here, because nothing has an update waiting.
    let screen = server.attach("\x01uurls", 100, 24);

    assert!(screen.contains("plugins"), "the browser never opened:\n{screen}");
    assert!(screen.contains("demo"), "the repository is missing:\n{screen}");
    assert!(screen.contains("urls"), "the plugin is missing:\n{screen}");
    // The panel is about the selected plugin, and says what it asks for
    // — the thing worth knowing before trusting it. `urls` is the one
    // that wants to read your panes.
    assert!(screen.contains("asks to"), "the panel is not about the plugin:\n{screen}");
    // Asserted on a fragment that cannot be split: the panel is narrow
    // and this line wraps in it.
    assert!(screen.contains("read what your"), "what it asks for is missing:\n{screen}");
    assert!(screen.contains("from demo"), "which repository it came from:\n{screen}");
    assert!(screen.contains("not installed"), "its state is missing:\n{screen}");
    // And the way out, plus the command that acts on the selection,
    // because a modal with no exit and no next step is a trap.
    assert!(screen.contains("esc"), "no hint:\n{screen}");
    assert!(screen.contains("haven plugin"), "no command to act on it:\n{screen}");

    let _ = std::fs::remove_dir_all(&home);
}

/// An update that moves a version, and the browser showing both the
/// update waiting and what the refresh found.
///
/// `plugins-moved.yaml` in the fixtures is the same index with `gitline`
/// at 0.3.0, signed by the same key, so this needs no `gpg` at test time.
#[test]
fn the_browser_shows_an_update_and_what_the_last_refresh_changed() {
    let home = std::env::temp_dir().join(format!("haven-browser-moved-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let config = home.join("config");
    let data = home.join("data");
    let mirror = home.join("mirror");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(&data).expect("data");

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../haven-plugin/tests/fixtures");
    copy_tree(&fixture, &mirror);

    let plugin = |args: &[&str]| {
        let out = Command::new(HAVEN)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data)
            .args(["plugin"])
            .args(args)
            .output()
            .expect("run plugin cli");
        assert!(
            out.status.success(),
            "haven plugin {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    plugin(&[
        "repo",
        "add",
        mirror.to_str().unwrap(),
        "--key",
        "BA205FB38B5FBA0FFFB59CC1E1E1F35F2B2ED307",
        "--name",
        "demo",
    ]);
    plugin(&["install", "gitline", "--yes"]);

    // The repository publishes a new version of what is installed.
    std::fs::copy(mirror.join("plugins-moved.yaml"), mirror.join("plugins.yaml")).unwrap();
    std::fs::copy(mirror.join("plugins-moved.yaml.sig"), mirror.join("plugins.yaml.sig")).unwrap();
    let updated = plugin(&["repo", "update"]);
    assert!(updated.contains("demo"), "{updated}");

    let server = Server::start_env(
        "browser-moved",
        &[("XDG_CONFIG_HOME", config.as_path()), ("XDG_DATA_HOME", data.as_path())],
    );
    server.wait_for_listing(|listing| !listing.trim().is_empty());

    // Opens on the plugin with the update, because that is the question
    // the browser exists to answer.
    let screen = server.attach("\x01u", 100, 24);
    assert!(screen.contains("0.2.1 → 0.3.0"), "no update shown:\n{screen}");
    assert!(screen.contains("installed: 0.2.1"), "the panel is not on the plugin:\n{screen}");
    assert!(
        screen.contains("haven plugin install gitline"),
        "no command to take the update:\n{screen}"
    );

    // And typing the repository's name selects it, where the change log
    // the refresh wrote is shown.
    let screen = server.attach("\x01udemo ", 100, 24);
    assert!(screen.contains("recent changes"), "no change log:\n{screen}");
    assert!(screen.contains("gitline"), "the change log is empty:\n{screen}");

    let _ = std::fs::remove_dir_all(&home);
}

/// Copy a directory tree, for a mirror a test can tamper with without
/// writing to the fixtures.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}
