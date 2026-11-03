//! `haven attach`: the TUI client against a daemon.
//!
//! The same `Client` loop the tests drive, with a real terminal on one side
//! and a socket on the other (`docs/PLAN.md` §3).

use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use haven_client::bindings::{Bindings, Table};
use haven_client::input::{Key, KeyEvent, KeyState, Modifiers, NamedKey};
use haven_client::status_bar::Mode;
use haven_client::{Client, Intercepted, Tick};
use haven_proto::transport::SocketTransport;
use haven_proto::{ClientMsg, SessionId, Transport};
use haven_tui::{HostCaps, TuiFrontend};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::keys::{self, Action};

/// Connect, starting a daemon if there is not one already.
///
/// Autostart rather than an error: a terminal that refuses to open because no
/// server is running would be a strange thing to ship.
pub fn connect_or_start(socket: &Path) -> io::Result<SocketTransport> {
    if let Ok(transport) = SocketTransport::connect(socket) {
        return Ok(transport);
    }
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("server")
        .arg("run")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // Wait for the socket to answer rather than sleeping a fixed amount.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(transport) = SocketTransport::connect(socket) {
            return Ok(transport);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "daemon did not start"))
}

/// The prefix key table, as a filter the client consults per keystroke.
/// The client's key handling: a prefix table, and a command line behind `:`.
///
/// The mode is the client's, not this closure's, because the status line
/// shows it. Everything resolves to the same [`keys::Action`], so a typed
/// command and a bound key cannot drift apart.
fn key_filter() -> impl FnMut(&KeyEvent, &mut Mode, &Bindings) -> Intercepted {
    move |event: &KeyEvent, mode: &mut Mode, bindings: &Bindings| {
        // Only presses drive the mode machine. A terminal transmits nothing
        // on key-up — the encoder drops releases outright — but the GPU
        // window delivers them, and letting them through meant the release
        // of Ctrl+A matched the prefix table and disarmed the prefix a
        // millisecond after the press armed it. In command mode it would
        // also have typed every character twice. The TUI frontend has no
        // key-up at all, which is why this only broke in the window.
        if event.state == KeyState::Release {
            return Intercepted::Consumed;
        }
        match mode {
            Mode::Command { text, suggestions, selected } => {
                // The arrows walk the completion box and put the chosen entry
                // on the command line, so what Enter will run is always what
                // is written there. They deliberately do not recompute the
                // list: narrowing it to the one entry just selected would
                // close the box after a single press.
                let step = match &event.key {
                    Key::Named(NamedKey::Down) => 1,
                    Key::Named(NamedKey::Up) => -1,
                    _ => 0,
                };
                if step != 0 && !suggestions.is_empty() {
                    let len = suggestions.len() as i32;
                    *selected = (((*selected as i32 + step) % len + len) % len) as usize;
                    *text = suggestions[*selected].clone();
                    return Intercepted::Consumed;
                }
                let out = match &event.key {
                    Key::Named(NamedKey::Escape) => {
                        *mode = Mode::Normal;
                        return Intercepted::Consumed;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if text.pop().is_none() {
                            *mode = Mode::Normal;
                            return Intercepted::Consumed;
                        }
                        Intercepted::Consumed
                    }
                    Key::Named(NamedKey::Enter) => {
                        let line = text.clone();
                        *mode = Mode::Normal;
                        return run_command(&line);
                    }
                    Key::Named(NamedKey::Tab) => {
                        // Complete as far as every candidate agrees, which is
                        // as far as can be done without choosing for the user.
                        let candidates = keys::complete(text);
                        let common = keys::common_prefix(&candidates);
                        if common.len() > text.len() {
                            *text = common;
                        } else if candidates.len() == 1 {
                            *text = candidates[0].to_string();
                        }
                        Intercepted::Consumed
                    }
                    Key::Char(c) if !event.mods.contains(Modifiers::CTRL) => {
                        text.push(*c);
                        Intercepted::Consumed
                    }
                    _ => Intercepted::Consumed,
                };
                *suggestions = keys::complete(text).into_iter().map(|s| s.to_string()).collect();
                // Typing moves the goalposts: a selection into the old list
                // would point at whatever happens to sit at that index now.
                *selected = 0;
                return out;
            }
            Mode::Prefix => {
                *mode = Mode::Normal;
                return match bindings.command(Table::Prefix, event) {
                    // `command-prompt` is the one binding that opens a mode
                    // rather than doing something, and it may arrive with
                    // text to start the line off with.
                    Some(command) => match keys::parse_command(command) {
                        Ok(Action::EnterCommand(prefill)) => {
                            *mode = Mode::command(prefill);
                            Intercepted::Consumed
                        }
                        _ => run_command(command),
                    },
                    // An unbound key after the prefix does nothing rather
                    // than leaking into the pane.
                    None => Intercepted::Consumed,
                };
            }
            Mode::Normal => {}
        }
        if bindings.is_prefix(event) {
            *mode = Mode::Prefix;
            return Intercepted::Consumed;
        }
        if let Some(command) = bindings.command(Table::Root, event) {
            return run_command(command);
        }
        Intercepted::Pass
    }
}

