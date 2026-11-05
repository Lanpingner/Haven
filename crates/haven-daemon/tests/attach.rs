//! Attach, detach, and two clients on one session — over a real socket, with
//! the real client stack on the other end. See `docs/PLAN.md` §16 (M5).

use std::io;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use haven_client::frontend::Frontend;
use haven_client::{Client, NullFrontend};
use haven_daemon::{Daemon, ResizePolicy};
use haven_mux::PaneRequest;
use haven_proto::transport::SocketTransport;
use haven_proto::{Action, CellSize, ClientMsg, PaneId, SessionId, Transport, TransportError};
use haven_pty::{Pty, PtyConfig};

type TestClient = Client<NullFrontend, SocketTransport>;

fn size(cols: u16, rows: u16) -> CellSize {
    CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
}

/// Panes running a quiet shell that echoes what it is told.
fn spawner(
    _env: haven_daemon::SessionEnv,
    _containers: haven_daemon::ContainerSettings,
) -> Box<dyn FnMut(PaneRequest) -> io::Result<Pty>> {
    Box::new(|request| {
        let size = request.size;
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "while read l; do echo \"got:$l\"; done".into()],
            size,
            ..Default::default()
        })
    })
}

/// Panes whose shell reports what it runs, the way a shell with integration
/// does. Typing the escape directly cannot work: the tty echoes control
/// characters in caret notation, so no real `ESC` ever reaches the emulator.
fn marking_spawner(
    _env: haven_daemon::SessionEnv,
    _containers: haven_daemon::ContainerSettings,
) -> Box<dyn FnMut(PaneRequest) -> io::Result<Pty>> {
    Box::new(|request| {
        let size = request.size;
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                // Doubled: Rust must pass the shell a literal backslash,
                // and `\033` in a Rust string is a NUL followed by "33".
                "while read l; do printf '\\033]133;C;cmdline_url=%s\\007' \"$l\"; done".into(),
            ],
            size,
            ..Default::default()
        })
    })
}

fn daemon() -> Daemon {
    Daemon::detached(spawner)
}

fn attach(daemon: &mut Daemon, cols: u16, rows: u16) -> TestClient {
    attach_to(daemon, cols, rows, None)
}

fn attach_to(daemon: &mut Daemon, cols: u16, rows: u16, session: Option<SessionId>) -> TestClient {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    daemon.accept_stream(theirs).expect("adopt");
    let transport = SocketTransport::from_stream(ours).expect("transport");
    let mut client = Client::new(NullFrontend::new(size(cols, rows)), transport);
    client.attach(session).expect("attach");
    client
}

/// Pump both sides until a predicate holds.
fn settle<F>(daemon: &mut Daemon, clients: &mut [&mut TestClient], mut done: F) -> bool
where
    F: FnMut(&[&mut TestClient]) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        for client in clients.iter_mut() {
            let _ = client.tick();
        }
        if done(clients) {
            return true;
        }
    }
    false
}

/// Which session a client is on, as the daemon reports it to the status bar.
/// The pane ids restart per session, so they cannot tell the two apart.
fn session_name(client: &TestClient) -> Option<String> {
    client.frame.status.as_ref().map(|s| s.name.clone())
}

fn send_input(client: &mut TestClient, pane: PaneId, bytes: &[u8]) {
    client.transport.send(ClientMsg::Input { pane, bytes: bytes.to_vec() }).expect("send input");
}

fn send_action(client: &mut TestClient, action: Action) -> Result<(), TransportError> {
    client.transport.send(ClientMsg::Action(action))
}

#[test]
fn attaching_delivers_a_layout_and_a_snapshot() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);

    assert!(settle(&mut daemon, &mut [&mut client], |c| {
        !c[0].frame.panes.is_empty() && c[0].focus.is_some()
    }));
    assert_eq!(daemon.session_names().len(), 1, "attaching with no session creates one");
    let pane = client.focus.expect("focus");
    let frame = client.frame.pane(pane).expect("pane in frame");
    assert_eq!(frame.rect.cols, 40, "the pane should fill the client");
    // One row fewer than the client's screen: the client keeps the bottom
    // row for its status line and never offers it to the daemon.
    assert_eq!(frame.rows.len(), 7, "the status line's row was given to a pane");
}

