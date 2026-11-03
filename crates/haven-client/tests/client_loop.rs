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
                PaneInfo {
                    id: 7,
                    rect: Default::default(),
                    title: String::new(),
                    command: String::new(),
                    running: false,
                },
                PaneInfo {
                    id: 8,
                    rect: Default::default(),
                    title: String::new(),
                    command: String::new(),
                    running: false,
                },
            ],
            active_pane: 8,
            zoomed: false,
            synchronized: false,
            bell: false,
            activity: false,
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

#[test]
fn find_window_goes_to_the_tab_whose_name_matches() {
    use haven_proto::{PaneInfo, SessionStatus, WindowInfo};

    let window = |id: u32, name: &str| WindowInfo {
        id,
        synchronized: false,
        bell: false,
        activity: false,
        name: name.into(),
        panes: vec![PaneInfo {
            id,
            rect: Default::default(),
            title: String::new(),
            command: String::new(),
            running: false,
        }],
        active_pane: id,
        zoomed: false,
    };
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 10, 2)], 1)),
        ServerMsg::Status(SessionStatus {
            session: SessionId(1),
            name: "s".into(),
            windows: vec![window(1, "fish"), window(2, "cargo build"), window(3, "vim")],
            active_window: 1,
            started_unix: 0,
            clients: 1,
        }),
    ]);
    let frontend = ScriptedFrontend::new(size(10, 2), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();

    client.find_window_for_test("CARGO").expect("search");
    assert_eq!(
        client.transport.sent.last(),
        Some(&ClientMsg::Action(haven_proto::Action::SelectWindow(1))),
        "case should not matter, and the index is zero-based"
    );

    client.find_window_for_test("nothing like this").expect("search");
    assert!(
        client.frame.notice.as_deref().is_some_and(|n| n.contains("no tab")),
        "a search that found nothing must say so"
    );
}

#[test]
fn the_shell_listing_becomes_a_menu_that_does_what_was_asked() {
    // The daemon answers with its own machine's shells, because that is
    // where the pane will be spawned.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells {
            shells: vec!["/usr/bin/fish".into(), "/bin/bash".into()],
            containers: vec![],
        },
    ]);
    let frontend = ScriptedFrontend::new(size(20, 6), []);
    let mut client = Client::new(frontend, server);
    client.tick().unwrap();

    // A new tab by default, because the keys that offer a shell are the
    // ones that make tabs; `c` has meant a tab everywhere else all along.
    let menu = client.menu.as_ref().expect("no menu was opened");
    assert_eq!(menu.entries.len(), 2);
    assert!(menu.entries[0].0.starts_with("fish"), "the label should lead with the name");
    assert_eq!(menu.entries[0].1, "new-window /usr/bin/fish");
    assert!(menu.title.contains("tab"), "the title should say where it will go: {}", menu.title);
    // And it is on screen: a modal nobody can see is a hang.
    assert!(
        client.frame.widgets.iter().any(|w| w.kind == haven_client::WidgetKind::Overlay),
        "the menu was not drawn"
    );
}

#[test]
fn containers_are_in_the_same_menu_as_the_shells() {
    // One menu, because it is one decision: "give me a shell" does not
    // split into "which shell" and "where" until after you have seen the
    // list. Two keys would mean choosing before you knew.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells {
            shells: vec!["/bin/bash".into()],
            containers: vec![
                haven_proto::ContainerTarget {
                    spec: "docker://exec/web".into(),
                    label: "web".into(),
                    detail: "nginx:latest · Up 2 hours".into(),
                    present: true,
                    choosable: false,
                },
                haven_proto::ContainerTarget {
                    spec: "docker://run/archlinux:latest".into(),
                    label: "archlinux:latest".into(),
                    detail: "not pulled yet".into(),
                    present: false,
                    choosable: true,
                },
            ],
        },
    ]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 8), []), server);
    client.tick().unwrap();

    let menu = client.menu.as_ref().expect("no menu");
    assert_eq!(menu.entries.len(), 3, "{:?}", menu.entries);
    assert_eq!(menu.entries[0].1, "new-window /bin/bash");
    // A container that is already up has nothing left to ask, so it opens.
    assert_eq!(menu.entries[1].1, "new-window docker://exec/web");
    // An image does: hence a command that opens the second question rather
    // than a pane.
    assert_eq!(menu.entries[2].1, "container-shell new-window docker://run/archlinux:latest");
    // And the list says what it would cost before you pick it: a pane that
    // sits blank while half a gigabyte arrives looks broken.
    assert!(menu.entries[2].0.contains("not pulled yet"), "{}", menu.entries[2].0);
}

#[test]
fn a_menu_pick_that_opens_another_menu_is_not_dropped_on_the_floor() {
    // The bug this is here for: picking a distro did nothing and said
    // nothing. Menu picks honoured only `Act` — the verdict that sends a
    // message to the daemon — and silently discarded every other one, so an
    // entry whose whole job is to ask a second question could never ask it.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells {
            shells: vec![],
            containers: vec![haven_proto::ContainerTarget {
                spec: "docker://run/fedora:latest".into(),
                label: "fedora:latest".into(),
                detail: "not pulled yet".into(),
                present: false,
                choosable: true,
            }],
        },
    ]);
    let frontend =
        ScriptedFrontend::new(size(40, 10), [InputEvent::Key(KeyEvent::named(NamedKey::Enter))]);
    let mut client = Client::new(frontend, server);
    // The vocabulary lives above the client, so the filter stands in for it.
    client.key_filter = Some(Box::new(|_, _, _| haven_client::Intercepted::ChooseLifetime {
        verb: "new-window".into(),
        spec: "docker://run/fedora:latest".into(),
    }));
    client.tick().unwrap();
    assert!(client.menu.is_some(), "the shell menu did not open");

    // Fed only now: a keystroke is read before the messages are folded in,
    // so one queued at the start would arrive before there was a menu.
    client.frontend.feed(InputEvent::Key(KeyEvent::named(NamedKey::Enter)));
    client.tick().unwrap();
    let menu = client.menu.as_ref().expect("picking a distro left no menu at all");
    assert!(
        menu.title.contains("keep what you install"),
        "the second question was never asked: {}",
        menu.title
    );
    assert_eq!(menu.entries.len(), 2, "{:?}", menu.entries);
}

