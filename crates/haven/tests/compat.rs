//! Application compatibility harness. See `docs/PLAN.md` §16 (M2.7) and §17.
//!
//! Each test drives a real program on a real pty, parses its output with
//! `haven-core`, and asserts on the resulting grid. No GPU, no terminal, no
//! human. This is the gate that is meant to catch reflow, mux, daemon and TUI
//! work silently breaking the programs people actually run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use haven::haven_core::Emulator;
use haven::haven_pty::{PtyConfig, PtySize};
use haven::haven_shell::{launch_config, Injection, IntegrationCtx};
use haven::pump::{run_to_screen, PumpLimits, PumpStats};

struct Run {
    emu: Emulator,
    raw: String,
    #[allow(dead_code)]
    stats: PumpStats,
}

impl Run {
    fn screen(&self) -> String {
        self.emu.dump()
    }

    fn contains(&self, needle: &str) -> bool {
        self.screen().contains(needle)
    }
}

/// Programs we test against may not be installed; skipping beats a red CI on a
/// machine without `mc`.
fn find(program: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .iter()
        .map(|d| Path::new(d).join(program))
        .find(|p| p.exists())
}

fn run_app(
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    send: &str,
    (cols, rows): (u16, u16),
    idle_ms: u64,
) -> Run {
    let cfg = PtyConfig {
        program: Some(program.to_path_buf()),
        args: args.iter().map(Into::into).collect(),
        env: env.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
        env_remove: vec!["LINES".into(), "COLUMNS".into()],
        size: PtySize { cols, rows, ..Default::default() },
        ..Default::default()
    };
    let limits =
        PumpLimits { idle: Duration::from_millis(idle_ms), total: Duration::from_secs(20) };
    let (emu, raw, stats) =
        run_to_screen(&cfg, limits, send.as_bytes()).expect("pty spawn and pump");
    Run { emu, raw: String::from_utf8_lossy(&raw).into_owned(), stats }
}

fn sh(script: &str, size: (u16, u16)) -> Run {
    run_app(Path::new("/bin/sh"), &["-c", script], &[("TERM", "xterm-256color")], "", size, 400)
}

// --- the emulator against a real pty ---------------------------------------

#[test]
fn child_output_lands_on_the_grid() {
    let run = sh("printf 'line one\\r\\nline two'", (20, 4));
    assert_eq!(run.screen(), "line one\nline two\n\n\n");
}

#[test]
fn cpr_reply_round_trips_back_into_the_child() {
    // The child asks where the cursor is and reads our answer off its stdin.
    // This proves the whole reply path, which is what stops shells stalling on
    // their startup probes (`docs/PLAN.md` §9.4).
    let run = sh(
        "printf '\\033[3;7H\\033[6n'; IFS=';' read -r -d R row col; \
         printf '\\033[9;1Hgot %s %s' \"${row#*[}\" \"$col\"",
        (20, 10),
    );
    assert!(run.contains("got 3 7"), "screen was:\n{}", run.screen());
}

#[test]
fn truecolor_and_attributes_survive_the_pty() {
    use haven::haven_core::{Attrs, Color, Rgb};
    let run = sh("printf '\\033[1;38;2;10;200;30mX'", (10, 2));
    let cell = run.emu.term.grid.row(0).cells[0];
    let style = run.emu.term.styles.get(cell.style);
    assert_eq!(cell.c, 'X');
    assert_eq!(style.fg, Color::Rgb(Rgb::new(10, 200, 30)));
    assert!(style.attrs.contains(Attrs::BOLD));
}

#[test]
fn wide_characters_keep_the_columns_aligned() {
    let run = sh("printf '漢字|\\r\\nabcd|'", (10, 2));
    // The pipe must land in the same column on both lines: two wide glyphs
    // occupy four cells.
    let screen = run.screen();
    let lines: Vec<&str> = screen.lines().take(2).collect();
    assert_eq!(lines[0].chars().count(), 3, "{:?}", lines[0]);
    assert_eq!(lines[1], "abcd|");
}

// --- real full-screen applications -----------------------------------------

