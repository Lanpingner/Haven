//! Haven CLI. For now: `dump` (headless screen capture) and `shell-info`.
//! The GPU client lands with M1 (`docs/PLAN.md` §16).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use haven_pty::PtySize;
use haven_shell::{launch_config, IntegrationCtx};

use haven::pump::{run_to_screen, PumpLimits};

const USAGE: &str = "\
haven — terminal emulator (work in progress)

usage:
  haven gpu [options]               attach in a GPU window
  haven attach [options]            attach to the daemon (starts it if needed)
  haven ls                          list sessions
  haven show-env                    the session's environment, as exports
  haven kill-session -t <id>        end one session
  haven server run|stop             run or stop the daemon
  haven tui [options]               standalone: the mux with no daemon
  haven dump [options]              headless: print a program's final screen
  haven screenshot [options]        headless: render a program on the GPU to a file
  haven shell-info [--login]        what a pane would launch

options:
  --cols <n>       columns (default 80)
  --rows <n>       rows (default 24)
  --idle-ms <n>    stop after this long without output (default 250)
  --login          launch as a login shell
  --no-integrate   skip shell integration
  --send <text>    type this at the child; \\n \\r \\t \\e \\xNN are decoded
  --socket <path>  daemon socket (default: $XDG_RUNTIME_DIR/haven/<uid>-default.sock)
  -t <id>          session id, for attach and kill-session
  --out <path>     screenshot output (PPM; default haven.ppm)
  --gpu <backend>  auto (default), gl or vulkan
  --gpu-power <p>  low (default, the integrated GPU) or high (discrete).
                   On a hybrid laptop the discrete GPU reaches the display
                   over PRIME, and presenting there while the window manager
                   unmaps the window freezes the whole X server for seconds.
  --font <name>    font family
  --font-size <n>  pixel size (default 16)
  --raw            also print the raw byte stream, escaped
  -h, --help       this message
";

struct Args {
    cols: u16,
    rows: u16,
    idle_ms: u64,
    login: bool,
    integrate: bool,
    raw: bool,
    send: Vec<u8>,
    socket: Option<PathBuf>,
    out: Option<PathBuf>,
    font: Option<String>,
    font_size: f32,
    gpu: haven_render::GpuPreference,
    target: Option<u32>,
    subcommand: Option<String>,
    program: Option<PathBuf>,
    rest: Vec<OsString>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            idle_ms: 250,
            login: false,
            integrate: true,
            raw: false,
            send: Vec::new(),
            socket: None,
            out: None,
            font: None,
            font_size: 16.0,
            gpu: haven_render::GpuPreference::default(),
            target: None,
            subcommand: None,
            program: None,
            rest: Vec::new(),
        }
    }
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<(String, Args), String> {
    let cmd = it.next().unwrap_or_else(|| "help".into());
    let mut args = Args::default();
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--cols" => args.cols = value("--cols")?.parse().map_err(|_| "bad --cols")?,
            "--rows" => args.rows = value("--rows")?.parse().map_err(|_| "bad --rows")?,
            "--idle-ms" => {
                args.idle_ms = value("--idle-ms")?.parse().map_err(|_| "bad --idle-ms")?
            }
            "--login" => args.login = true,
            "--no-integrate" => args.integrate = false,
            "--raw" => args.raw = true,
            "--send" => args.send = decode_escapes(&value("--send")?),
            "--socket" => args.socket = Some(PathBuf::from(value("--socket")?)),
            "--out" => args.out = Some(PathBuf::from(value("--out")?)),
            "--font" => args.font = Some(value("--font")?),
            "--gpu" => args.gpu = args.gpu.with_backend(&value("--gpu")?)?,
            "--gpu-power" => args.gpu = args.gpu.with_power(&value("--gpu-power")?)?,
            "--font-size" => {
                args.font_size = value("--font-size")?.parse().map_err(|_| "bad --font-size")?
            }
            "-t" | "--target" => {
                args.target = Some(value("-t")?.parse().map_err(|_| "bad -t: expected a number")?)
            }
            "-h" | "--help" => return Ok(("help".into(), args)),
            "--" => {
                let mut rest: Vec<OsString> = it.map(OsString::from).collect();
                if !rest.is_empty() {
                    args.program = Some(PathBuf::from(rest.remove(0)));
                    args.rest = rest;
                }
                break;
            }
            // A bare word is a subcommand, as in `haven server run`.
            other if !other.starts_with('-') && args.subcommand.is_none() => {
                args.subcommand = Some(other.to_string())
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok((cmd, args))
}

/// Decode the small escape vocabulary a test script needs.
fn decode_escapes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('0') => out.push(0),
            // \xNN: the only way to type a raw control byte such as mc's
            // Ctrl+O (0x0f) from a shell script.
            Some('x') => {
                let hex: String = chars.clone().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) if hex.len() == 2 => {
                        out.push(byte);
                        chars.next();
                        chars.next();
                    }
                    _ => out.push(b'x'),
                }
            }
            Some('\\') => out.push(b'\\'),
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("haven")
}

