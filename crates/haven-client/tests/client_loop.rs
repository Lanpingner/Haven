//! M0.5's acceptance test: the whole client loop, with no window, no terminal
//! and no daemon. See `docs/PLAN.md` §16.

use haven_client::input::InputEvent;
use haven_client::{Client, KeyEvent, Modifiers, NamedKey, ScriptedFrontend, Tick};
use haven_core::{Cell, Cursor};
use haven_proto::transport::ScriptedTransport;
use haven_proto::{
    CellSize, ClientMsg, KeyTier, Layout, PaneId, PaneRect, RowRle, ServerMsg, SessionId,
    PROTOCOL_VERSION,
};

fn size(cols: u16, rows: u16) -> CellSize {
    CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
}

fn cells(s: &str) -> Vec<Cell> {
    s.chars().map(Cell::new).collect()
}

fn layout(panes: &[(u32, u16, u16, u16, u16)], active: u32) -> Layout {
    Layout {
        window: 0,
        panes: panes
            .iter()
            .map(|&(pane, col, row, cols, rows)| PaneRect {
                pane: PaneId(pane),
                col,
                row,
                cols,
                rows,
            })
            .collect(),
        active: Some(PaneId(active)),
    }
}

#[test]
fn attach_then_render_a_pane_end_to_end() {
    let server = ScriptedTransport::new([
        ServerMsg::Hello { protocol: PROTOCOL_VERSION, session: SessionId(1), build: 0 },
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 2)], 1)),
        ServerMsg::Snapshot {
            pane: PaneId(1),
            rows: vec![RowRle::encode(&cells("hello haven")), RowRle::encode(&cells(""))],
            cursor: Cursor::default(),
            rev: 1,
        },
    ]);
    let frontend = ScriptedFrontend::new(size(20, 2), []);
    let mut client = Client::new(frontend, server);

    client.attach(None).unwrap();
    assert!(client.is_attached());
    assert_eq!(client.tick().unwrap(), Tick::Redraw);

    assert_eq!(client.focus, Some(PaneId(1)));
    assert_eq!(client.frame.pane_text(PaneId(1)), "hello haven\n\n");

    // The frontend was actually presented to, exactly once for the batch.
    assert_eq!(client.frontend.presented.len(), 1);
    assert_eq!(client.frontend.last().unwrap().panes.len(), 1);

    // And the handshake really went out.
    assert!(matches!(client.transport.sent[0], ClientMsg::Attach { .. }));
}

#[test]
fn an_idle_tick_presents_nothing() {
    let frontend = ScriptedFrontend::new(size(10, 2), []);
    let mut client = Client::new(frontend, ScriptedTransport::new([]));
    assert_eq!(client.tick().unwrap(), Tick::Idle);
    assert!(client.frontend.presented.is_empty(), "idle must stay genuinely idle");
}

#[test]
fn keystrokes_are_encoded_and_forwarded_to_the_focused_pane() {
    let server = ScriptedTransport::new([ServerMsg::Layout(layout(&[(7, 0, 0, 10, 2)], 7))]);
    let script = [
        InputEvent::Key(KeyEvent::char('a')),
        InputEvent::Key(KeyEvent::char('c').with(Modifiers::CTRL)),
        InputEvent::Key(KeyEvent::named(NamedKey::Up)),
    ];
    let frontend = ScriptedFrontend::new(size(10, 2), script);
    let mut client = Client::new(frontend, server);

    // First tick learns the layout (and thus the focus).
    assert_eq!(client.tick().unwrap(), Tick::Redraw);
    assert_eq!(client.focus, Some(PaneId(7)));
    // Second tick has input to deliver.
    client.frontend = ScriptedFrontend::new(
        size(10, 2),
        [
            InputEvent::Key(KeyEvent::char('a')),
            InputEvent::Key(KeyEvent::char('c').with(Modifiers::CTRL)),
            InputEvent::Key(KeyEvent::named(NamedKey::Up)),
        ],
    );
    client.tick().unwrap();

    let inputs: Vec<Vec<u8>> = client
        .transport
        .sent
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Input { pane, bytes } if *pane == PaneId(7) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec![b"a".to_vec(), vec![0x03], b"\x1b[A".to_vec()]);
}

