//! The mux driven through the TUI frontend, end to end: real ptys, real
//! escape sequences, parsed back by `haven-core`.
//!
//! Every test runs `haven tui` inside a pty we own and asserts on the screen
//! the inner client painted — `docs/PLAN.md` §17's "the TUI client is fully
//! testable headlessly".

use std::path::Path;
use std::time::Duration;

use haven::haven_core::Emulator;
use haven::haven_pty::{PtyConfig, PtySize};
use haven::pump::{run_to_screen, PumpLimits};

/// Ctrl+A, the prefix.
const PREFIX: &str = "\x01";

fn tui(send: &str, cols: u16, rows: u16, idle_ms: u64) -> Emulator {
    let cfg = PtyConfig {
        program: Some(Path::new(env!("CARGO_BIN_EXE_haven")).to_path_buf()),
        args: vec![
            "tui".into(),
            "--no-integrate".into(),
            "--".into(),
            "/bin/sh".into(),
            "-i".into(),
        ],
        env: vec![("TERM".into(), "xterm-256color".into()), ("PS1".into(), "$ ".into())],
        env_remove: vec!["LINES".into(), "COLUMNS".into()],
        size: PtySize { cols, rows, ..Default::default() },
        ..Default::default()
    };
    let limits =
        PumpLimits { idle: Duration::from_millis(idle_ms), total: Duration::from_secs(25) };
    let (emu, _, _) = run_to_screen(&cfg, limits, send.as_bytes()).expect("run haven tui");
    emu
}

#[test]
fn splitting_columns_gives_two_independent_shells() {
    let keys = format!("echo LEFT\n{PREFIX}%echo RIGHT\n");
    let screen = tui(&keys, 70, 12, 2500).dump();

    // Both shells ran their own command...
    assert!(screen.contains("LEFT"), "left pane missing:\n{screen}");
    assert!(screen.contains("RIGHT"), "right pane missing:\n{screen}");
    // ...and a separator divides them, on the same row as both outputs.
    let divided = screen.lines().filter(|line| line.contains('│')).count();
    assert!(divided >= 2, "no separator column:\n{screen}");
}

#[test]
fn splitting_rows_stacks_the_panes() {
    let keys = format!("echo TOPPANE\n{PREFIX}\"echo LOWPANE\n");
    let screen = tui(&keys, 50, 12, 2500).dump();
    let row_of = |needle: &str| screen.lines().position(|l| l.contains(needle));
    let (top, low) = (row_of("TOPPANE"), row_of("LOWPANE"));
    assert!(top.is_some() && low.is_some(), "both panes should have output:\n{screen}");
    assert!(top < low, "the second pane must be below the first:\n{screen}");
    assert!(screen.contains('─'), "no horizontal separator:\n{screen}");
}

#[test]
fn the_status_line_shows_window_tabs() {
    let keys = format!("{PREFIX}c");
    let screen = tui(&keys, 60, 10, 2500);
    let status = screen.dump().lines().last().unwrap_or_default().to_string();
    assert!(status.contains("haven"), "no session name in the status line: {status:?}");
    assert!(status.contains("1:"), "no first window tab: {status:?}");
    assert!(status.contains("2:"), "a new window should add a tab: {status:?}");
}

#[test]
fn zoom_fills_the_screen_and_is_marked_in_the_status_line() {
    let keys = format!("{PREFIX}%echo ZOOMED\n{PREFIX}z");
    let screen = tui(&keys, 60, 8, 2500).dump();
    assert!(screen.contains("ZOOMED"), "{screen}");
    // A zoomed pane owns the whole area, so no separator is drawn.
    assert!(!screen.contains('│'), "zoom should hide the separator:\n{screen}");
    let status = screen.lines().last().unwrap_or_default();
    assert!(status.contains('Z'), "zoom indicator missing: {status:?}");
}

#[test]
fn closing_a_pane_returns_the_space_to_its_neighbour() {
    // Split, then close the new pane: the separator goes away again.
    let keys = format!("echo KEEPME\n{PREFIX}%{PREFIX}x");
    let screen = tui(&keys, 60, 8, 2500).dump();
    assert!(screen.contains("KEEPME"), "the surviving pane lost its content:\n{screen}");
    assert!(!screen.contains('│'), "separator should be gone after closing:\n{screen}");
}

#[test]
fn an_unbound_key_after_the_prefix_does_not_leak_into_the_pane() {
    // `Ctrl+A` then `Q` is unbound: the Q must not reach the shell.
    let keys = format!("{PREFIX}Qecho AFTER\n");
    let screen = tui(&keys, 50, 8, 2500).dump();
    assert!(screen.contains("AFTER"), "the shell stopped accepting input:\n{screen}");
    assert!(!screen.contains("Qecho"), "the unbound key leaked through:\n{screen}");
}
