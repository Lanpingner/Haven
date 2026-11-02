//! `haven attach`: the TUI client against a daemon.
//!
//! The same `Client` loop the tests drive, with a real terminal on one side
//! and a socket on the other (`docs/PLAN.md` §3).

use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
fn key_filter() -> impl FnMut(&KeyEvent, &mut Mode) -> Intercepted {
    move |event: &KeyEvent, mode: &mut Mode| {
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
            Mode::Command { text, suggestions } => {
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
                return out;
            }
            Mode::Prefix => {
                *mode = Mode::Normal;
                return match keys::prefix_table(event) {
                    Some(Action::EnterCommand) => {
                        *mode = Mode::command(String::new());
                        Intercepted::Consumed
                    }
                    Some(Action::Choose) => Intercepted::OpenChooser,
                    Some(Action::Detach) => Intercepted::Detach,
                    Some(Action::SendPrefix) => Intercepted::Pass,
                    Some(action) => match keys::to_proto(action) {
                        Some(wire) => Intercepted::Act(wire),
                        None => Intercepted::Consumed,
                    },
                    // An unbound key after the prefix does nothing rather
                    // than leaking into the pane.
                    None => Intercepted::Consumed,
                };
            }
            Mode::Normal => {}
        }
        if keys::is_prefix(event) {
            *mode = Mode::Prefix;
            return Intercepted::Consumed;
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

    fn ctrl_a() -> KeyEvent {
        key(Key::Char('a'), Modifiers::CTRL)
    }

    #[test]
    fn the_prefix_arms_and_a_bound_key_acts() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;

        assert_eq!(filter(&ctrl_a(), &mut mode), Intercepted::Consumed);
        assert_eq!(mode, Mode::Prefix, "the status line would show nothing");

        let act = filter(&key(Key::Char('%'), Modifiers::empty()), &mut mode);
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
        filter(&ctrl_a(), &mut mode);
        assert_eq!(
            filter(&release(Key::Char('a'), Modifiers::CTRL), &mut mode),
            Intercepted::Consumed
        );
        assert_eq!(mode, Mode::Prefix, "the release disarmed the prefix");

        assert_eq!(
            filter(&key(Key::Char('%'), Modifiers::SHIFT), &mut mode),
            Intercepted::Act(Wire::SplitColumns)
        );
    }

    #[test]
    fn a_release_does_not_type_into_a_command() {
        let mut filter = key_filter();
        let mut mode = Mode::command(String::new());
        filter(&key(Key::Char('z'), Modifiers::empty()), &mut mode);
        filter(&release(Key::Char('z'), Modifiers::empty()), &mut mode);
        assert_eq!(mode.command_text(), Some("z"), "the release typed a second character");
    }

    #[test]
    fn an_ordinary_key_reaches_the_pane() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        assert_eq!(filter(&key(Key::Char('x'), Modifiers::empty()), &mut mode), Intercepted::Pass);
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn the_prefix_twice_sends_a_literal_one() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode);
        assert_eq!(filter(&ctrl_a(), &mut mode), Intercepted::Pass);
    }

    #[test]
    fn a_typed_command_runs_and_matches_its_binding() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode);
        assert_eq!(
            filter(&key(Key::Char(':'), Modifiers::empty()), &mut mode),
            Intercepted::Consumed
        );
        assert_eq!(mode.command_text(), Some(""));

        for c in "split-v".chars() {
            filter(&key(Key::Char(c), Modifiers::empty()), &mut mode);
        }
        assert_eq!(mode.command_text(), Some("split-v"), "the line was not collected");

        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode);
        // The same action the `%` binding produces: one vocabulary, two ways in.
        assert_eq!(out, Intercepted::Act(Wire::SplitColumns));
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn escape_abandons_a_command_and_backspace_backs_out_of_an_empty_one() {
        let mut filter = key_filter();
        let mut mode = Mode::command("zoo".into());
        filter(&key(Key::Named(NamedKey::Backspace), Modifiers::empty()), &mut mode);
        assert_eq!(mode.command_text(), Some("zo"));

        mode = Mode::command("x".into());
        filter(&key(Key::Named(NamedKey::Escape), Modifiers::empty()), &mut mode);
        assert_eq!(mode, Mode::Normal);

        mode = Mode::command(String::new());
        filter(&key(Key::Named(NamedKey::Backspace), Modifiers::empty()), &mut mode);
        assert_eq!(mode, Mode::Normal, "backspace on an empty line should leave the mode");
    }

    #[test]
    fn an_unknown_command_is_swallowed_rather_than_typed_into_the_pane() {
        let mut filter = key_filter();
        let mut mode = Mode::command("nonsense".into());
        let out = filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode);
        assert_eq!(out, Intercepted::Consumed);
        assert_eq!(mode, Mode::Normal);
    }
}
