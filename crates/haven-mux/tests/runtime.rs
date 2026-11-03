//! The mux runtime against real ptys: splits, geometry, exits.
//!
//! Every pane is its own terminal with its own child, so the thing most worth
//! testing is that each child is told about *its* rectangle and nothing else.

use std::io;
use std::time::{Duration, Instant};

use haven_mux::layout::{Direction, Edge};
use haven_mux::{MuxRuntime, RuntimeEvent};
use haven_proto::{CellSize, PaneId, SessionId};
use haven_pty::{Pty, PtyConfig};

fn size(cols: u16, rows: u16) -> CellSize {
    CellSize { cols, rows, pixel_width: cols * 8, pixel_height: rows * 16 }
}

/// A pane that reports its geometry on startup and again on every line of
/// input. Re-reporting is what lets a test check that a resize actually
/// reached the child's kernel view, rather than only our bookkeeping.
fn geometry_shell(request: haven_mux::PaneRequest) -> io::Result<Pty> {
    let size = request.size;
    Pty::spawn(&PtyConfig {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), "stty size; while read l; do stty size; done".into()],
        size,
        ..Default::default()
    })
}

fn runtime(
    cols: u16,
    rows: u16,
) -> MuxRuntime<impl FnMut(haven_mux::PaneRequest) -> io::Result<Pty>> {
    MuxRuntime::new(SessionId(1), "test", size(cols, rows), geometry_shell).expect("spawn")
}

/// Pump until a predicate holds or we give up.
fn pump_until<F, P>(rt: &mut MuxRuntime<F>, mut done: P) -> bool
where
    F: FnMut(haven_mux::PaneRequest) -> io::Result<Pty>,
    P: FnMut(&MuxRuntime<F>) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        rt.pump(Duration::from_millis(50)).expect("pump");
        if done(rt) {
            return true;
        }
    }
    false
}

fn pane_text(
    rt: &MuxRuntime<impl FnMut(haven_mux::PaneRequest) -> io::Result<Pty>>,
    pane: PaneId,
) -> String {
    rt.pane(pane).map(|p| p.grid().dump()).unwrap_or_default()
}

#[test]
fn a_fresh_session_runs_one_full_size_pane() {
    let mut rt = runtime(80, 24);
    let pane = rt.session.active_pane();
    assert!(pump_until(&mut rt, |rt| pane_text(rt, pane).contains("24 80")));
    assert_eq!(rt.visible().len(), 1);
}

#[test]
fn splitting_gives_each_child_its_own_geometry() {
    let mut rt = runtime(80, 24);
    let first = rt.session.active_pane();
    let second = rt.split(Direction::Columns, None).unwrap().expect("split succeeded");

    let rects = rt.rects();
    let width = |id: PaneId| rects.iter().find(|r| r.pane == id).unwrap().cols;
    assert_eq!(width(first) + width(second), 79, "a separator column is reserved");

    // Ask each child what size it thinks it is, *after* the split. The first
    // pane's startup report predates the split, so only a fresh answer proves
    // SIGWINCH arrived.
    for pane in [first, second] {
        rt.send_to(pane, b"\n").unwrap();
    }
    let expected_first = format!("24 {}", width(first));
    let expected_second = format!("24 {}", width(second));
    assert!(
        pump_until(&mut rt, |rt| {
            pane_text(rt, first).contains(&expected_first)
                && pane_text(rt, second).contains(&expected_second)
        }),
        "children were not told their own geometry:\nfirst:\n{}\nsecond:\n{}",
        pane_text(&rt, first),
        pane_text(&rt, second),
    );
}

#[test]
fn resizing_the_window_resizes_every_child() {
    let mut rt = runtime(80, 24);
    rt.split(Direction::Rows, None).unwrap();
    rt.resize(size(100, 40)).unwrap();

    for (rect, pane) in rt.visible() {
        assert_eq!(pane.size().cols, rect.cols, "pane {:?} has a stale width", rect.pane);
        assert_eq!(pane.size().rows, rect.rows, "pane {:?} has a stale height", rect.pane);
        // Pixel geometry scales with the pane's share, because image
        // protocols read it out of TIOCGWINSZ.
        assert!(pane.size().pixel_width > 0 && pane.size().pixel_height > 0);
    }
}