#[test]
fn input_before_focus_is_dropped_rather_than_sent_to_nobody() {
    let frontend = ScriptedFrontend::new(size(10, 2), [InputEvent::Key(KeyEvent::char('x'))]);
    let mut client = Client::new(frontend, ScriptedTransport::new([]));
    client.tick().unwrap();
    assert!(client.transport.sent.is_empty());
}

#[test]
fn resize_updates_caps_and_tells_the_server() {
    let frontend = ScriptedFrontend::new(size(80, 24), [InputEvent::Resize(size(100, 30))]);
    let mut client = Client::new(frontend, ScriptedTransport::new([]));
    assert_eq!(client.tick().unwrap(), Tick::Redraw);
    match &client.transport.sent[0] {
        // The daemon is told about one row fewer than the screen has: the
        // bottom row belongs to the status line, and a pane laid out there
        // would be drawn over every frame.
        ClientMsg::Resize { caps } => assert_eq!(caps.size, size(100, 29)),
        other => panic!("expected a resize, got {other:?}"),
    }
    // The frontend still draws the whole screen.
    assert_eq!(client.frame.size, size(100, 30));
}

#[test]
fn quit_detaches_and_finishes() {
    let frontend = ScriptedFrontend::new(size(10, 2), [InputEvent::Quit]);
    let mut client = Client::new(frontend, ScriptedTransport::new([]));
    assert_eq!(client.tick().unwrap(), Tick::Finished);
    assert_eq!(client.transport.sent.last(), Some(&ClientMsg::Detach));
}

#[test]
fn a_vanished_server_ends_the_session_instead_of_erroring() {
    let mut transport = ScriptedTransport::new([]);
    // Refuse the very first send, as a dead socket would.
    transport.disconnect_after = Some(0);
    let frontend = ScriptedFrontend::new(size(10, 2), [InputEvent::Quit]);
    let mut client = Client::new(frontend, transport);
    // Quit tries to send Detach, which fails: the session is over either way,
    // and a user-visible crash would be the wrong answer.
    assert!(client.tick().is_err() || !client.is_attached());
}

#[test]
fn a_pane_that_exits_disappears_from_the_frame() {
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 10, 1), (2, 10, 0, 10, 1)], 1)),
        ServerMsg::Snapshot {
            pane: PaneId(2),
            rows: vec![RowRle::encode(&cells("doomed"))],
            cursor: Cursor::default(),
            rev: 1,
        },
        ServerMsg::PaneExited { pane: PaneId(2), status: 0 },
    ]);
    let frontend = ScriptedFrontend::new(size(20, 1), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();
    assert!(client.frame.pane(PaneId(2)).is_none());
    assert!(client.frame.pane(PaneId(1)).is_some());
}