#[test]
fn typing_reaches_the_pane_and_the_output_comes_back() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());

    let pane = client.focus.unwrap();
    send_input(&mut client, pane, b"hello\n");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("got:hello")
        }),
        "pane never echoed:\n{}",
        client.frame.pane_text(pane)
    );
}

#[test]
fn two_clients_on_one_session_see_the_same_screen() {
    let mut daemon = daemon();
    let mut first = attach(&mut daemon, 40, 8);
    let mut second = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut first, &mut second], |c| {
        c[0].focus.is_some() && c[1].focus.is_some()
    });
    assert_eq!(daemon.client_count(), 2);

    let pane = first.focus.unwrap();
    send_input(&mut first, pane, b"shared\n");

    assert!(
        settle(&mut daemon, &mut [&mut first, &mut second], |c| {
            c[0].frame.pane_text(pane).contains("got:shared")
                && c[1].frame.pane_text(pane).contains("got:shared")
        }),
        "clients diverged:\nfirst:\n{}\nsecond:\n{}",
        first.frame.pane_text(pane),
        second.frame.pane_text(pane),
    );
}

#[test]
fn a_client_that_disappears_leaves_the_session_running() {
    let mut daemon = daemon();
    let pane;
    {
        let mut client = attach(&mut daemon, 40, 8);
        settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
        pane = client.focus.unwrap();
        send_input(&mut client, pane, b"survives\n");
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("got:survives")
        });
        // The client drops here, as if its terminal had been closed.
    }
    daemon.tick(Duration::from_millis(10)).unwrap();
    assert_eq!(daemon.client_count(), 0, "the client should be reaped");
    assert_eq!(daemon.session_names().len(), 1, "the session must outlive its client");

    // Reattaching gets the state back, including what was there before.
    let mut again = attach(&mut daemon, 40, 8);
    assert!(
        settle(&mut daemon, &mut [&mut again], |c| {
            c[0].frame.pane_text(pane).contains("got:survives")
        }),
        "reattach lost the screen:\n{}",
        again.frame.pane_text(pane)
    );
}

#[test]
fn an_action_from_a_client_splits_the_session() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());

    send_action(&mut client, Action::SplitColumns { program: None }).unwrap();
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.panes.len() == 2),
        "split never arrived: {} panes",
        client.frame.panes.len()
    );
    // The two panes tile the client's width, with a separator column between.
    let widths: u16 = client.frame.panes.values().map(|p| p.rect.cols).sum();
    assert_eq!(widths, 39);
}

#[test]
fn the_smallest_client_decides_the_size() {
    let mut daemon = daemon();
    daemon.set_policy(ResizePolicy::Smallest);
    let mut big = attach(&mut daemon, 100, 30);
    settle(&mut daemon, &mut [&mut big], |c| c[0].focus.is_some());
    let mut small = attach(&mut daemon, 50, 12);

    assert!(
        settle(&mut daemon, &mut [&mut big, &mut small], |c| {
            c[0].frame.panes.values().all(|p| p.rect.cols == 50)
                && c[1].frame.panes.values().all(|p| p.rect.cols == 50)
        }),
        "panes did not shrink to the smaller client"
    );
}

#[test]
fn asking_for_a_session_that_does_not_exist_says_so() {
    let mut daemon = daemon();
    let mut client = attach_to(&mut daemon, 40, 8, Some(SessionId(99)));

    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.notice.is_some()),
        "no refusal arrived"
    );
    assert!(client.frame.notice.as_deref().unwrap().contains("no such session"));
    assert!(daemon.session_names().is_empty(), "a bad attach must not create a session");
}

#[test]
fn a_new_session_is_made_beside_the_one_that_exists() {
    // `Attach { session: None }` joins the first session and only creates
    // one when there are none, so until this message there was no way to
    // have two sessions on one daemon at all.
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut client], |c| session_name(c[0]).is_some());
    assert_eq!(session_name(&client).as_deref(), Some("haven-1"));
    assert_eq!(daemon.session_names().len(), 1);

    client.new_session(None).expect("ask for a session");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            session_name(c[0]).as_deref() == Some("haven-2")
        }),
        "the client never landed in the new session; sessions={:?} on={:?}",
        daemon.session_names(),
        session_name(&client)
    );
    assert_eq!(daemon.session_names().len(), 2, "the old session was replaced, not kept");
    // And the new one is live, not an empty shell.
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"hello\n");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("got:hello")
        }),
        "the new session's pane is not running anything"
    );
}