#[test]
fn a_menu_pick_that_only_has_something_to_say_says_it() {
    // The same bug, in the shape it takes for a command that reports rather
    // than acts: the notice was dropped too, which is how "nothing happened"
    // became indistinguishable from "nothing happened, silently".
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells { shells: vec!["/bin/bash".into()], containers: vec![] },
    ]);
    let frontend =
        ScriptedFrontend::new(size(40, 10), [InputEvent::Key(KeyEvent::named(NamedKey::Enter))]);
    let mut client = Client::new(frontend, server);
    client.key_filter =
        Some(Box::new(|_, _, _| haven_client::Intercepted::Notice("could not".into())));
    client.tick().unwrap();
    client.tick().unwrap();
    assert_eq!(client.frame.notice.as_deref(), Some("could not"));
}

#[test]
fn the_clock_on_the_bar_moves_without_anything_happening() {
    // It did not: the bar was rebuilt only when something else caused a
    // redraw, so the session's age sat still until a keystroke or some
    // output repainted the row. The one thing on the bar that no message
    // announces was the one thing nothing redrew.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Status(haven_proto::SessionStatus {
            session: SessionId(1),
            name: "work".into(),
            windows: vec![],
            active_window: 0,
            started_unix: 1_000,
            clients: 1,
        }),
    ]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 6), []), server);
    assert_eq!(client.tick_at(1_060).unwrap(), Tick::Redraw, "the status should have arrived");
    let before = client.frontend.presented.len();

    // Same second, nothing happening: no work at all.
    assert_eq!(client.tick_at(1_060).unwrap(), Tick::Idle);
    assert_eq!(client.frontend.presented.len(), before, "an idle tick must not repaint");

    // The second moves, so the bar does.
    assert_eq!(client.tick_at(1_061).unwrap(), Tick::Redraw);
    assert_eq!(client.frontend.presented.len(), before + 1);
    assert!(
        client.next_wake().is_some(),
        "a frontend that sleeps until something happens has to be told to wake for this"
    );
}

#[test]
fn a_client_with_no_bar_has_no_reason_to_wake() {
    // The other half: an idle window with nothing to show still costs
    // nothing, which is the property the event loop was written for.
    let server = ScriptedTransport::new([ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1))]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 6), []), server);
    client.tick_at(1_000).unwrap();
    assert_eq!(client.tick_at(1_001).unwrap(), Tick::Idle);
    assert!(client.next_wake().is_none());
}

#[test]
fn a_long_list_is_typed_at_rather_than_counted_down() {
    // A machine with sixty images filled the screen with a menu you could
    // only reach the top of. Digits stop at nine; typing does not.
    let containers: Vec<haven_proto::ContainerTarget> = (0..40)
        .map(|i| haven_proto::ContainerTarget {
            spec: format!("docker://run/thing-{i}:latest"),
            label: format!("thing-{i}:latest"),
            detail: "120MB".into(),
            present: true,
            choosable: true,
        })
        .collect();
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells { shells: vec!["/bin/bash".into()], containers },
    ]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 10), []), server);
    client.tick().unwrap();
    assert!(client.menu.as_ref().expect("no menu").filterable);

    // A handful of shells is still picked by number.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells {
            shells: vec!["/bin/bash".into(), "/usr/bin/fish".into()],
            containers: vec![],
        },
    ]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 10), []), server);
    client.tick().unwrap();
    assert!(!client.menu.as_ref().expect("no menu").filterable);
}

#[test]
fn mounts_follow_the_container_and_leave_ordinary_shells_alone() {
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells {
            shells: vec!["/bin/bash".into()],
            containers: vec![haven_proto::ContainerTarget {
                spec: "docker://exec/web".into(),
                label: "web".into(),
                detail: String::new(),
                present: true,
                choosable: false,
            }],
        },
    ]);
    let mut client = Client::new(ScriptedFrontend::new(size(40, 8), []), server);
    client.container_mounts = vec!["cwd".into(), "/srv:/work".into()];
    client.tick().unwrap();

    let menu = client.menu.as_ref().expect("no menu");
    assert_eq!(
        menu.entries[0].1, "new-window /bin/bash",
        "a local shell has nothing to mount and must not be given mount options"
    );
    assert_eq!(menu.entries[1].1, "new-window docker://exec/web?mount=cwd&mount=/srv:/work");
}

#[test]
fn the_shell_menu_can_be_asked_for_a_pane_instead() {
    // The same menu, a different verb: what to do with the pick travels
    // with the request rather than being baked into the menu.
    let server = ScriptedTransport::new([
        ServerMsg::Layout(layout(&[(1, 0, 0, 20, 4)], 1)),
        ServerMsg::Shells { shells: vec!["/bin/bash".into()], containers: vec![] },
    ]);
    let frontend = ScriptedFrontend::new(size(20, 6), []);
    let mut client = Client::new(frontend, server);
    client.shell_verb = "split-v".into();
    client.tick().unwrap();

    let menu = client.menu.as_ref().expect("no menu");
    assert_eq!(menu.entries[0].1, "split-v /bin/bash");
    assert!(menu.title.contains("pane"), "{}", menu.title);
}