/// Execute a typed command line.
///
/// An empty line is a no-op rather than an error: pressing Enter on nothing
/// is how people cancel.
fn run_command(line: &str) -> Intercepted {
    if line.trim().is_empty() {
        return Intercepted::Consumed;
    }
    match keys::parse_command(line.trim()) {
        Ok(Action::Detach) => Intercepted::Detach,
        Ok(Action::Choose) => Intercepted::OpenChooser,
        Ok(Action::NewSession) => Intercepted::NewSession,
        Ok(Action::Scroll(rows)) => Intercepted::Scroll(rows),
        Ok(Action::Copy) => Intercepted::EnterCopy,
        Ok(Action::Paste) => Intercepted::Paste,
        // A literal prefix key goes to the pane, which is what `send-prefix`
        // means and the only way to drive a nested session.
        Ok(Action::SendPrefix) => Intercepted::Pass,
        // Opening a prompt from a prompt would be a loop with nothing in it.
        Ok(Action::EnterCommand(_)) => Intercepted::Consumed,
        Ok(action) => match keys::to_proto(action) {
            Some(wire) => Intercepted::Act(wire),
            None => Intercepted::Consumed,
        },
        Err(_) => Intercepted::Consumed,
    }
}

/// Attach with the GPU frontend: a window instead of the host terminal.
///
/// Same client loop, same key table, same daemon — only the `Frontend` impl
/// differs, which is the whole point of the seam (`docs/PLAN.md` §3).
pub fn run_gpu(
    socket: &Path,
    session: Option<SessionId>,
    config: haven_render::WindowConfig,
) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    // The event loop blocks until something happens; without a descriptor to
    // watch, it would never notice the daemon. Duplicating it lets the
    // watcher thread wait on the same socket without owning the transport.
    let wake = rustix::io::dup(transport.as_fd()).ok();
    // The same table the TUI client uses: a binding that works in one and
    // not the other would be a trap, and this one was.
    let filter: haven_client::KeyFilter = Box::new(key_filter());
    haven_render::window::run(transport, session, config, wake, Some(filter))
        .map_err(|e| io::Error::other(e.to_string()))
}

pub fn run(socket: &Path, session: Option<SessionId>, host: HostCaps) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    let frontend = TuiFrontend::enter(host)?;
    let mut client = Client::new(frontend, transport);
    client.key_filter = Some(Box::new(key_filter()));
    client.attach(session).map_err(to_io)?;

    let stdin = rustix::stdio::stdin();
    loop {
        // Block on either the terminal or the daemon, so an idle client uses
        // no CPU and wakes with no timer.
        {
            let socket_fd = client.transport.as_fd();
            let input = stdin.as_fd();
            let mut fds =
                [PollFd::new(&socket_fd, PollFlags::IN), PollFd::new(&input, PollFlags::IN)];
            let ts = Timespec { tv_sec: 0, tv_nsec: 50_000_000 };
            match poll(&mut fds, Some(&ts)) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        match client.tick() {
            Ok(Tick::Finished) => break,
            Ok(Tick::Idle | Tick::Redraw) => {}
            // Recorded rather than returned: the terminal has to be handed
            // back before anything is printed, or the message lands in the
            // middle of the alternate screen and is lost with it.
            Err(e) => {
                client.note_error(&e);
                break;
            }
        }
    }

    // Dropping the client restores the terminal; only then is there anywhere
    // sensible to print.
    let reason = client.fatal().map(str::to_string);
    drop(client);
    match reason {
        Some(reason) => Err(io::Error::other(reason)),
        None => Ok(()),
    }
}