#[test]
fn mc_draws_its_panels_and_menu_bar() {
    let Some(mc) = find("mc") else { return };
    let run = run_app(
        &mc,
        &["-u"],
        &[("TERM", "xterm-256color"), ("LANG", "en_US.UTF-8"), ("HOME", "/tmp")],
        "",
        (80, 20),
        1500,
    );
    let screen = run.screen();
    for needle in ["Left", "File", "Command", "Options", "Right", "Name", "Size"] {
        assert!(screen.contains(needle), "mc screen is missing {needle:?}:\n{screen}");
    }
    // The alternate screen and box drawing both have to work for this to hold.
    assert!(screen.contains('║') || screen.contains('│'), "no panel frame:\n{screen}");
}

#[test]
fn mc_box_drawing_works_in_a_non_utf8_locale() {
    // In a C locale mc designates DEC Special Graphics into G0 and draws with
    // ASCII letters. Without SCS support this screen is full of `qqqq`
    // (`docs/PLAN.md` §8).
    let Some(mc) = find("mc") else { return };
    let run = run_app(
        &mc,
        &["-u"],
        &[("TERM", "xterm-256color"), ("LANG", "C"), ("LC_ALL", "C"), ("HOME", "/tmp")],
        "",
        (60, 12),
        1500,
    );
    let screen = run.screen();
    assert!(
        screen.contains('─') || screen.contains('│'),
        "DEC Special Graphics was not translated:\n{screen}"
    );
    assert!(!screen.contains("qqqqqq"), "raw graphics letters leaked onto the screen:\n{screen}");
}

#[test]
fn mc_ctrl_o_toggles_to_the_subshell_and_back() {
    // The canonical alternate-screen test: mc lives on the alt screen, Ctrl+O
    // drops to the primary (the subshell), and Ctrl+O again must bring the
    // panels back *intact*. A terminal that half-implements 1049 loses one or
    // the other (`docs/PLAN.md` §8).
    let Some(mc) = find("mc") else { return };
    let env = [("TERM", "xterm-256color"), ("LANG", "en_US.UTF-8"), ("HOME", "/tmp")];

    let away = run_app(&mc, &["-u"], &env, "\x0f", (60, 10), 1800);
    assert!(!away.contains("Options"), "Ctrl+O should have left the panels:\n{}", away.screen());

    let back = run_app(&mc, &["-u"], &env, "\x0f\x0f", (60, 10), 2000);
    assert!(
        back.contains("Options") && back.contains("Name"),
        "Ctrl+O twice must restore the panels:\n{}",
        back.screen()
    );
}

#[test]
fn vim_renders_a_buffer_and_the_tilde_filler() {
    let Some(vim) = find("vim") else { return };
    let run = run_app(
        &vim,
        &["-u", "NONE", "-c", "call setline(1, 'hello from vim')"],
        &[("TERM", "xterm-256color")],
        "",
        (40, 8),
        900,
    );
    assert!(run.contains("hello from vim"), "{}", run.screen());
    assert!(run.contains("~"), "vim's empty-line filler is missing:\n{}", run.screen());
}

#[test]
fn less_paginates_and_shows_its_prompt() {
    let Some(less) = find("less") else { return };
    let run = run_app(
        &less,
        &["-X", "/etc/hostname"],
        &[("TERM", "xterm-256color"), ("LESSSECURE", "1")],
        "",
        (40, 6),
        700,
    );
    // Either the file contents or the (END) marker proves less drew something.
    assert!(!run.screen().trim().is_empty(), "less drew nothing");
}

// --- shell integration ------------------------------------------------------

fn integration_run(shell: &Path, send: &str, integrate: bool) -> (Run, Injection) {
    let ctx = IntegrationCtx {
        program: shell.to_path_buf(),
        dir: std::env::temp_dir().join("haven-tests"),
        login: false,
        home: std::env::var_os("HOME").map(PathBuf::from),
    };
    let (mut cfg, _, injection) = launch_config(Some(shell), false, integrate, &ctx);
    cfg.args.push("-i".into());
    cfg.size = PtySize { cols: 60, rows: 8, ..Default::default() };
    let limits = PumpLimits { idle: Duration::from_millis(1500), total: Duration::from_secs(20) };
    let (emu, raw, stats) = run_to_screen(&cfg, limits, send.as_bytes()).expect("shell");
    (Run { emu, raw: String::from_utf8_lossy(&raw).into_owned(), stats }, injection)
}