#[test]
fn a_new_session_can_be_named() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut client], |c| session_name(c[0]).is_some());

    client.new_session(Some("work".into())).expect("ask for a session");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            session_name(c[0]).as_deref() == Some("work")
        }),
        "the name was dropped: {:?}",
        daemon.session_names()
    );
}

/// A pane that prints numbered lines on demand, so history has content a
/// test can recognise by eye.
fn counting_spawner(
    _env: haven_daemon::SessionEnv,
    _containers: haven_daemon::ContainerSettings,
) -> Box<dyn FnMut(PaneRequest) -> io::Result<Pty>> {
    Box::new(|request| {
        let size = request.size;
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "while read n; do i=1; while [ $i -le $n ]; do echo \"line$i\";                  i=$((i+1)); done; done"
                    .into(),
            ],
            size,
            ..Default::default()
        })
    })
}

#[test]
fn scrolling_back_shows_rows_that_left_the_screen() {
    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");

    // Twenty lines through a five-row pane: most of them are history.
    send_input(&mut client, pane, b"20\n");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("line20")
        }),
        "the pane never caught up:\n{}",
        client.frame.pane_text(pane)
    );
    assert!(!client.frame.pane_text(pane).contains("line1\n"), "nothing scrolled off");

    // Eight rows back from a five-row view: the oldest line on screen was
    // line16, and line9 was gone.
    client.transport.send(ClientMsg::Scroll { pane, delta: 8 }).expect("scroll");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("line9")
        }),
        "scrolling back showed nothing older:\n{}",
        client.frame.pane_text(pane)
    );

    // And the bar is told, so it can say the screen is not live.
    assert_eq!(client.frame.scroll().map(|(o, _)| o), Some(8));
    assert!(!client.frame.cursor_visible(), "a cursor was drawn below the view");

    // Back to the bottom, however far it is asked to go.
    client.transport.send(ClientMsg::Scroll { pane, delta: i32::MIN }).expect("scroll");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("line20")
        }),
        "could not get back to the live screen"
    );
    assert_eq!(client.frame.scroll(), None, "still reported as scrolled");
    assert!(client.frame.cursor_visible(), "the cursor never came back");
}

#[test]
fn scrolling_cannot_go_past_the_oldest_row() {
    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"20\n");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.pane_text(pane).contains("line20"));

    client.transport.send(ClientMsg::Scroll { pane, delta: i32::MAX }).expect("scroll");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.scroll().is_some());
    let (offset, total) = client.frame.scroll().expect("scrolled");
    assert_eq!(offset, total, "the top of the history is as far as it goes");
    let text = client.frame.pane_text(pane);
    assert!(text.contains("line1\n") || text.contains("$"), "not at the top:\n{text}");
}

#[test]
fn typing_returns_to_the_live_screen() {
    // Reading history and then pressing a key means you are done reading;
    // watching your own keystrokes land off-screen would be worse than
    // losing your place.
    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"20\n");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.pane_text(pane).contains("line20"));

    client.transport.send(ClientMsg::Scroll { pane, delta: 8 }).expect("scroll");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.scroll().is_some());
    assert!(client.frame.scroll().is_some());

    send_input(&mut client, pane, b"1\n");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.scroll().is_none()),
        "typing left the view in the history"
    );
}

#[test]
fn two_clients_scroll_independently() {
    // The offset is per client. Two people on one session read at their own
    // pace, which a session-wide offset could never allow.
    let mut daemon = Daemon::detached(counting_spawner);
    let mut reader = attach(&mut daemon, 20, 6);
    let mut watcher = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut reader, &mut watcher], |c| {
        c[0].focus.is_some() && c[1].focus.is_some()
    });
    let pane = reader.focus.expect("focus");
    send_input(&mut reader, pane, b"20\n");
    settle(&mut daemon, &mut [&mut reader, &mut watcher], |c| {
        c[0].frame.pane_text(pane).contains("line20")
            && c[1].frame.pane_text(pane).contains("line20")
    });

    reader.transport.send(ClientMsg::Scroll { pane, delta: 8 }).expect("scroll");
    assert!(settle(&mut daemon, &mut [&mut reader, &mut watcher], |c| {
        c[0].frame.pane_text(pane).contains("line9")
    }));
    assert!(
        watcher.frame.pane_text(pane).contains("line20"),
        "one client's scrolling moved the other's screen:\n{}",
        watcher.frame.pane_text(pane)
    );
    assert_eq!(watcher.frame.scroll(), None);
}