#[test]
fn zooming_tells_the_child_it_now_owns_the_screen() {
    let mut rt = runtime(80, 24);
    let zoomed = rt.split(Direction::Columns, None).unwrap().unwrap();
    let before = rt.pane(zoomed).unwrap().size();
    rt.toggle_zoom().unwrap();
    let after = rt.pane(zoomed).unwrap().size();
    assert!(after.cols > before.cols, "zoom must grow the pane: {before:?} -> {after:?}");
    assert_eq!(after.cols, 80);
    assert_eq!(rt.visible().len(), 1, "only the zoomed pane is drawn");

    rt.toggle_zoom().unwrap();
    assert_eq!(rt.pane(zoomed).unwrap().size().cols, before.cols, "unzoom must restore");
}

#[test]
fn input_goes_only_to_the_focused_pane() {
    let mut rt = runtime(40, 6);
    let first = rt.session.active_pane();
    let second = rt.split(Direction::Rows, None).unwrap().unwrap();
    assert_eq!(rt.session.active_pane(), second, "the new pane takes focus");

    rt.send(b"typed-into-second\n").unwrap();
    assert!(pump_until(&mut rt, |rt| pane_text(rt, second).contains("typed-into-second")));
    assert!(
        !pane_text(&rt, first).contains("typed-into-second"),
        "input leaked into the unfocused pane:\n{}",
        pane_text(&rt, first)
    );
}

#[test]
fn focus_movement_changes_where_input_lands() {
    let mut rt = runtime(40, 6);
    let first = rt.session.active_pane();
    rt.split(Direction::Columns, None).unwrap();
    assert!(rt.focus(Edge::Left).unwrap());
    assert_eq!(rt.session.active_pane(), first);
    rt.send(b"to-first\n").unwrap();
    assert!(pump_until(&mut rt, |rt| pane_text(rt, first).contains("to-first")));
}

#[test]
fn closing_a_pane_kills_its_child_and_reclaims_the_space() {
    let mut rt = runtime(80, 24);
    let first = rt.session.active_pane();
    let second = rt.split(Direction::Columns, None).unwrap().unwrap();

    assert!(!rt.close(second).unwrap(), "the session is not over");
    assert!(rt.pane(second).is_none(), "the pane's runtime must be gone");
    assert_eq!(rt.visible().len(), 1);
    assert_eq!(rt.pane(first).unwrap().size().cols, 80, "the survivor takes the space");
}

#[test]
fn a_child_that_exits_on_its_own_closes_its_pane() {
    let mut rt = MuxRuntime::new(SessionId(1), "test", size(40, 6), |request| {
        let size = request.size;
        Pty::spawn(&PtyConfig {
            program: Some("/bin/sh".into()),
            // Exits immediately, as a shell does when you type `exit`.
            args: vec!["-c".into(), "exit 0".into()],
            size,
            ..Default::default()
        })
    })
    .unwrap();

    let ended = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ended = false;
        while Instant::now() < deadline && !ended {
            let events = rt.pump(Duration::from_millis(50)).unwrap();
            ended = events.contains(&RuntimeEvent::SessionEnded);
        }
        ended
    };
    assert!(ended, "the last pane exiting must end the session");
    assert!(rt.is_empty());
}

#[test]
fn closing_the_last_pane_ends_the_session() {
    let mut rt = runtime(40, 6);
    assert!(rt.close_active().unwrap());
    assert!(rt.is_empty());
}

#[test]
fn windows_are_independent_and_switching_resizes_their_panes() {
    let mut rt = runtime(80, 24);
    let first = rt.session.active_pane();
    let second = rt.new_window(None).unwrap();
    assert_eq!(rt.session.windows.len(), 2);
    assert_eq!(rt.session.active_pane(), second);

    // Only the active window's pane is drawn...
    let visible: Vec<PaneId> = rt.visible().into_iter().map(|(r, _)| r.pane).collect();
    assert_eq!(visible, vec![second]);
    // ...but the other window's pane still exists and still has its child.
    assert!(rt.pane(first).is_some());

    rt.select_window(0).unwrap();
    assert_eq!(rt.session.active_pane(), first);
}