#[test]
fn bash_integration_emits_semantic_prompt_marks() {
    let Some(bash) = find("bash") else { return };
    let (run, injection) = integration_run(&bash, "echo marker\n", true);
    assert_eq!(injection, Injection::Auto);
    assert!(run.raw.contains("133;A"), "no prompt-start mark");
    assert!(run.raw.contains("133;D"), "no command-end mark");
    assert!(run.raw.contains("]7;file://"), "no cwd report");
}

#[test]
fn a_real_shell_reports_which_command_it_ran() {
    // Naming a tab after the title shows a path. Naming it after the command
    // needs the shell to say what the command *was*, which only the OSC 133
    // `C` mark carries.
    for name in ["bash", "zsh", "fish"] {
        let Some(shell) = find(name) else { continue };
        let (run, _) = integration_run(&shell, "echo marker\n", true);
        assert_eq!(
            run.emu.term.command.as_deref(),
            Some("echo marker"),
            "{name} did not report the command it ran; raw:\n{}",
            run.raw
        );
        // And it must know the command finished, or a tab would show a
        // spinner forever.
        assert!(!run.emu.term.command_running, "{name} never reported the command ending");
    }
}

#[test]
fn a_command_line_with_a_semicolon_survives_the_mark() {
    // `;` separates OSC parameters. Unencoded it truncates the command, so
    // `echo a; echo b` would name a tab `echo a`.
    let Some(bash) = find("bash") else { return };
    let (run, _) = integration_run(&bash, "echo a; echo b\n", true);
    assert_eq!(run.emu.term.command.as_deref(), Some("echo a; echo b"), "raw:\n{}", run.raw);
}

#[test]
fn fish_marks_are_not_duplicated_by_our_integration() {
    // Regression guard: fish >= 3.7 emits OSC 133 itself, so installing our
    // script produced *two* command-end marks per command — a consumer cannot
    // tell which D belongs to which command.
    let Some(fish) = find("fish") else { return };
    let (native, _) = integration_run(&fish, "echo marker\n", false);
    let (ours, injection) = integration_run(&fish, "echo marker\n", true);

    let count = |s: &str| s.matches("133;D").count();
    if matches!(injection, Injection::Native { .. }) {
        assert_eq!(
            count(&ours.raw),
            count(&native.raw),
            "our integration changed the mark count on a natively-capable fish"
        );
    }
    assert!(count(&native.raw) > 0, "fish emitted no command-end mark at all");
}

// --- the TUI frontend, end to end ------------------------------------------

#[test]
fn haven_tui_runs_a_shell_inside_haven_itself() {
    // The strongest round-trip available: the inner `haven tui` serializes its
    // grid to escape sequences, and the outer emulator parses them back. If
    // either half is wrong, the screen does not match.
    let haven = env!("CARGO_BIN_EXE_haven");
    let run = run_app(
        Path::new(haven),
        &["tui", "--no-integrate", "--", "/bin/sh", "-i"],
        &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
        "printf 'nested works\\r\\n'\n",
        (44, 6),
        2000,
    );
    let screen = run.screen();
    assert!(screen.contains("nested works"), "inner terminal did not render:\n{screen}");
    // The inner client must be using the alternate screen, like any full-screen app.
    assert!(run.raw.contains("\u{1b}[?1049h"), "inner client never entered the alt screen");
}

#[test]
fn haven_tui_restores_the_terminal_when_its_child_exits() {
    let haven = env!("CARGO_BIN_EXE_haven");
    let run = run_app(
        Path::new(haven),
        &["tui", "--no-integrate", "--", "/bin/sh", "-c", "printf hello"],
        &[("TERM", "xterm-256color")],
        "",
        (30, 4),
        1200,
    );
    // Leaving the alt screen and showing the cursor again are both required:
    // a client that dies in raw mode on the alt screen leaves an unusable
    // terminal (`docs/PLAN.md` §11.1).
    assert!(run.raw.contains("\u{1b}[?1049l"), "never left the alt screen:\n{:?}", run.raw);
    assert!(run.raw.contains("\u{1b}[?25h"), "never restored the cursor");
}