#[test]
fn copy_mode_takes_text_out_of_the_history() {
    use haven_proto::HistoryPoint;

    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"20\n");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.pane_text(pane).contains("line20"));

    // Rows 1 and 2 of the history: the echoed command, then line1.
    client
        .transport
        .send(ClientMsg::Copy {
            pane,
            start: HistoryPoint { row: 1, col: 0 },
            end: HistoryPoint { row: 2, col: 19 },
        })
        .expect("copy");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| !c[0].buffers.is_empty()),
        "nothing came back"
    );
    let got = client.buffers[0].clone();
    assert!(got.contains("line1"), "copied the wrong rows: {got:?}");
    assert!(!got.ends_with(' '), "trailing padding was copied: {got:?}");
}

#[test]
fn a_search_moves_the_view_to_the_match() {
    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"40\n");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.pane_text(pane).contains("line40"));
    assert!(!client.frame.pane_text(pane).contains("line7\n"), "line7 should be off-screen");

    client
        .transport
        .send(ClientMsg::Search { pane, query: "line7 ".into(), from: 0, backwards: false })
        .expect("search");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.pane_text(pane).contains("line7\n")
        }),
        "the view never moved to the match:\n{}",
        client.frame.pane_text(pane)
    );
}

#[test]
fn a_search_that_finds_nothing_says_so() {
    let mut daemon = Daemon::detached(counting_spawner);
    let mut client = attach(&mut daemon, 20, 6);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");

    client
        .transport
        .send(ClientMsg::Search { pane, query: "nowhere-at-all".into(), from: 0, backwards: false })
        .expect("search");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.notice.is_some()),
        "a key that found nothing must not look like a key that did nothing"
    );
}

/// A pane that prints one variable from its own environment, so a test can
/// see what it was actually given rather than what it was meant to be given.
fn env_spawner(
    env: haven_daemon::SessionEnv,
    _containers: haven_daemon::ContainerSettings,
) -> Box<dyn FnMut(PaneRequest) -> io::Result<Pty>> {
    Box::new(move |request| {
        let size = request.size;
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "echo \"sock=$SSH_AUTH_SOCK\"; while read x; do :; done".into(),
            ],
            env: env.borrow().iter().map(|(k, v)| (k.clone().into(), v.clone().into())).collect(),
            size,
            ..Default::default()
        })
    })
}

#[test]
fn a_pane_opened_after_a_reattach_gets_the_new_agent_socket() {
    // The quiet one: a session outlives the connection it was made from, so
    // without this a pane opened an hour later inherits a dead socket.
    let mut daemon = Daemon::detached(env_spawner);
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    daemon.accept_stream(theirs).expect("adopt");
    let transport = SocketTransport::from_stream(ours).expect("transport");
    let mut client = Client::new(NullFrontend::new(size(40, 8)), transport);
    client
        .transport
        .send(ClientMsg::Attach {
            session: None,
            caps: client.frontend.caps(),
            env: vec![("SSH_AUTH_SOCK".into(), "/first".into())],
        })
        .expect("attach");
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let first = client.focus.expect("focus");
    assert!(settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.pane_text(first).contains("sock=/first")
    }));

    // Reattach from somewhere else, then open a pane.
    client
        .transport
        .send(ClientMsg::Attach {
            session: None,
            caps: client.frontend.caps(),
            env: vec![("SSH_AUTH_SOCK".into(), "/second".into())],
        })
        .expect("reattach");
    settle(&mut daemon, &mut [&mut client], |_| false);
    send_action(&mut client, Action::SplitColumns { program: None }).expect("split");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.panes.values().any(|p| {
                p.rows.iter().any(|r| r.spans.iter().any(|s| s.text.contains("sock=/second")))
            })
        }),
        "the new pane kept the old socket"
    );
}

#[test]
fn the_session_environment_can_be_asked_for() {
    // The only way to fix a shell that was started before the latest attach.
    let mut daemon = Daemon::detached(spawner);
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    daemon.accept_stream(theirs).expect("adopt");
    let transport = SocketTransport::from_stream(ours).expect("transport");
    let mut client = Client::new(NullFrontend::new(size(40, 8)), transport);
    client
        .transport
        .send(ClientMsg::Attach {
            session: None,
            caps: client.frontend.caps(),
            env: vec![("DISPLAY".into(), ":7".into())],
        })
        .expect("attach");
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());

    client.transport.send(ClientMsg::ShowEnvironment).expect("ask");
    assert!(settle(&mut daemon, &mut [&mut client], |c| !c[0].environment.is_empty()));
    assert_eq!(client.environment, vec![("DISPLAY".to_string(), ":7".to_string())]);
}