/// Ask the daemon for its session list.
/// The sessions a daemon is holding, and when that daemon was built.
pub fn list(socket: &Path) -> io::Result<(Vec<haven_proto::SessionInfo>, u64)> {
    let mut transport = match SocketTransport::connect(socket) {
        Ok(t) => t,
        // No socket means no sessions, which is an answer rather than a failure.
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            return Ok((Vec::new(), 0))
        }
        Err(e) => return Err(e),
    };
    transport.send(ClientMsg::ListSessions).map_err(to_io)?;
    wait_for_sessions(&mut transport)
}

pub fn kill_session(
    socket: &Path,
    id: SessionId,
) -> io::Result<(Vec<haven_proto::SessionInfo>, u64)> {
    let mut transport = SocketTransport::connect(socket)?;
    transport.send(ClientMsg::KillSession(id)).map_err(to_io)?;
    wait_for_sessions(&mut transport)
}

/// Run one command against a session without attaching.
///
/// This is the whole of control mode for now, and it is what makes Haven
/// scriptable at all: anything a key can do, a shell script can do, because
/// both resolve to the same [`crate::keys::Action`].
pub fn run_command_on(socket: &Path, session: Option<SessionId>, line: &str) -> io::Result<()> {
    let action = crate::keys::parse_command(line.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let Some(wire) = crate::keys::to_proto(action) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{line:?} only means something inside a client"),
        ));
    };
    let mut transport = SocketTransport::connect(socket)?;
    transport
        .send(ClientMsg::Attach {
            session,
            caps: haven_proto::ClientCaps::null(haven_proto::CellSize::default()),
            // A script is not a viewer: it must not resize the session or
            // take over its environment on the way past.
            env: Vec::new(),
        })
        .map_err(to_io)?;
    transport.send(ClientMsg::Action(wire)).map_err(to_io)?;
    // The daemon acts on reads, so give it one before dropping the socket.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