#[test]
fn a_tui_frontend_at_legacy_tier_encodes_differently_from_a_gpu_one() {
    // The same keypress, two fidelities, with no terminal involved.
    let event = KeyEvent::char('a').with(Modifiers::CTRL | Modifiers::SHIFT);
    let server = ScriptedTransport::new([ServerMsg::Layout(layout(&[(1, 0, 0, 10, 1)], 1))]);
    let legacy =
        ScriptedFrontend::new(size(10, 1), [InputEvent::Key(event)]).as_tui(KeyTier::Legacy);
    let mut client = Client::new(legacy, server);
    client.tick().unwrap(); // layout
    client.frontend =
        ScriptedFrontend::new(size(10, 1), [InputEvent::Key(event)]).as_tui(KeyTier::Legacy);
    client.tick().unwrap();
    let legacy_bytes = match client.transport.sent.last().unwrap() {
        ClientMsg::Input { bytes, .. } => bytes.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(legacy_bytes, vec![0x01], "legacy collapses ctrl+shift+a onto ctrl+a");

    let server = ScriptedTransport::new([ServerMsg::Layout(layout(&[(1, 0, 0, 10, 1)], 1))]);
    let full = ScriptedFrontend::new(size(10, 1), []);
    let mut client = Client::new(full, server);
    client.tick().unwrap();
    client.frontend = ScriptedFrontend::new(size(10, 1), [InputEvent::Key(event)]);
    client.tick().unwrap();
    let full_bytes = match client.transport.sent.last().unwrap() {
        ClientMsg::Input { bytes, .. } => bytes.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(full_bytes, b"\x1b[97;6u".to_vec(), "full tier disambiguates");
}

#[test]
fn version_skew_surfaces_as_a_readable_notice() {
    let server = ScriptedTransport::new([ServerMsg::Refused {
        reason: "daemon speaks protocol 2, client speaks 1 — restart the daemon".into(),
    }]);
    let frontend = ScriptedFrontend::new(size(10, 2), []);
    let mut client = Client::new(frontend, server);
    assert_eq!(client.tick().unwrap(), Tick::Redraw);
    assert!(client.frame.notice.as_deref().unwrap().contains("restart the daemon"));
}

#[test]
fn the_chooser_opens_on_the_active_pane_once_the_listing_arrives() {
    use haven_client::chooser::{Choice, Chooser};
    use haven_proto::{PaneInfo, SessionId, SessionInfo, SessionStatus, WindowInfo};

    let status = SessionStatus {
        session: SessionId(1),
        name: "work".into(),
        windows: vec![WindowInfo {
            id: 1,
            name: "shell".into(),
            panes: vec![
                PaneInfo { id: 7, title: String::new(), command: String::new(), running: false },
                PaneInfo { id: 8, title: String::new(), command: String::new(), running: false },
            ],
            active_pane: 8,
            zoomed: false,
        }],
        active_window: 1,
        started_unix: 0,
        clients: 1,
    };
    let sessions = vec![SessionInfo {
        id: SessionId(1),
        name: "work".into(),
        windows: 1,
        panes: 2,
        clients: 1,
        size: size(80, 24),
    }];

    // Built before the listing arrives: nothing to select.
    let empty = Chooser::new(&[], Some(&status));
    assert!(empty.rows.is_empty(), "a chooser with no sessions should have no rows");

    // Once it does, the selection must land on the active pane rather than
    // staying at row zero.
    let settled = Chooser::new(&sessions, Some(&status));
    assert_eq!(settled.choice(), Some(Choice::Pane(8)));
}

#[test]
fn a_daemon_speaking_another_protocol_is_refused_with_a_reason() {
    use haven_proto::{ServerMsg, SessionId};

    // A version mismatch must be caught at the handshake. Left unread, it
    // only surfaced when some later message failed to decode — whichever
    // one a feature happened to ask for first — so it looked like that
    // feature crashing, and the GPU window simply closed.
    let transport = ScriptedTransport::new([ServerMsg::Hello {
        protocol: haven_proto::PROTOCOL_VERSION + 1,
        session: SessionId(1),
        build: 0,
    }]);
    let frontend = ScriptedFrontend::new(size(80, 24), []);
    let mut client = Client::new(frontend, transport);

    assert_eq!(client.tick().unwrap(), Tick::Finished, "the session should end");
    let reason = client.fatal().expect("no reason given");
    assert!(reason.contains("protocol"), "unhelpful reason: {reason}");
    assert!(reason.contains("haven server stop"), "no way out offered: {reason}");
}

#[test]
fn the_matching_protocol_is_not_refused() {
    use haven_proto::{ServerMsg, SessionId};

    let transport = ScriptedTransport::new([ServerMsg::Hello {
        protocol: haven_proto::PROTOCOL_VERSION,
        session: SessionId(1),
        build: 0,
    }]);
    let frontend = ScriptedFrontend::new(size(80, 24), []);
    let mut client = Client::new(frontend, transport);
    assert_ne!(client.tick().unwrap(), Tick::Finished);
    assert!(client.fatal().is_none());
}

#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer() {
    use haven_client::{MouseButton, MouseEvent};

    let server = ScriptedTransport::new([ServerMsg::Layout(layout(
        &[(1, 0, 0, 10, 4), (2, 11, 0, 10, 4)],
        1,
    ))]);
    let wheel = |col: u16| InputEvent::Mouse(MouseEvent::press(MouseButton::WheelUp, col, 1));
    let frontend = ScriptedFrontend::new(size(21, 4), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap(); // layout

    // Over the right-hand pane, which does not have focus: the wheel follows
    // the pointer, because reading one pane while typing in another is the
    // ordinary case.
    client.frontend = ScriptedFrontend::new(size(21, 4), [wheel(15)]);
    client.tick().unwrap();
    assert_eq!(
        client.transport.sent.last(),
        Some(&ClientMsg::Scroll { pane: PaneId(2), delta: 3 })
    );

    client.frontend = ScriptedFrontend::new(size(21, 4), [wheel(2)]);
    client.tick().unwrap();
    assert_eq!(
        client.transport.sent.last(),
        Some(&ClientMsg::Scroll { pane: PaneId(1), delta: 3 })
    );
}

#[test]
fn a_scrolled_pane_says_so_and_hides_its_cursor() {
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 10, 2)], 1)),
        ServerMsg::Modes {
            pane: PaneId(1),
            app_cursor_keys: false,
            bracketed_paste: false,
            cursor_visible: false,
            mouse: haven_proto::MouseMode::default(),
        },
        ServerMsg::Scrolled { pane: PaneId(1), offset: 7, total: 40 },
    ]);
    let frontend = ScriptedFrontend::new(size(10, 2), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();
    assert_eq!(client.frame.scroll(), Some((7, 40)));
    assert!(!client.frame.cursor_visible(), "a cursor below the view would be drawn");
}