#[test]
fn synchronized_panes_type_into_every_pane_at_once() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    send_action(&mut client, Action::SplitColumns { program: None }).expect("split");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.panes.len() == 2);
    let panes: Vec<PaneId> = client.frame.panes.values().map(|p| p.id).collect();

    send_action(&mut client, Action::SynchronizePanes { on: Some(true) }).expect("sync");
    settle(&mut daemon, &mut [&mut client], |_| false);
    let active = client.focus.expect("focus");
    send_input(&mut client, active, b"hello\n");

    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            panes.iter().all(|p| c[0].frame.pane_text(*p).contains("got:hello"))
        }),
        "only one pane heard it:\n{}",
        panes.iter().map(|p| client.frame.pane_text(*p)).collect::<Vec<_>>().join("---\n")
    );
}

#[test]
fn a_bell_marks_the_tab_you_were_not_looking_at() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    // A second tab, then back to the first, so the bell rings out of sight.
    send_action(&mut client, Action::NewWindow { program: None }).expect("new window");
    settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.status.as_ref().is_some_and(|s| s.windows.len() == 2)
    });
    let second = client.frame.status.as_ref().unwrap().active_window;
    send_action(&mut client, Action::SelectWindow(0)).expect("select");
    settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.status.as_ref().is_some_and(|s| s.active_window != second)
    });

    // The test shell echoes what it is told, so a BEL comes back as output.
    let panes: Vec<PaneId> = client.frame.panes.values().map(|p| p.id).collect();
    let away = panes[0];
    client
        .transport
        .send(ClientMsg::Action(Action::SendKeys {
            pane: Some(away.0 + 1),
            bytes: b"\x07\n".to_vec(),
        }))
        .expect("send");

    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.status.as_ref().is_some_and(|s| s.windows.iter().any(|w| w.bell))
        }),
        "a bell in another tab left no mark"
    );

    // And visiting it clears the mark.
    send_action(&mut client, Action::SelectWindow(1)).expect("select");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.status.as_ref().is_some_and(|s| s.windows.iter().all(|w| !w.bell))
        }),
        "the mark survived a visit"
    );
}

/// A pane running a real shell, so `/proc` has a cwd worth reading.
fn shell_spawner(
    env: haven_daemon::SessionEnv,
    _containers: haven_daemon::ContainerSettings,
) -> Box<dyn FnMut(PaneRequest) -> io::Result<Pty>> {
    Box::new(move |request| {
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-i".into()],
            cwd: request.cwd.clone(),
            env: env.borrow().iter().map(|(k, v)| (k.clone().into(), v.clone().into())).collect(),
            size: request.size,
            ..Default::default()
        })
    })
}