/// The session's environment, as `export` lines a shell can evaluate.
///
/// The only way to fix a shell that was started before the latest attach:
/// its environment was set at exec and nothing can reach in and change it.
pub fn show_environment(socket: &Path) -> io::Result<Vec<(String, String)>> {
    let mut transport = SocketTransport::connect(socket)?;
    transport
        .send(ClientMsg::Attach {
            session: None,
            caps: haven_proto::ClientCaps::null(haven_proto::CellSize::default()),
            // Asking must not *change* what it is asking about.
            env: Vec::new(),
        })
        .map_err(to_io)?;
    transport.send(ClientMsg::ShowEnvironment).map_err(to_io)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match transport.try_recv() {
            Ok(Some(haven_proto::ServerMsg::Environment(env))) => return Ok(env),
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(to_io(e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from the daemon"))
}

pub fn shutdown(socket: &Path) -> io::Result<()> {
    let mut transport = match SocketTransport::connect(socket) {
        Ok(t) => t,
        Err(_) => return Ok(()), // Already stopped.
    };
    transport.send(ClientMsg::Shutdown).map_err(to_io)?;
    // Give the daemon a moment to act before we drop the socket.
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn wait_for_sessions(
    transport: &mut SocketTransport,
) -> io::Result<(Vec<haven_proto::SessionInfo>, u64)> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match transport.try_recv() {
            Ok(Some(haven_proto::ServerMsg::Sessions { sessions, daemon_build })) => {
                return Ok((sessions, daemon_build))
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(to_io(e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from the daemon"))
}

fn to_io(e: haven_proto::TransportError) -> io::Error {
    io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_proto::Action as Wire;

    fn key(k: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent { key: k, mods, state: KeyState::Press }
    }

    fn release(k: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent { key: k, mods, state: KeyState::Release }
    }

    /// The default table, which is what the tests are about: a binding that
    /// behaved differently from the one the config produces would be two
    /// implementations of the same thing.
    fn keys() -> Bindings {
        Bindings::default()
    }

    fn ctrl_a() -> KeyEvent {
        key(Key::Char('a'), Modifiers::CTRL)
    }

    #[test]
    fn the_prefix_arms_and_a_bound_key_acts() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;

        assert_eq!(filter(&ctrl_a(), &mut mode, &keys()), Intercepted::Consumed);
        assert_eq!(mode, Mode::Prefix, "the status line would show nothing");

        let act = filter(&key(Key::Char('%'), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(act, Intercepted::Act(Wire::SplitColumns));
        assert_eq!(mode, Mode::Normal, "the prefix stayed armed");
    }

    #[test]
    fn a_key_release_does_not_disarm_the_prefix() {
        // The GPU window delivers key-up; the TUI cannot. Without this the
        // release of Ctrl+A matched the prefix table and cancelled the
        // prefix immediately, so no binding in the window did anything.
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&release(Key::Char('a'), Modifiers::CTRL), &mut mode, &keys()),
            Intercepted::Consumed
        );
        assert_eq!(mode, Mode::Prefix, "the release disarmed the prefix");

        assert_eq!(
            filter(&key(Key::Char('%'), Modifiers::SHIFT), &mut mode, &keys()),
            Intercepted::Act(Wire::SplitColumns)
        );
    }

    #[test]
    fn a_release_does_not_type_into_a_command() {
        let mut filter = key_filter();
        let mut mode = Mode::command(String::new());
        filter(&key(Key::Char('z'), Modifiers::empty()), &mut mode, &keys());
        filter(&release(Key::Char('z'), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(mode.command_text(), Some("z"), "the release typed a second character");
    }

    #[test]
    fn capital_c_asks_for_a_session_and_lowercase_c_for_a_window() {
        // The one that is easy to get wrong: `c` and `C` differ by a shift,
        // and binding both to the same thing would be silent.
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&key(Key::Char('C'), Modifiers::SHIFT), &mut mode, &keys()),
            Intercepted::NewSession
        );

        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Act(Wire::NewWindow)
        );
    }

    #[test]
    fn new_session_is_typeable_as_well_as_bound() {
        let mut filter = key_filter();
        let mut mode = Mode::command("new-session".into());
        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(out, Intercepted::NewSession, "the typed form must match the binding");
    }

    #[test]
    fn the_prefix_pages_through_the_scrollback() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&key(Key::Named(NamedKey::PageUp), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Scroll(keys::PAGE_ROWS)
        );
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&key(Key::Named(NamedKey::End), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Scroll(i32::MIN),
            "End should go back to the live screen whatever the history"
        );
    }

    #[test]
    fn a_configured_binding_replaces_the_built_in_one() {
        // The point of the table: `c` is the new tab by default, and a config
        // that says otherwise must win without a code change.
        let bindings = Bindings::default().with(
            Some("C-b"),
            &[("c".into(), "split-v".into()), ("%".into(), String::new())],
            false,
        );
        let mut filter = key_filter();
        let mut mode = Mode::Normal;

        // The old prefix is now an ordinary key.
        assert_eq!(filter(&ctrl_a(), &mut mode, &bindings), Intercepted::Pass);
        assert_eq!(
            filter(&key(Key::Char('b'), Modifiers::CTRL), &mut mode, &bindings),
            Intercepted::Consumed
        );
        assert_eq!(mode, Mode::Prefix);
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::empty()), &mut mode, &bindings),
            Intercepted::Act(Wire::SplitColumns)
        );

        // And an unbound key does nothing rather than reaching the pane.
        filter(&key(Key::Char('b'), Modifiers::CTRL), &mut mode, &bindings);
        assert_eq!(
            filter(&key(Key::Char('%'), Modifiers::SHIFT), &mut mode, &bindings),
            Intercepted::Consumed
        );
    }

    #[test]
    fn a_root_binding_acts_without_the_prefix() {
        let bindings = Bindings::default().with(None, &[("C-t".into(), "new-window".into())], true);
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        assert_eq!(
            filter(&key(Key::Char('t'), Modifiers::CTRL), &mut mode, &bindings),
            Intercepted::Act(Wire::NewWindow)
        );
        assert_eq!(mode, Mode::Normal, "a root binding must not arm anything");
    }

    #[test]
    fn every_default_binding_names_a_command_that_exists() {
        // The table is data, so a typo in it is a key that silently does
        // nothing instead of a compile error.
        use haven_client::bindings::DEFAULT_BINDINGS;
        for (spec, command) in DEFAULT_BINDINGS {
            assert!(
                keys::parse_command(command).is_ok(),
                "{spec:?} is bound to {command:?}, which is not a command"
            );
        }
    }

    #[test]
    fn an_ordinary_key_reaches_the_pane() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        assert_eq!(
            filter(&key(Key::Char('x'), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Pass
        );
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn the_prefix_twice_sends_a_literal_one() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(filter(&ctrl_a(), &mut mode, &keys()), Intercepted::Pass);
    }

    #[test]
    fn a_typed_command_runs_and_matches_its_binding() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(
            filter(&key(Key::Char(':'), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Consumed
        );
        assert_eq!(mode.command_text(), Some(""));

        for c in "split-v".chars() {
            filter(&key(Key::Char(c), Modifiers::empty()), &mut mode, &keys());
        }
        assert_eq!(mode.command_text(), Some("split-v"), "the line was not collected");

        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys());
        // The same action the `%` binding produces: one vocabulary, two ways in.
        assert_eq!(out, Intercepted::Act(Wire::SplitColumns));
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn the_arrows_walk_the_completions_and_put_one_on_the_line() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        filter(&key(Key::Char(':'), Modifiers::empty()), &mut mode, &keys());
        filter(&key(Key::Char('s'), Modifiers::empty()), &mut mode, &keys());

        let listed = match &mode {
            Mode::Command { suggestions, .. } => suggestions.clone(),
            other => panic!("{other:?}"),
        };
        assert!(listed.len() > 2, "not enough candidates to walk: {listed:?}");

        let down = key(Key::Named(NamedKey::Down), Modifiers::empty());
        filter(&down, &mut mode, &keys());
        assert_eq!(mode.command_text(), Some(listed[1].as_str()), "Down did not take the next one");
        filter(&down, &mut mode, &keys());
        assert_eq!(mode.command_text(), Some(listed[2].as_str()));

        // The list must not narrow as the selection moves, or the box would
        // close after one press.
        match &mode {
            Mode::Command { suggestions, selected, .. } => {
                assert_eq!(suggestions, &listed, "the list was recomputed under the selection");
                assert_eq!(*selected, 2);
            }
            other => panic!("{other:?}"),
        }

        // Up wraps back round the top.
        let up = key(Key::Named(NamedKey::Up), Modifiers::empty());
        filter(&up, &mut mode, &keys());
        filter(&up, &mut mode, &keys());
        filter(&up, &mut mode, &keys());
        assert_eq!(mode.command_text(), Some(listed[listed.len() - 1].as_str()), "Up did not wrap");
    }

    #[test]
    fn enter_runs_what_the_arrows_chose() {
        let mut filter = key_filter();
        let mut mode = Mode::command(String::new());
        for c in "split-".chars() {
            filter(&key(Key::Char(c), Modifiers::empty()), &mut mode, &keys());
        }
        // split-h and split-v, in that order.
        filter(&key(Key::Named(NamedKey::Down), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(mode.command_text(), Some("split-v"));
        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(out, Intercepted::Act(Wire::SplitColumns));
    }

    #[test]
    fn typing_after_arrowing_starts_the_selection_again() {
        // Otherwise the old index points at whatever happens to sit there in
        // the new, narrower list.
        let mut filter = key_filter();
        let mut mode = Mode::command(String::new());
        filter(&key(Key::Char('s'), Modifiers::empty()), &mut mode, &keys());
        filter(&key(Key::Named(NamedKey::Down), Modifiers::empty()), &mut mode, &keys());
        filter(&key(Key::Char('x'), Modifiers::empty()), &mut mode, &keys());
        match &mode {
            Mode::Command { selected, .. } => assert_eq!(*selected, 0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn escape_abandons_a_command_and_backspace_backs_out_of_an_empty_one() {
        let mut filter = key_filter();
        let mut mode = Mode::command("zoo".into());
        filter(&key(Key::Named(NamedKey::Backspace), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(mode.command_text(), Some("zo"));

        mode = Mode::command("x".into());
        filter(&key(Key::Named(NamedKey::Escape), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(mode, Mode::Normal);

        mode = Mode::command(String::new());
        filter(&key(Key::Named(NamedKey::Backspace), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(mode, Mode::Normal, "backspace on an empty line should leave the mode");
    }

    #[test]
    fn an_unknown_command_is_swallowed_rather_than_typed_into_the_pane() {
        let mut filter = key_filter();
        let mut mode = Mode::command("nonsense".into());
        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(out, Intercepted::Consumed);
        assert_eq!(mode, Mode::Normal);
    }
}
