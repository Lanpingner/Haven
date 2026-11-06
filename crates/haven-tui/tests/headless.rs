//! The TUI frontend, tested with no terminal at all.
//!
//! `docs/PLAN.md` §17: render a frame, parse the frontend's own output with
//! `haven-core`, and assert on the resulting grid. No tty, no GPU, no human.

use haven_client::frame::{FrameModel, FrameRow, Span, Widget, WidgetKind};
use haven_client::frontend::Frontend;
use haven_client::input::InputEvent;
use haven_core::{Emulator, Style};
use haven_proto::{CellSize, PaneId, PaneRect};
use haven_tui::{HostCaps, TuiFrontend};

fn size(cols: u16, rows: u16) -> CellSize {
    CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
}

fn row(text: &str) -> FrameRow {
    FrameRow {
        spans: vec![Span { col: 0, text: text.into(), style: Style::default(), link: None }],
        rev: 1,
    }
}

fn pane(frame: &mut FrameModel, id: u32, rect: (u16, u16, u16, u16), lines: &[&str]) {
    let rect = PaneRect { pane: PaneId(id), col: rect.0, row: rect.1, cols: rect.2, rows: rect.3 };
    frame.apply_layout(&haven_proto::Layout {
        window: 0,
        panes: frame.panes.values().map(|p| p.rect).chain(std::iter::once(rect)).collect(),
        active: Some(PaneId(id)),
    });
    let entry = frame.panes.get_mut(&id).expect("pane exists after layout");
    entry.rows = lines.iter().map(|l| row(l)).collect();
}

/// Render a frame and read the result back through the parser.
fn render_and_parse(frontend: &mut TuiFrontend, frame: &FrameModel) -> String {
    let bytes = frontend.render(frame);
    let mut emu = Emulator::new(frame.size.cols as usize, frame.size.rows as usize);
    emu.feed(&bytes);
    emu.dump()
}

#[test]
fn a_single_pane_renders_its_text() {
    let mut frontend = TuiFrontend::detached(size(20, 3), HostCaps::default());
    let mut frame = FrameModel::new(size(20, 3));
    pane(&mut frame, 1, (0, 0, 20, 3), &["hello tui", "second line"]);

    assert_eq!(render_and_parse(&mut frontend, &frame), "hello tui\nsecond line\n\n");
}

#[test]
fn two_panes_are_composited_side_by_side_with_a_separator() {
    let mut frontend = TuiFrontend::detached(size(21, 2), HostCaps::default());
    let mut frame = FrameModel::new(size(21, 2));
    pane(&mut frame, 1, (0, 0, 10, 2), &["left", "LEFT"]);
    pane(&mut frame, 2, (11, 0, 10, 2), &["right", "RIGHT"]);

    let screen = render_and_parse(&mut frontend, &frame);
    let lines: Vec<&str> = screen.lines().collect();
    // The separator sits in the column between the two panes.
    assert_eq!(lines[0], "left      │right");
    assert_eq!(lines[1], "LEFT      │RIGHT");
}

#[test]
fn a_pane_cannot_draw_outside_its_rectangle() {
    let mut frontend = TuiFrontend::detached(size(20, 2), HostCaps::default());
    let mut frame = FrameModel::new(size(20, 2));
    pane(&mut frame, 1, (0, 0, 5, 2), &["this text is far too long"]);

    let screen = render_and_parse(&mut frontend, &frame);
    // Clipped at 5 columns. There is no separator: a gap is only a rule when
    // there is a pane on each side of it, and here there is nothing to the
    // right at all.
    assert_eq!(screen.lines().next().unwrap(), "this", "pane overflowed its rect");
}