#[test]
fn a_saved_session_comes_back_with_its_shape_and_its_directories() {
    let dir = std::env::temp_dir().join(format!("haven-resurrect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let save = dir.join("sessions.toml");

    // Build something worth restoring: two tabs, the first split in two,
    // with one pane somewhere other than where the daemon started.
    let mut daemon = Daemon::detached(shell_spawner);
    daemon.save_path = save.clone();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    send_action(&mut client, Action::SplitColumns { program: None }).expect("split");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.panes.len() == 2);
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"cd /tmp\n");
    send_action(&mut client, Action::NewWindow { program: None }).expect("new window");
    send_action(&mut client, Action::Rename { window: true, name: "second".into() })
        .expect("rename");
    settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.status.as_ref().is_some_and(|s| s.windows.len() == 2)
    });
    // Let the `cd` actually happen before the shape is read off /proc.
    for _ in 0..40 {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        let _ = client.tick();
    }

    let (count, problem) = daemon.save_sessions();
    assert_eq!(problem, None, "save failed");
    assert_eq!(count, 1);

    // A second daemon, as a reboot would give: nothing in it but the file.
    let mut fresh = Daemon::detached(shell_spawner);
    fresh.save_path = save.clone();
    let (restored, problem) = fresh.restore_sessions();
    assert_eq!(problem, None, "restore failed");
    assert_eq!(restored, 1);

    let names = fresh.session_names();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].1, "haven-1", "the session kept its name");
    assert_eq!(names[0].2, 2, "both tabs should be back");

    // Save again *from the restored daemon*, and compare. Checking the file
    // written before the restore would only prove the shape was recorded;
    // this proves the panes that came back are really in those directories,
    // because the second save reads them from `/proc` all over again.
    for _ in 0..40 {
        fresh.tick(Duration::from_millis(10)).expect("tick");
    }
    let (_, problem) = fresh.save_sessions();
    assert_eq!(problem, None);
    let saved = haven_daemon::snapshot::Snapshot::read(&save).expect("read");
    let window = &saved.sessions[0].windows[0];
    assert_eq!(window.panes.len(), 2, "the split did not survive");
    assert!(
        window.panes.iter().any(|p| p.cwd.as_deref() == Some("/tmp")),
        "a restored pane did not start where it was saved: {:?}",
        window.panes
    );
    assert_eq!(saved.sessions[0].windows[1].name.as_deref(), Some("second"));
    assert_eq!(
        fresh.session_names()[0].1,
        "haven-1",
        "the restored session should keep its name through a second save"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_pane_starts_where_the_one_you_were_in_is() {
    // It started wherever the daemon happened to be — usually `/`, because
    // the daemon is autostarted and outlives the shell that asked for it.
    // Splitting a pane to run one command against the files in front of you
    // meant `cd`-ing back to them first, every time.
    let mut daemon = Daemon::detached(shell_spawner);
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());

    let first = client.focus.expect("focus");
    send_input(&mut client, first, b"cd /tmp\n");
    for _ in 0..40 {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        let _ = client.tick();
    }

    send_action(&mut client, Action::SplitColumns { program: None }).expect("split");
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.panes.len() == 2);
    for _ in 0..40 {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        let _ = client.tick();
    }

    // Read the directories back the way a save does, off /proc.
    daemon.save_path = std::env::temp_dir().join(format!("haven-cwd-{}.toml", std::process::id()));
    let (_, problem) = daemon.save_sessions();
    assert_eq!(problem, None);
    let saved = haven_daemon::snapshot::Snapshot::read(&daemon.save_path).expect("read");
    let dirs: Vec<Option<&str>> =
        saved.sessions[0].windows[0].panes.iter().map(|p| p.cwd.as_deref()).collect();
    assert!(
        dirs.iter().filter(|d| **d == Some("/tmp")).count() == 2,
        "the new pane should have opened in /tmp too: {dirs:?}"
    );
    let _ = std::fs::remove_file(&daemon.save_path);
}

#[test]
fn a_grep_finds_lines_in_every_pane_and_says_which_one() {
    // The point of searching the session rather than the pane: the error
    // you are looking for is in the window you are not looking at.
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let first = client.focus.expect("focus");
    send_input(&mut client, first, b"echo needle-one\n");
    send_action(&mut client, Action::NewWindow { program: None }).expect("new window");
    settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.status.as_ref().is_some_and(|s| s.windows.len() == 2)
    });
    let second = client.focus.expect("focus");
    send_input(&mut client, second, b"echo needle-two\n");
    for _ in 0..60 {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        let _ = client.tick();
    }

    client.transport.send(ClientMsg::Grep { query: "needle-".into() }).expect("grep");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].menu.as_ref().is_some_and(|m| m.entries.len() >= 2)
        }),
        "the grep found {:?}",
        client.menu.as_ref().map(|m| m.entries.clone())
    );
    let menu = client.menu.as_ref().expect("no menu");
    let text: String = menu.entries.iter().map(|(label, _)| label.clone()).collect();
    assert!(text.contains("needle-one"), "{text}");
    assert!(text.contains("needle-two"), "a pane in another window was not searched: {text}");
    // Both panes are represented, each hit naming where it is.
    let panes: std::collections::HashSet<&str> =
        menu.entries.iter().filter_map(|(_, cmd)| cmd.split(' ').nth(2)).collect();
    assert!(panes.len() >= 2, "every hit came from one pane: {:?}", menu.entries);
}

