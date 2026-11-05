//! `haven attach`: the TUI client against a daemon.
//!
//! The same `Client` loop the tests drive, with a real terminal on one side
//! and a socket on the other (`docs/PLAN.md` §3).

use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use haven_client::bindings::{self, Bindings};
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
                        // Through `act`, not `run_command`: a typed line is
                        // a bound command that happens to have been typed,
                        // and the two commands that change the client's own
                        // mode were the two a typed line could not run. That
                        // made most of the command palette inert — its
                        // entries are `command-prompt <verb> ` strings, so
                        // picking one asked for a prompt and got silence.
                        return act(&line, mode, bindings);
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
            Mode::Table(name) => {
                let name = name.clone();
                // A table of your own stays armed; `prefix` is one-shot, as
                // it has always been.
                if !bindings.sticky(&name) {
                    *mode = Mode::Normal;
                }
                // `_or_root` because a root binding means "this acts with
                // no prefix", and an armed table blocking it is surprising:
                // pressing the prefix and then changing your mind must not
                // disable `Alt+C`.
                return match bindings.command_or_root(&name, event) {
                    Some(command) => act(command, mode, bindings),
                    // An unbound key in an armed table does nothing rather
                    // than leaking into the pane. Escape leaves a sticky
                    // table, or there would be no way out of a table that
                    // forgot to bind one.
                    None => {
                        if matches!(event.key, Key::Named(NamedKey::Escape)) {
                            *mode = Mode::Normal;
                            return Intercepted::Consumed;
                        }
                        // Say so. Swallowing it silently is what made a
                        // stray prefix look like a broken binding.
                        Intercepted::Notice(format!(
                            "{} is not bound in {name}",
                            bindings::label(event)
                        ))
                    }
                };
            }
            Mode::Normal => {}
        }
        if bindings.is_prefix(event) {
            *mode = Mode::prefix();
            return Intercepted::Consumed;
        }
        if let Some(command) = bindings.command(bindings::ROOT, event) {
            return act(command, mode, bindings);
        }
        Intercepted::Pass
    }
}

/// Run a bound command, including the two that change the client's own mode
/// rather than doing something to a session.
fn act(command: &str, mode: &mut Mode, bindings: &Bindings) -> Intercepted {
    match keys::parse_command(command) {
        // `command-prompt` may arrive with text to start the line off with.
        Ok(Action::EnterCommand(prefill)) => {
            *mode = Mode::command(prefill);
            Intercepted::Consumed
        }
        Ok(Action::SwitchTable(name)) => {
            // A name no table answers to would arm something that swallows
            // every key and answers none, so a typo goes nowhere instead.
            *mode = match name.as_str() {
                bindings::ROOT | "" => Mode::Normal,
                other if bindings.has(other) => Mode::Table(other.to_string()),
                _ => Mode::Normal,
            };
            Intercepted::Consumed
        }
        _ => run_command(command),
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
        Ok(Action::DisplayPanes) => Intercepted::DisplayPanes,
        Ok(Action::ChooseShell(verb)) => Intercepted::ChooseShell(verb),
        Ok(Action::ContainerShell { verb, spec }) => Intercepted::ChooseLifetime { verb, spec },
        Ok(Action::ContainerMounts { add, remove }) => Intercepted::ContainerMounts { add, remove },
        Ok(Action::Font(step)) => Intercepted::Zoom(step),
        // Reached only when a caller has no mode to change — `act` handles
        // these, and everything with a mode goes through `act`.
        Ok(Action::SwitchTable(_)) => Intercepted::Consumed,
        Ok(Action::CommandPalette) => Intercepted::OpenMenu {
            title: "command".into(),
            // Built here because this is where the vocabulary lives. The
            // client has no idea what a command is, which is what keeps the
            // menu from being able to offer one the command line cannot run.
            entries: keys::COMMANDS
                .iter()
                .map(|c| ((*c).to_string(), keys::palette_entry(c)))
                .collect(),
            filterable: true,
        },
        Ok(Action::FindWindow(text)) => Intercepted::FindWindow(text),
        Ok(Action::Paste) => Intercepted::Paste,
        // A literal prefix key goes to the pane, which is what `send-prefix`
        // means and the only way to drive a nested session.
        Ok(Action::SendPrefix) => Intercepted::Pass,
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
    config_path: std::path::PathBuf,
) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    // The event loop blocks until something happens; without a descriptor to
    // watch, it would never notice the daemon. Duplicating it lets the
    // watcher thread wait on the same socket without owning the transport.
    let wake = rustix::io::dup(transport.as_fd()).ok();
    // The same table the TUI client uses: a binding that works in one and
    // not the other would be a trap, and this one was.
    let filter: haven_client::KeyFilter = Box::new(key_filter());
    // Zooming writes the size back, so the next window opens where this one
    // was left. A failure is said out loud and not fatal: a config file that
    // cannot be written is a reason to lose the preference, not the session.
    let saver: Box<dyn FnMut(f32)> = Box::new(move |px| {
        if let Err(e) = haven_daemon::config::write_font_size(&config_path, px) {
            eprintln!("haven: could not remember the font size: {e}");
        }
    });
    haven_render::window::run(transport, session, config, wake, Some(filter), Some(saver))
        .map_err(|e| io::Error::other(e.to_string()))
}

