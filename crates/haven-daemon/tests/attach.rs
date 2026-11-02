//! Attach, detach, and two clients on one session — over a real socket, with
//! the real client stack on the other end. See `docs/PLAN.md` §16 (M5).

use std::io;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use haven_client::{Client, NullFrontend};
use haven_daemon::{Daemon, ResizePolicy};
use haven_proto::transport::SocketTransport;
use haven_proto::{Action, CellSize, ClientMsg, PaneId, SessionId, Transport, TransportError};
use haven_pty::{Pty, PtyConfig, PtySize};

type TestClient = Client<NullFrontend, SocketTransport>;

fn size(cols: u16, rows: u16) -> CellSize {
    CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
}

/// Panes running a quiet shell that echoes what it is told.
fn spawner() -> Box<dyn FnMut(PtySize) -> io::Result<Pty>> {
    Box::new(|size| {
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "while read l; do echo \"got:$l\"; done".into()],
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
    assert_eq!(frame.rows.len(), 8);
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

    send_action(&mut client, Action::SplitColumns).unwrap();
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
fn colours_survive_the_round_trip_through_the_protocol() {
    use haven_core::{Color, Rgb};

    let mut daemon = Daemon::detached(|| {
        Box::new(|size| {
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