fn ctx(login: bool) -> IntegrationCtx {
    IntegrationCtx {
        // Overwritten by `launch_config` once the shell is resolved.
        program: PathBuf::new(),
        dir: runtime_dir(),
        login,
        home: std::env::var_os("HOME").map(PathBuf::from),
    }
}

fn main() -> ExitCode {
    let (cmd, args) = match parse(std::env::args().skip(1)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("haven: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match cmd.as_str() {
        "gpu" => gpu(args),
        "attach" => attach(args),
        "ls" => list(args),
        "kill-session" => kill_session(args),
        "show-env" | "show-environment" => show_env(args),
        "server" => server(args),
        "tui" => tui(args),
        "dump" => dump(args),
        "screenshot" => screenshot(args),
        "shell-info" => shell_info(args),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("haven: unknown command {other:?}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn socket_of(args: &Args) -> PathBuf {
    args.socket.clone().unwrap_or_else(|| haven_daemon::socket_path("default"))
}

/// Attach with a GPU window.
fn gpu(args: Args) -> ExitCode {
    let socket = socket_of(&args);
    let session = args.target.map(haven_proto::SessionId);
    let config = haven_render::WindowConfig {
        title: "Haven".into(),
        cols: args.cols,
        rows: args.rows,
        font: args.font.clone(),
        font_size: args.font_size,
        gpu: args.gpu,
    };
    match haven::attach::run_gpu(&socket, session, config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Attach to the daemon, starting one if it is not running.
fn attach(args: Args) -> ExitCode {
    let socket = socket_of(&args);
    let session = args.target.map(haven_proto::SessionId);
    match haven::attach::run(&socket, session, haven_tui::HostCaps::from_env()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The daemon holds the terminal emulator and outlives every client, so a
/// rebuild changes nothing until it is restarted. That looks exactly like a
/// fix that did not work, so say it where someone will be looking.
fn warn_if_daemon_is_stale(daemon_build: u64) {
    let ours = haven_proto::build_stamp();
    if daemon_build != 0 && ours != 0 && daemon_build < ours {
        let age = ours - daemon_build;
        eprintln!(
            "haven: the running daemon was built {age}s before this binary; it is still \
             executing the older code.\n       `haven server stop` and reattach to pick up \
             the new build."
        );
    }
}

fn list(args: Args) -> ExitCode {
    match haven::attach::list(&socket_of(&args)) {
        Ok((sessions, daemon_build)) if sessions.is_empty() => {
            println!("no sessions");
            // A daemon with no sessions is still a daemon, and still the one
            // whose code is running.
            warn_if_daemon_is_stale(daemon_build);
            ExitCode::SUCCESS
        }
        Ok((sessions, daemon_build)) => {
            for s in sessions {
                println!(
                    "{}: {} ({} tabs, {} panes, {} clients, {}x{})",
                    s.id.0, s.name, s.windows, s.panes, s.clients, s.size.cols, s.size.rows
                );
            }
            warn_if_daemon_is_stale(daemon_build);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `eval "$(haven show-env)"` in a shell that predates the latest attach.
fn show_env(args: Args) -> ExitCode {
    match haven::attach::show_environment(&socket_of(&args)) {
        Ok(env) => {
            for (name, value) in env {
                // Single-quoted with the usual escape, so a value with a
                // space or a quote in it survives being evaluated.
                println!("export {name}='{}'", value.replace('\'', r"'\''"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

fn kill_session(args: Args) -> ExitCode {
    let Some(id) = args.target else {
        eprintln!("haven: kill-session needs -t <id>");
        return ExitCode::FAILURE;
    };
    match haven::attach::kill_session(&socket_of(&args), haven_proto::SessionId(id)) {
        Ok(remaining) => {
            println!("{} session(s) left", remaining.0.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

fn server(args: Args) -> ExitCode {
    let socket = socket_of(&args);
    match args.subcommand.as_deref() {
        Some("run") | None => {
            // Each pane gets its own freshly-configured shell, integration
            // and all — the daemon is where that decision belongs.
            let login = args.login;
            let integrate = args.integrate;
            let program = args.program.clone();
            // The program's own arguments, which every other subcommand
            // already honours. Without this `server run -- prog -x` ran
            // `prog` with nothing, silently.
            let rest = args.rest.clone();
            let make_spawner = move |env: haven_daemon::SessionEnv| {
                let program = program.clone();
                let rest = rest.clone();
                let spawner: Box<dyn FnMut(haven_pty::PtySize) -> std::io::Result<haven_pty::Pty>> =
                    Box::new(move |size| {
                        let (mut cfg, _, _) =
                            launch_config(program.as_deref(), login, integrate, &ctx(login));
                        cfg.args.extend(rest.iter().cloned());
                        // Read at spawn time, not at session creation: a pane
                        // opened after a reattach must get that attach's
                        // agent socket, not the one the session was born with.
                        cfg.env.extend(
                            env.borrow().iter().map(|(k, v)| (k.clone().into(), v.clone().into())),
                        );
                        cfg.size = size;
                        haven_pty::Pty::spawn(&cfg)
                    });
                spawner
            };
            match haven_daemon::Daemon::bind(&socket, make_spawner) {
                Ok(mut daemon) => match daemon.run() {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("haven: {e}");
                        ExitCode::FAILURE
                    }
                },
                Err(e) => {
                    eprintln!("haven: cannot bind {}: {e}", socket.display());
                    ExitCode::FAILURE
                }
            }
        }
        Some("stop") => match haven::attach::shutdown(&socket) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("haven: {e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("haven: unknown server command {other:?} (try run or stop)");
            ExitCode::FAILURE
        }
    }
}

/// Standalone TUI: a working terminal inside the terminal.
fn tui(args: Args) -> ExitCode {
    let (mut cfg, _, _) =
        launch_config(args.program.as_deref(), args.login, args.integrate, &ctx(args.login));
    cfg.args.extend(args.rest);

    match haven::solo::run(cfg, haven_tui::HostCaps::from_env()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dump(args: Args) -> ExitCode {
    let (mut cfg, shell, injection) =
        launch_config(args.program.as_deref(), args.login, args.integrate, &ctx(args.login));
    cfg.args.extend(args.rest);
    cfg.size = PtySize { cols: args.cols, rows: args.rows, ..Default::default() };

    let limits = PumpLimits { idle: Duration::from_millis(args.idle_ms), ..Default::default() };
    match run_to_screen(&cfg, limits, &args.send) {
        Ok((emu, raw, stats)) => {
            print!("{}", emu.dump());
            eprintln!(
                "--- {shell} | integration: {injection:?} | {} bytes in, {} replied, eof={}",
                stats.bytes_read,
                stats.reply_bytes,
                stats.eof,
                shell = shell.name(),
            );
            if let Some(title) = &emu.term.title {
                eprintln!("--- title: {title}");
            }
            if args.raw {
                eprintln!("--- raw: {}", String::from_utf8_lossy(&raw).escape_debug());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Render a program's screen on the GPU and write it to a file.
fn screenshot(args: Args) -> ExitCode {
    let (mut cfg, _, _) =
        launch_config(args.program.as_deref(), args.login, args.integrate, &ctx(args.login));
    cfg.args.extend(args.rest);
    cfg.size = PtySize { cols: args.cols, rows: args.rows, ..Default::default() };

    let out = args.out.clone().unwrap_or_else(|| PathBuf::from("haven.ppm"));
    match haven::shot::capture(
        &cfg,
        &args.send,
        Duration::from_millis(args.idle_ms),
        args.font_size,
        args.font.as_deref(),
        &out,
    ) {
        Ok((w, h)) => {
            println!("wrote {} ({w}x{h})", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("haven: {e}");
            ExitCode::FAILURE
        }
    }
}

fn shell_info(args: Args) -> ExitCode {
    let resolved = haven_shell::resolve(args.program.as_deref());
    let (cfg, shell, injection) =
        launch_config(Some(&resolved), args.login, true, &ctx(args.login));
    println!("shell:       {}", resolved.display());
    println!("impl:        {}", shell.name());
    println!("login:       {}", args.login);
    println!("integration: {injection:?}");
    println!("env:");
    for (k, v) in &cfg.env {
        println!("  {}={}", k.to_string_lossy(), v.to_string_lossy());
    }
    if !cfg.args.is_empty() {
        let shown: Vec<_> = cfg.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        println!("argv extra:  {}", shown.join(" "));
    }
    println!(
        "scrubbed:    {}",
        cfg.env_remove
            .iter()
            .map(|k| k.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = Path::new("");
    ExitCode::SUCCESS
}