#[test]
fn a_panes_history_comes_back_marked_as_this_panes_own() {
    // The marking spawner stands in for a shell with integration: it
    // reports each line as a command starting. Typing the escape by hand
    // cannot work — the tty echoes control characters in caret notation —
    // which is why that spawner exists.
    let mut daemon = Daemon::detached(marking_spawner);
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"cargo test\n");
    // A second command: the first is remembered when something displaces
    // it, since this stub never reports an ending.
    send_input(&mut client, pane, b"ls -la\n");
    for _ in 0..60 {
        daemon.tick(Duration::from_millis(10)).expect("tick");
        let _ = client.tick();
    }

    client.transport.send(ClientMsg::History { pane }).expect("history");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].menu
                .as_ref()
                .is_some_and(|m| m.entries.iter().any(|(label, _)| label.contains("cargo test")))
        }),
        "the pane's own command was not in its history"
    );
    let menu = client.menu.as_ref().expect("no menu");
    let (label, command) =
        menu.entries.iter().find(|(l, _)| l.contains("cargo test")).expect("the entry");
    // Ran here, with no ending reported: marked, but not as a success.
    assert!(label.starts_with('*'), "{label:?}");
    assert_eq!(command, "send-keys cargo test");
    // And the panel asks about the program, not the whole line.
    assert_eq!(haven_client::help_subject("cargo test"), "cargo");
}

#[test]
fn renaming_a_session_reaches_everything_that_shows_its_name() {
    // It reached nothing. The name was kept twice — once in the mux, once
    // beside it in the daemon — and `rename-session` wrote the mux's copy
    // while the status bar, `haven ls` and the session tree all read the
    // other one. The command worked perfectly and showed up nowhere.
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].frame.status.is_some());

    send_action(&mut client, Action::Rename { window: false, name: "build".into() })
        .expect("rename");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| {
            c[0].frame.status.as_ref().is_some_and(|s| s.name == "build")
        }),
        "the bar never heard about the rename: {:?}",
        client.frame.status.as_ref().map(|s| s.name.clone())
    );
    // And the listing `haven ls` and the session tree are built from.
    assert_eq!(daemon.session_infos()[0].name, "build");

    // An empty name is not a rename: a session with no name is unreachable
    // by name, and the old one is better than nothing.
    send_action(&mut client, Action::Rename { window: false, name: "  ".into() }).expect("rename");
    for _ in 0..20 {
        daemon.tick(Duration::from_millis(5)).expect("tick");
        let _ = client.tick();
    }
    assert_eq!(daemon.session_infos()[0].name, "build");
}

