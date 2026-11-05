//! The round-trip identity test from `docs/PLAN.md` §17:
//!
//!   core -> haven-vtwrite -> vte parser -> core
//!
//! must be the identity. One property validates the parser, the serializer and
//! the cell model against each other, which is why the plan puts the
//! serializer early (M2.5) rather than with the TUI frontend.

use haven_core::Emulator;
use haven_vtwrite::{snapshot, Caps, ColorDepth, Serializer};

/// Byte streams that exercise the parts of the model worth protecting.
fn corpus() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("plain text", b"hello world" as &[u8]),
        ("multiple lines", b"one\r\ntwo\r\nthree"),
        ("sgr basics", b"\x1b[1mbold\x1b[0m plain \x1b[4munder\x1b[24m"),
        ("indexed colour", b"\x1b[31mred\x1b[92mbright\x1b[38;5;200mcube\x1b[m"),
        ("truecolor", b"\x1b[38;2;10;20;30mfg\x1b[48;2;200;100;50mbg\x1b[m"),
        ("colon sgr", b"\x1b[38:2::9:8:7mx\x1b[4:3my\x1b[58:5:12mz\x1b[m"),
        ("wide glyphs", "漢字 mixed 日本語".as_bytes()),
        ("combining marks", "e\u{0301}clair nai\u{0308}ve".as_bytes()),
        ("dec graphics", b"\x1b(0lqqqk\x1b(B text \x1b(0mqqqj\x1b(B"),
        ("erase ops", b"abcdefgh\x1b[1;4H\x1b[K\x1b[2;1Hxyz\x1b[2;2H\x1b[1K"),
        ("insert delete", b"abcdef\x1b[1;3H\x1b[2P\x1b[1;1H\x1b[3@"),
        ("scroll region", b"\x1b[2;4r\x1b[2;1Ha\r\nb\r\nc\r\nd"),
        ("cursor save restore", b"abc\x1b7\x1b[5;5Hxyz\x1b8Q"),
        ("tabs", b"a\tb\tc"),
        ("rep", b"-\x1b[20b"),
        ("reverse and dim", b"\x1b[7mrev\x1b[27m \x1b[2mdim\x1b[22m"),
        ("wrapping", b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    ]
}

fn round_trip(bytes: &[u8], caps: Caps, cols: usize, rows: usize) {
    let mut source = Emulator::new(cols, rows);
    source.feed(bytes);

    let mut serializer = Serializer::new(caps);
    let emitted = serializer.frame(&source.term.grid, &source.term.styles);

    let mut replayed = Emulator::new(cols, rows);
    replayed.feed(&emitted);

    let want = snapshot(&source.term.grid, &source.term.styles);
    let got = snapshot(&replayed.term.grid, &replayed.term.styles);

    if want != got {
        // Report the first differing cell: a whole-grid dump is unreadable.
        for (r, (a, b)) in want.iter().zip(got.iter()).enumerate() {
            for (c, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert_eq!(
                    x,
                    y,
                    "cell ({r},{c}) differs\nemitted: {:?}\nsource screen:\n{}\nreplayed screen:\n{}",
                    String::from_utf8_lossy(&emitted).escape_debug().to_string(),
                    source.dump(),
                    replayed.dump(),
                );
            }
        }
        panic!("snapshots differ in shape: {} vs {} rows", want.len(), got.len());
    }
}

#[test]
fn the_corpus_round_trips_exactly() {
    for (name, bytes) in corpus() {
        for (cols, rows) in [(20usize, 6usize), (40, 10), (7, 4)] {
            std::panic::catch_unwind(|| round_trip(bytes, Caps::default(), cols, rows))
                .unwrap_or_else(|_| panic!("`{name}` failed to round-trip at {cols}x{rows}"));
        }
    }
}

#[test]
fn the_corpus_round_trips_with_synchronized_output() {
    let caps = Caps { synchronized_output: true, ..Default::default() };
    for (name, bytes) in corpus() {
        std::panic::catch_unwind(|| round_trip(bytes, caps, 24, 6))
            .unwrap_or_else(|_| panic!("`{name}` failed with sync markers"));
    }
}

#[test]
fn an_unchanged_frame_costs_almost_nothing() {
    let mut emu = Emulator::new(40, 10);
    emu.feed(b"some content here\r\nand a second line");
    let mut s = Serializer::new(Caps::default());

    let first = s.frame(&emu.term.grid, &emu.term.styles);
    assert!(first.len() > 20, "the first frame is a full repaint");

    let second = s.frame(&emu.term.grid, &emu.term.styles);
    // Nothing changed, so at most a cursor move should be emitted.
    assert!(second.len() <= 8, "idle frame emitted {} bytes: {second:?}", second.len());
}

#[test]
fn a_single_changed_cell_does_not_repaint_the_screen() {
    let mut emu = Emulator::new(40, 10);
    emu.feed(b"line one\r\nline two\r\nline three");
    let mut s = Serializer::new(Caps::default());
    let _ = s.frame(&emu.term.grid, &emu.term.styles);

    emu.feed(b"\x1b[2;1HX");
    let delta = s.frame(&emu.term.grid, &emu.term.styles);
    let text = String::from_utf8_lossy(&delta);
    assert!(!text.contains("2J"), "a one-cell change must not clear the screen: {text:?}");
    assert!(delta.len() < 24, "delta was {} bytes: {text:?}", delta.len());
    assert!(text.contains('X'));
}