#[test]
fn a_click_moves_focus_to_the_pane_under_the_pointer() {
    use haven_client::{MouseButton, MouseEvent};

    let server = ScriptedTransport::new([ServerMsg::Layout(layout(
        &[(1, 0, 0, 10, 4), (2, 11, 0, 10, 4)],
        1,
    ))]);
    let frontend = ScriptedFrontend::new(size(21, 4), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();
    assert_eq!(client.focus, Some(PaneId(1)));

    client.frontend = ScriptedFrontend::new(
        size(21, 4),
        [InputEvent::Mouse(MouseEvent::press(MouseButton::Left, 15, 2))],
    );
    client.tick().unwrap();
    assert_eq!(
        client.transport.sent.last(),
        Some(&ClientMsg::Action(haven_proto::Action::SelectPane(2)))
    );
}

#[test]
fn a_pane_that_asked_for_the_mouse_gets_it_in_its_own_coordinates() {
    use haven_client::{MouseButton, MouseEvent};
    use haven_proto::{MouseMode, MouseTracking};

    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 10, 4), (2, 11, 0, 10, 4)], 2)),
        ServerMsg::Modes {
            pane: PaneId(2),
            app_cursor_keys: false,
            bracketed_paste: false,
            cursor_visible: true,
            mouse: MouseMode { tracking: MouseTracking::Click, sgr: true },
        },
    ]);
    let frontend = ScriptedFrontend::new(size(21, 4), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();

    // Screen column 15 is column 4 of a pane that starts at 11, and the
    // wire is one-based on top of that.
    client.frontend = ScriptedFrontend::new(
        size(21, 4),
        [InputEvent::Mouse(MouseEvent::press(MouseButton::Left, 15, 2))],
    );
    client.tick().unwrap();
    let sent: Vec<Vec<u8>> = client
        .transport
        .sent
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Input { pane, bytes } if *pane == PaneId(2) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec![b"\x1b[<0;5;3M".to_vec()], "wrong coordinates or no report");
}

#[test]
fn the_wheel_still_scrolls_a_pane_that_does_not_want_the_mouse() {
    use haven_client::{MouseButton, MouseEvent};

    let server = ScriptedTransport::new([ServerMsg::Layout(layout(&[(1, 0, 0, 10, 4)], 1))]);
    let frontend = ScriptedFrontend::new(size(10, 4), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();
    client.frontend = ScriptedFrontend::new(
        size(10, 4),
        [InputEvent::Mouse(MouseEvent::press(MouseButton::WheelUp, 2, 2))],
    );
    client.tick().unwrap();
    assert!(matches!(client.transport.sent.last(), Some(ClientMsg::Scroll { .. })));
}