#[test]
fn a_container_pane_is_saved_as_a_spec_and_restored_only_if_there_is_one_to_rejoin() {
    // Half of this is what the save file may say, and half is what a restore
    // may act on. A save file decides what a machine runs when it comes
    // back, so the line is: a virtual shell is recorded, and honoured only
    // when its container is *already* running. Nothing in a file can make a
    // restore start a container.
    let dir = std::env::temp_dir().join(format!("haven-containers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let save = dir.join("sessions.toml");

    let mut daemon = Daemon::detached(shell_spawner);
    daemon.save_path = save.clone();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    // A container that could not possibly be running, so the restore below
    // has nothing to rejoin whatever this machine happens to have.
    let spec = "docker://run/haven-no-such-image:latest?keep";
    send_action(&mut client, Action::NewWindow { program: Some(spec.into()) }).expect("new window");
    // And an ordinary program, which must never be written to the file.
    send_action(&mut client, Action::NewWindow { program: Some("/bin/sh".into()) })
        .expect("new window");
    settle(&mut daemon, &mut [&mut client], |c| {
        c[0].frame.status.as_ref().is_some_and(|s| s.windows.len() == 3)
    });

    let (_, problem) = daemon.save_sessions();
    assert_eq!(problem, None, "save failed");
    let saved = haven_daemon::snapshot::Snapshot::read(&save).expect("read");
    let windows = &saved.sessions[0].windows;
    assert_eq!(windows[1].panes[0].program.as_deref(), Some(spec), "the spec was not recorded");
    assert_eq!(
        windows[2].panes[0].program, None,
        "only a virtual shell belongs in this field: an arbitrary program in a save file is \
         the thing `restore_programs` exists to prevent"
    );

    let mut fresh = Daemon::detached(shell_spawner);
    fresh.save_path = save.clone();
    let (restored, problem) = fresh.restore_sessions();
    assert_eq!(problem, None, "restore failed");
    assert_eq!(restored, 1);
    for _ in 0..40 {
        fresh.tick(Duration::from_millis(10)).expect("tick");
    }
    let (_, problem) = fresh.save_sessions();
    assert_eq!(problem, None);
    let again = haven_daemon::snapshot::Snapshot::read(&save).expect("read");
    assert_eq!(
        again.sessions[0].windows[1].panes[0].program, None,
        "the container is gone, so the pane that came back is an ordinary shell"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restoring_twice_does_not_duplicate_or_clobber_a_live_session() {
    // A restore has to be safe to run from a login script that may fire
    // more than once, and safe to run while you are working.
    let dir = std::env::temp_dir().join(format!("haven-resurrect2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let save = dir.join("sessions.toml");

    let mut daemon = Daemon::detached(shell_spawner);
    daemon.save_path = save.clone();
    let mut client = attach(&mut daemon, 40, 10);
    settle(&mut daemon, &mut [&mut client], |c| c[0].focus.is_some());
    daemon.save_sessions();

    let before = daemon.session_names();
    let (restored, problem) = daemon.restore_sessions();
    assert_eq!(problem, None);
    assert_eq!(restored, 0, "a session of that name is already here");
    assert_eq!(daemon.session_names(), before, "the live session was touched");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn colours_survive_the_round_trip_through_the_protocol() {
    use haven_core::{Color, Rgb};

    let mut daemon = Daemon::detached(|_env, _containers| {
        Box::new(|request| {
            let size = request.size;
            Pty::spawn(&PtyConfig {
                program: Some("/bin/sh".into()),
                args: vec![
                    "-c".into(),
                    "printf '\\033[38;2;10;200;30mGREEN\\033[0m'; while read l; do :; done".into(),
                ],
                size,
                ..Default::default()
            })
        })
    });
    let mut client = attach(&mut daemon, 30, 4);

    assert!(settle(&mut daemon, &mut [&mut client], |c| {
        c[0].focus.is_some_and(|p| c[0].frame.pane_text(p).contains("GREEN"))
    }));

    // The style table has to arrive for this to be anything but default.
    let pane = client.focus.unwrap();
    let frame = client.frame.pane(pane).unwrap();
    let span = frame.rows[0].spans.iter().find(|s| s.text.contains("GREEN")).expect("span");
    assert_eq!(span.style.fg, Color::Rgb(Rgb::new(10, 200, 30)), "truecolor was lost on the wire");
}

#[test]
fn the_status_reaches_the_client_and_follows_the_layout() {
    let mut daemon = daemon();
    let mut client = attach(&mut daemon, 40, 8);

    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.status.is_some()),
        "no status ever arrived"
    );
    let status = client.frame.status.clone().expect("status");
    assert_eq!(status.windows.len(), 1);
    assert_eq!(status.windows[0].panes.len(), 1);
    assert_eq!(status.windows[0].active_pane, status.windows[0].panes[0].id);
    assert_eq!(status.active_window, status.windows[0].id);
    assert!(status.started_unix > 0, "no start time to measure uptime from");

    // Splitting must be reflected: a status pinned to the first layout would
    // leave the pane count permanently wrong.
    send_action(&mut client, Action::SplitColumns { program: None }).expect("split");
    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0].frame.status.as_ref().is_some_and(|s| s
            .windows[0]
            .panes
            .len()
            == 2)),
        "the status did not follow the split: {:?}",
        client.frame.status
    );
}

#[test]
fn a_tab_is_named_after_the_command_the_shell_reports() {
    // The shell integration mark is the only reliable source: a title is
    // whatever the shell decided to write, and is usually a path.
    let mut daemon = Daemon::detached(marking_spawner);
    let mut client = attach(&mut daemon, 50, 8);
    assert!(settle(&mut daemon, &mut [&mut client], |c| c[0].frame.status.is_some()));

    let pane = client.focus.expect("focus");
    send_input(&mut client, pane, b"make%20-j8\n");

    assert!(
        settle(&mut daemon, &mut [&mut client], |c| c[0]
            .frame
            .status
            .as_ref()
            .is_some_and(|s| s.windows[0].name.contains("make -j8"))),
        "the tab was not named after the command: {:?}\npane text:\n{}",
        client.frame.status.as_ref().map(|s| (
            s.windows[0].name.clone(),
            s.windows[0].panes.iter().map(|p| p.command.clone()).collect::<Vec<_>>()
        )),
        client.frame.pane_text(pane)
    );

    // And the pane carries it too, for the chooser.
    let status = client.frame.status.clone().expect("status");
    assert_eq!(status.windows[0].panes[0].command, "make -j8");
}