#[test]
fn a_scrolled_screen_is_emitted_as_a_scroll_not_a_repaint() {
    let mut emu = Emulator::new(20, 6);
    for i in 0..6 {
        emu.feed(format!("line {i}\r\n").as_bytes());
    }
    let mut s = Serializer::new(Caps::default());
    let _ = s.frame(&emu.term.grid, &emu.term.styles);

    // One more line scrolls the screen by one row.
    emu.feed(b"line 6\r\n");
    let delta = s.frame(&emu.term.grid, &emu.term.styles);
    let text = String::from_utf8_lossy(&delta);
    assert!(text.contains("\x1b[1S"), "expected a scroll-up, got {text:?}");
    // And the result still matches cell for cell.
    let mut replayed = Emulator::new(20, 6);
    let mut fresh = Serializer::new(Caps::default());
    replayed.feed(&fresh.frame(&emu.term.grid, &emu.term.styles));
    assert_eq!(replayed.dump(), emu.dump());
}

#[test]
fn synchronized_output_brackets_the_whole_frame() {
    let mut emu = Emulator::new(10, 2);
    emu.feed(b"hi");
    let mut s = Serializer::new(Caps { synchronized_output: true, ..Default::default() });
    let out = String::from_utf8(s.frame(&emu.term.grid, &emu.term.styles)).unwrap();
    assert!(out.starts_with("\x1b[?2026h"), "{out:?}");
    assert!(out.ends_with("\x1b[?2026l"), "{out:?}");
}

#[test]
fn a_16_colour_host_never_receives_truecolor() {
    let mut emu = Emulator::new(20, 2);
    emu.feed(b"\x1b[38;2;12;240;90mgreenish\x1b[48;2;10;10;200mon blue\x1b[m");
    let mut s = Serializer::new(Caps { depth: ColorDepth::Ansi16, ..Default::default() });
    let out = String::from_utf8(s.frame(&emu.term.grid, &emu.term.styles)).unwrap();
    assert!(!out.contains("38;2"), "truecolor leaked to a 16-colour host: {out:?}");
    assert!(!out.contains("38;5"), "256-colour indices leaked: {out:?}");
}

#[test]
fn resizing_forces_a_full_repaint_rather_than_a_corrupt_delta() {
    let mut emu = Emulator::new(20, 4);
    emu.feed(b"before resize");
    let mut s = Serializer::new(Caps::default());
    let _ = s.frame(&emu.term.grid, &emu.term.styles);

    emu.term.resize(30, 6);
    let after = String::from_utf8(s.frame(&emu.term.grid, &emu.term.styles)).unwrap();
    assert!(after.contains("2J"), "a geometry change must repaint: {after:?}");
}

/// A split screen: two panes stacked, only the lower one scrolling.
///
/// This is the shape the scroll optimisation gets wrong. `SU` moves the
/// whole screen, and a pane is not the whole screen.
fn split_frame(
    rows: usize,
    cols: usize,
    top_text: &str,
    bottom: &[String],
) -> Vec<Vec<haven_vtwrite::RenderCell>> {
    let mut out = Vec::new();
    let line = |text: &str| -> Vec<haven_vtwrite::RenderCell> {
        let mut row = vec![haven_vtwrite::RenderCell::blank(); cols];
        for (i, c) in text.chars().take(cols).enumerate() {
            row[i] = haven_vtwrite::RenderCell::new(c.to_string(), Default::default());
        }
        row
    };
    out.push(line(top_text));
    for _ in 1..rows / 2 {
        out.push(line(""));
    }
    for i in 0..rows - rows / 2 {
        out.push(line(bottom.get(i).map(String::as_str).unwrap_or("")));
    }
    out
}

#[test]
fn a_pane_that_scrolls_does_not_scroll_the_whole_screen() {
    // Pressing Enter in the lower pane of a split scrolled the entire
    // window, because the scroll optimisation emitted `SU` for the whole
    // screen — which moves every other pane, the status bar, and the top
    // line into the *host* terminal's scrollback.
    let (rows, cols) = (12, 40);
    let mut serializer = Serializer::new(Caps::default());
    let mut host = Emulator::new(cols, rows);

    let bottom: Vec<String> = (0..6).map(|i| format!("$ line {i}")).collect();
    let first = split_frame(rows, cols, "TOP PANE - must not move", &bottom);
    host.feed(&serializer.frame_snapshot(first, (0, 0)));

    let scrolled: Vec<String> = (1..8).map(|i| format!("$ line {i}")).collect();
    let second = split_frame(rows, cols, "TOP PANE - must not move", &scrolled);
    let out = serializer.frame_snapshot(second.clone(), (0, 0));
    let text = String::from_utf8_lossy(&out).into_owned();
    host.feed(&out);

    // The scroll is confined to the rows that moved, and the region is
    // handed back afterwards.
    if text.contains("\x1b[1S") || text.contains("\x1b[1T") {
        assert!(text.contains("r\u{1b}[1S") || text.contains(";12r"), "no region: {text:?}");
        assert!(text.contains("\x1b[r"), "the scroll region was never reset: {text:?}");
    }

    // And the picture is right, which is the part worth proving: the top
    // pane says what it said, and the lower one has moved on.
    let got = snapshot(&host.term.grid, &host.term.styles);
    let row_text = |row: &[haven_vtwrite::RenderCell]| -> String {
        row.iter().map(|c| c.text.clone()).collect::<String>().trim_end().to_string()
    };
    assert_eq!(row_text(&got[0]), "TOP PANE - must not move", "the top pane moved");
    for (i, row) in got.iter().enumerate() {
        assert_eq!(row_text(row), row_text(&second[i]), "row {i} differs");
    }
}