#[test]
fn widgets_draw_over_the_panes() {
    let mut frontend = TuiFrontend::detached(size(20, 3), HostCaps::default());
    let mut frame = FrameModel::new(size(20, 3));
    pane(&mut frame, 1, (0, 0, 20, 3), &["content", "content", "content"]);
    frame.widgets.push(Widget {
        kind: WidgetKind::StatusLine,
        rect: PaneRect { pane: PaneId(0), col: 0, row: 2, cols: 20, rows: 1 },
        rows: vec![row("[status line]")],
    });

    let screen = render_and_parse(&mut frontend, &frame);
    assert_eq!(screen.lines().nth(2).unwrap(), "[status line]");
}

#[test]
fn the_cursor_is_placed_inside_the_focused_pane() {
    let mut frontend = TuiFrontend::detached(size(21, 3), HostCaps::default());
    let mut frame = FrameModel::new(size(21, 3));
    pane(&mut frame, 1, (0, 0, 10, 3), &["a"]);
    pane(&mut frame, 2, (11, 0, 10, 3), &["b"]);
    // Pane 2 is active (the last `pane()` call sets it) with its cursor at 1,2.
    frame.panes.get_mut(&2).unwrap().cursor.row = 1;
    frame.panes.get_mut(&2).unwrap().cursor.col = 2;

    let bytes = frontend.render(&frame);
    let mut emu = Emulator::new(21, 3);
    emu.feed(&bytes);
    // Screen coordinates: pane origin (11,0) plus the pane-local cursor.
    assert_eq!((emu.term.grid.cursor.row, emu.term.grid.cursor.col), (1, 13));
}

#[test]
fn an_unchanged_frame_emits_almost_nothing() {
    let mut frontend = TuiFrontend::detached(size(40, 10), HostCaps::default());
    let mut frame = FrameModel::new(size(40, 10));
    pane(&mut frame, 1, (0, 0, 40, 10), &["some content", "more content"]);

    let first = frontend.render(&frame);
    assert!(first.len() > 20);
    let second = frontend.render(&frame);
    assert!(second.len() <= 8, "idle frame cost {} bytes", second.len());
}

#[test]
fn synchronized_output_is_used_when_the_host_supports_it() {
    let host = HostCaps { synchronized_output: true, ..Default::default() };
    let mut frontend = TuiFrontend::detached(size(10, 2), host);
    let mut frame = FrameModel::new(size(10, 2));
    pane(&mut frame, 1, (0, 0, 10, 2), &["hi"]);

    let out = String::from_utf8(frontend.render(&frame)).unwrap();
    assert!(out.starts_with("\x1b[?2026h") && out.ends_with("\x1b[?2026l"), "{out:?}");
}

#[test]
fn input_fed_to_the_frontend_decodes_into_events() {
    let mut frontend = TuiFrontend::detached(size(10, 2), HostCaps::default());
    frontend.feed_input(b"a\x1b[B\x1b[200~pasted\x1b[201~");

    let mut events = Vec::new();
    while let Some(event) = frontend.next_input() {
        events.push(event);
    }
    assert!(matches!(events[0], InputEvent::Key(_)));
    assert!(matches!(events[1], InputEvent::Key(_)));
    assert_eq!(events[2], InputEvent::Paste("pasted".into()));
}

#[test]
fn a_16_colour_host_gets_no_truecolor_from_the_frontend() {
    use haven_core::{Color, Rgb};
    use haven_vtwrite::ColorDepth;

    let host = HostCaps { depth: ColorDepth::Ansi16, ..Default::default() };
    let mut frontend = TuiFrontend::detached(size(20, 2), host);
    let mut frame = FrameModel::new(size(20, 2));
    pane(&mut frame, 1, (0, 0, 20, 2), &["x"]);
    frame.panes.get_mut(&1).unwrap().rows[0].spans[0].style =
        Style { fg: Color::Rgb(Rgb::new(10, 200, 30)), ..Default::default() };

    let out = String::from_utf8(frontend.render(&frame)).unwrap();
    assert!(!out.contains("38;2"), "truecolor reached a 16-colour host: {out:?}");
}