/// As [`run`], but asking the host terminal what it can do first.
///
/// Separate so tests and `haven dump` can hand in known capabilities rather
/// than writing escape sequences at whatever is reading them.
pub fn run_probed(socket: &Path, session: Option<SessionId>) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    let frontend = TuiFrontend::enter_probed()?;
    drive(transport, frontend, session)
}

pub fn run(socket: &Path, session: Option<SessionId>, host: HostCaps) -> io::Result<()> {
    let transport = connect_or_start(socket)?;
    let frontend = TuiFrontend::enter(host)?;
    drive(transport, frontend, session)
}

fn drive(
    transport: SocketTransport,
    frontend: TuiFrontend,
    session: Option<SessionId>,
) -> io::Result<()> {
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

/// Save or restore from the command line, for a login script or a cron job.
///
/// Returns what the daemon reported, so a script can tell whether it worked
/// rather than assuming.
pub fn save_or_restore(socket: &Path, restore: bool) -> io::Result<(u16, String)> {
    // Deliberately *not* attached first. Attaching with no session named
    // calls `first_or_create`, so asking a fresh daemon to restore would
    // make an empty session called `haven-1` and then skip restoring the
    // saved one because a session of that name was already there.
    let mut transport = SocketTransport::connect(socket)?;
    let ask = if restore { ClientMsg::RestoreSessions } else { ClientMsg::SaveSessions };
    transport.send(ask).map_err(to_io)?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match transport.try_recv() {
            Ok(Some(haven_proto::ServerMsg::Saved { sessions, path, problem })) => {
                return match problem {
                    Some(reason) => Err(io::Error::other(reason)),
                    None => Ok((sessions, path)),
                };
            }
            Ok(Some(_)) => {}
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(to_io(e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from the daemon"))
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
        assert_eq!(mode, Mode::prefix(), "the status line would show nothing");

        let act = filter(&key(Key::Char('%'), Modifiers::empty()), &mut mode, &keys());
        assert_eq!(act, Intercepted::Act(Wire::SplitColumns { program: None }));
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
        assert_eq!(mode, Mode::prefix(), "the release disarmed the prefix");

        assert_eq!(
            filter(&key(Key::Char('%'), Modifiers::SHIFT), &mut mode, &keys()),
            Intercepted::Act(Wire::SplitColumns { program: None })
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
            Intercepted::Act(Wire::NewWindow { program: None })
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
            bindings::PREFIX,
            &[("c".into(), "split-v".into()), ("%".into(), String::new())],
        );
        let mut filter = key_filter();
        let mut mode = Mode::Normal;

        // The old prefix is now an ordinary key.
        assert_eq!(filter(&ctrl_a(), &mut mode, &bindings), Intercepted::Pass);
        assert_eq!(
            filter(&key(Key::Char('b'), Modifiers::CTRL), &mut mode, &bindings),
            Intercepted::Consumed
        );
        assert_eq!(mode, Mode::prefix());
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::empty()), &mut mode, &bindings),
            Intercepted::Act(Wire::SplitColumns { program: None })
        );

        // And an unbound key does not reach the pane — but it says so,
        // rather than being swallowed in silence.
        filter(&key(Key::Char('b'), Modifiers::CTRL), &mut mode, &bindings);
        match filter(&key(Key::Char('%'), Modifiers::SHIFT), &mut mode, &bindings) {
            Intercepted::Notice(text) => assert!(text.contains('%'), "{text:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_root_binding_acts_without_the_prefix() {
        let bindings =
            Bindings::default().with(None, bindings::ROOT, &[("C-t".into(), "new-window".into())]);
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        assert_eq!(
            filter(&key(Key::Char('t'), Modifiers::CTRL), &mut mode, &bindings),
            Intercepted::Act(Wire::NewWindow { program: None })
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
    fn alt_c_offers_a_shell_without_the_prefix() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::ALT), &mut mode, &keys()),
            Intercepted::ChooseShell("new-window".into())
        );
        assert_eq!(mode, Mode::Normal, "a root binding must not arm anything");
        // Without alt it is an ordinary key and belongs to the pane.
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Pass
        );
    }

    #[test]
    fn offering_a_shell_defaults_to_a_tab_like_every_other_c() {
        // `c` is the new tab, so the key that offers a shell makes one too.
        // It used to split, which meant one `c` meant two different things.
        assert_eq!(
            keys::parse_command("choose-shell"),
            Ok(keys::Action::ChooseShell("new-window".into()))
        );
        assert_eq!(
            keys::parse_command("choose-shell split-v"),
            Ok(keys::Action::ChooseShell("split-v".into()))
        );
    }

    /// A key table named `copy`, so `switch-table copy` has something to
    /// arm: a name no table answers to is deliberately refused.
    fn copy_table() -> Bindings {
        keys().with(None, "copy", &[("q".into(), "copy-mode".into())])
    }

    #[test]
    fn a_typed_line_can_run_the_commands_that_change_the_clients_own_mode() {
        // These two were unreachable from the command line and from the
        // palette, which is most of what the palette is: its entries for
        // commands that take an argument are `command-prompt <verb> `
        // strings, so picking one asked for a prompt and got nothing.
        let mut filter = key_filter();
        let mut mode = Mode::command("command-prompt rename-window ".into());
        assert_eq!(
            filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys()),
            Intercepted::Consumed
        );
        assert_eq!(
            mode.command_text(),
            Some("rename-window "),
            "picking it from the palette should leave a prompt with the verb on it"
        );

        let mut mode = Mode::command("switch-table copy".into());
        filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &copy_table());
        assert_eq!(mode.table(), Some("copy"), "a typed switch-table armed nothing");
    }

    #[test]
    fn every_palette_entry_does_something_when_it_is_picked() {
        // The palette is built from `COMMANDS`, so a command that resolves
        // to nothing is an entry that looks like a feature and is not one.
        for name in keys::COMMANDS {
            let entry = keys::palette_entry(name);
            let mut mode = Mode::command(entry.clone());
            let mut filter = key_filter();
            let verdict =
                filter(&key(Key::Named(NamedKey::Enter), Modifiers::empty()), &mut mode, &keys());
            let opened_a_prompt = mode.command_text().is_some();
            assert!(
                verdict != Intercepted::Consumed || opened_a_prompt || mode.table().is_some(),
                "picking {name:?} ran {entry:?} and nothing happened"
            );
        }
    }

    #[test]
    fn the_zoom_keys_are_the_ones_every_other_terminal_uses() {
        let b = keys();
        for (c, want) in [
            ('=', haven_client::Zoom::In),
            ('+', haven_client::Zoom::In),
            ('-', haven_client::Zoom::Out),
            ('0', haven_client::Zoom::Reset),
        ] {
            let mut filter = key_filter();
            let mut mode = Mode::Normal;
            assert_eq!(
                filter(&key(Key::Char(c), Modifiers::CTRL), &mut mode, &b),
                Intercepted::Zoom(want),
                "Ctrl+{c}"
            );
            assert_eq!(mode, Mode::Normal, "a root binding must not arm anything");
        }
    }

    #[test]
    fn zooming_is_not_the_pane_zoom_the_prefix_does() {
        // One word, two meanings, so the commands are spelled as fonts.
        assert_eq!(keys::parse_command("zoom"), Ok(keys::Action::ToggleZoom));
        assert_eq!(
            keys::parse_command("font-bigger"),
            Ok(keys::Action::Font(haven_client::Zoom::In))
        );
        assert_eq!(
            keys::parse_command("zoom-out"),
            Ok(keys::Action::Font(haven_client::Zoom::Out))
        );
        assert_eq!(run_command("font-reset"), Intercepted::Zoom(haven_client::Zoom::Reset));
    }

    #[test]
    fn a_container_from_the_shell_menu_asks_whether_it_lasts() {
        // Picking a distro is two questions, and the second one is the one
        // that matters a week later: did what I install in here survive.
        assert_eq!(
            run_command("container-shell new-window docker://run/archlinux:latest"),
            Intercepted::ChooseLifetime {
                verb: "new-window".into(),
                spec: "docker://run/archlinux:latest".into()
            }
        );
    }

    #[test]
    fn the_answer_to_that_question_is_an_ordinary_new_window() {
        // What the second menu produces is the plain command with the whole
        // spec as the program, so nothing past this point knows a container
        // was involved — not the protocol, not the mux, not the save file.
        assert_eq!(
            run_command("new-window docker://run/archlinux:latest?keep"),
            Intercepted::Act(Wire::NewWindow {
                program: Some("docker://run/archlinux:latest?keep".into())
            })
        );
        assert_eq!(
            run_command("split-v docker://run/alpine:latest?scratch&mount=cwd"),
            Intercepted::Act(Wire::SplitColumns {
                program: Some("docker://run/alpine:latest?scratch&mount=cwd".into())
            })
        );
    }

    #[test]
    fn a_running_container_is_opened_without_being_asked_about() {
        // It already exists, so there is no lifetime to choose: it outlives
        // the pane whatever the pane does.
        assert_eq!(
            run_command("new-window docker://exec/web"),
            Intercepted::Act(Wire::NewWindow { program: Some("docker://exec/web".into()) })
        );
    }

    #[test]
    fn mounts_are_added_dropped_and_asked_about() {
        assert_eq!(
            run_command("container-mount /Projects"),
            Intercepted::ContainerMounts { add: Some("/Projects".into()), remove: None }
        );
        // `cwd` is the word for the directory the pane was opened in,
        // resolved where the pane is spawned rather than where it was typed.
        assert_eq!(
            run_command("mount cwd"),
            Intercepted::ContainerMounts { add: Some("cwd".into()), remove: None }
        );
        assert_eq!(
            run_command("container-unmount /Projects"),
            Intercepted::ContainerMounts { add: None, remove: Some("/Projects".into()) }
        );
        assert_eq!(
            run_command("container-mounts"),
            Intercepted::ContainerMounts { add: None, remove: None }
        );
    }

    #[test]
    fn a_mount_with_no_directory_is_an_error_rather_than_a_silent_nothing() {
        assert!(keys::parse_command("container-mount").is_err());
        assert!(keys::parse_command("container-unmount").is_err());
        assert!(keys::parse_command("container-shell new-window").is_err());
    }

    #[test]
    fn a_menu_entry_resolves_to_a_split_that_names_its_program() {
        // The menu holds command strings and nothing else, so this is the
        // only place that decides what one means.
        assert_eq!(
            run_command("split-v /usr/bin/fish"),
            Intercepted::Act(Wire::SplitColumns { program: Some("/usr/bin/fish".into()) })
        );
    }

    #[test]
    fn a_table_of_your_own_stays_armed_until_something_leaves_it() {
        // The modal part: `prefix` is one-shot, a table of your own is not,
        // so a run of resizes needs the prefix once rather than each time.
        let bindings = Bindings::default()
            .with(None, "resize", &[("h".into(), "resize-left".into())])
            .with(None, bindings::PREFIX, &[("R".into(), "switch-table resize".into())]);
        let mut filter = key_filter();
        let mut mode = Mode::Normal;

        filter(&ctrl_a(), &mut mode, &bindings);
        assert_eq!(
            filter(&key(Key::Char('R'), Modifiers::SHIFT), &mut mode, &bindings),
            Intercepted::Consumed
        );
        assert_eq!(mode.table(), Some("resize"));

        for _ in 0..3 {
            assert_eq!(
                filter(&key(Key::Char('h'), Modifiers::empty()), &mut mode, &bindings),
                Intercepted::Act(Wire::ResizeLeft)
            );
            assert_eq!(mode.table(), Some("resize"), "a sticky table must not drop itself");
        }

        // Escape always leaves, even if the table forgot to bind a way out.
        filter(&key(Key::Named(NamedKey::Escape), Modifiers::empty()), &mut mode, &bindings);
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn switching_to_a_table_that_does_not_exist_arms_nothing() {
        // Otherwise a typo arms something that swallows every key and
        // answers none of them.
        let bindings = Bindings::default().with(
            None,
            bindings::PREFIX,
            &[("R".into(), "switch-table nosuch".into())],
        );
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &bindings);
        filter(&key(Key::Char('R'), Modifiers::SHIFT), &mut mode, &bindings);
        assert_eq!(mode, Mode::Normal);
    }

    #[test]
    fn the_palette_offers_every_command_and_nothing_else() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        match filter(&key(Key::Char('P'), Modifiers::SHIFT), &mut mode, &keys()) {
            Intercepted::OpenMenu { entries, filterable, .. } => {
                assert!(filterable, "a list this long is only usable by typing");
                assert_eq!(entries.len(), keys::COMMANDS.len());
                // Every entry resolves, because the menu runs it through
                // exactly the same parser a typed line goes through. The
                // ones that need an argument open the command line with the
                // verb already on it rather than running a bare verb that
                // would do nothing or the wrong thing.
                for (label, command) in &entries {
                    assert!(
                        keys::parse_command(command).is_ok(),
                        "{label:?} offers {command:?}, which does not parse"
                    );
                }
                let prompted: Vec<&String> = entries
                    .iter()
                    .filter(|(_, c)| c.starts_with("command-prompt "))
                    .map(|(l, _)| l)
                    .collect();
                assert!(
                    prompted.contains(&&"select-window".to_string()),
                    "select-window needs a number: {prompted:?}"
                );
                assert!(prompted.contains(&&"rename-window".to_string()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_root_binding_still_fires_with_the_prefix_armed() {
        // The one that cost an evening: `Ctrl+A` and then a change of mind
        // left the prefix armed, so `Alt+C` was swallowed by the prefix
        // table and did nothing, silently.
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        assert_eq!(mode.table(), Some("prefix"));
        assert_eq!(
            filter(&key(Key::Char('c'), Modifiers::ALT), &mut mode, &keys()),
            Intercepted::ChooseShell("new-window".into())
        );
        assert_eq!(mode, Mode::Normal, "the prefix should be spent either way");
    }

    #[test]
    fn an_unbound_key_after_the_prefix_names_itself() {
        let mut filter = key_filter();
        let mut mode = Mode::Normal;
        filter(&ctrl_a(), &mut mode, &keys());
        match filter(&key(Key::Char('~'), Modifiers::empty()), &mut mode, &keys()) {
            Intercepted::Notice(text) => {
                assert!(text.contains('~'), "it should name the key: {text:?}");
                assert!(text.contains("prefix"), "and the table: {text:?}");
            }
            other => panic!("silently swallowed: {other:?}"),
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
        assert_eq!(out, Intercepted::Act(Wire::SplitColumns { program: None }));
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
        assert_eq!(out, Intercepted::Act(Wire::SplitColumns { program: None }));
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
