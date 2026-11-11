//! Write a Haven plugin.
//!
//! A plugin is a WebAssembly component. In Rust that means a `cdylib`
//! that depends on this crate, implements one trait and exports itself:
//!
//! ```ignore
//! haven_plugin_api::plugin!(Clock);
//!
//! struct Clock;
//!
//! impl haven_plugin_api::Plugin for Clock {
//!     fn render() -> Vec<Run> {
//!         vec![plain(&now())]
//!     }
//! }
//! ```
//!
//! and then
//!
//! ```console
//! $ cargo build --release --target wasm32-wasip2
//! ```
//!
//! which produces the component. No `cargo-component` and no
//! `wasm-tools`: since Rust 1.82 the `wasm32-wasip2` target emits one
//! directly.
//!
//! # What a plugin may do
//!
//! Only what its manifest asked for and the user granted. The functions
//! in [`host`] that reach outside the plugin return a `Result`, and a
//! capability that was not granted comes back as an error rather than as
//! a crash — a plugin that wanted `git` and did not get it should be
//! able to say so on the status bar.
//!
//! # The two rules
//!
//! [`Plugin::on_event`] returns commands; it does not change anything
//! itself. [`Plugin::render`] is pure and may be called whenever Haven
//! is drawing. Between them they mean a plugin can be replayed against a
//! recorded event stream with no terminal in sight, and that no plugin
//! can make the screen redraw in a loop.

wit_bindgen::generate!({
    world: "plugin",
    path: "wit",
    // The plugin crate is the one that exports, so the macro that does
    // it has to be public — and it has to know the name of this crate to
    // refer to the shared types from inside somebody else's.
    pub_export_macro: true,
    default_bindings_module: "haven_plugin_api",
});

pub use haven::plugin::host;
pub use haven::plugin::types::{
    Colour, Command, CommandEnd, CommandStart, CwdChange, Emitted, Event, Pane, Remembered, Rgb,
    Run, Style, Told,
};

/// What every plugin implements.
///
/// Only `render` and `on_event` have defaults worth having: a plugin
/// that only draws does not care about events, and a plugin that only
/// reacts draws nothing.
pub trait Plugin {
    /// Called once, before any event. Return an error and Haven unloads
    /// the plugin and says why, rather than leaving it half-alive.
    fn init() -> Result<(), String> {
        Ok(())
    }

    /// What happened, and what Haven should do about it.
    fn on_event(_event: Event) -> Vec<Command> {
        Vec::new()
    }

    /// What to draw on the status bar.
    fn render() -> Vec<Run> {
        Vec::new()
    }
}

/// Export a type as this component's plugin.
#[macro_export]
macro_rules! plugin {
    ($ty:ty) => {
        const _: () = {
            struct Shim;

            impl $crate::exports::haven::plugin::guest::Guest for Shim {
                fn init() -> Result<(), String> {
                    <$ty as $crate::Plugin>::init()
                }
                fn on_event(event: $crate::Event) -> Vec<$crate::Command> {
                    <$ty as $crate::Plugin>::on_event(event)
                }
                fn render() -> Vec<$crate::Run> {
                    <$ty as $crate::Plugin>::render()
                }
            }

    $crate::export!(Shim with_types_in $crate);
        };
    };
}

/// The theme's own colours, which is what a status segment should use
/// unless it has a reason not to. A plugin that hard-codes grey is
/// unreadable on half the themes people have.
pub const fn inherit() -> Style {
    Style { fg: Colour::Default, bg: Colour::Default, bold: false, dim: false, italic: false }
}

/// Text in the theme's colours.
pub fn plain(text: impl Into<String>) -> Run {
    Run { text: text.into(), style: inherit() }
}

/// Text that should catch the eye.
pub fn bold(text: impl Into<String>) -> Run {
    Run { text: text.into(), style: Style { bold: true, ..inherit() } }
}

/// Text that should not.
pub fn dim(text: impl Into<String>) -> Run {
    Run { text: text.into(), style: Style { dim: true, ..inherit() } }
}

/// Text in one of the 256 indexed colours. Prefer [`plain`]: an indexed
/// colour is readable on more themes than an RGB one, and the theme's own
/// is readable on all of them.
pub fn coloured(text: impl Into<String>, colour: u8) -> Run {
    Run { text: text.into(), style: Style { fg: Colour::Indexed(colour), ..inherit() } }
}

/// Run a program, and give back its standard output with the trailing
/// newline removed — which is what a plugin wants nine times in ten.
///
/// An error is the refusal, the program being missing, or a non-zero
/// exit, in each case in words a status bar can show.
pub fn output_of(argv: &[&str]) -> Result<String, String> {
    let owned: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let out = host::run(&owned)?;
    if out.status != 0 {
        let why = out.stderr.lines().next().unwrap_or("failed").trim().to_string();
        return Err(if why.is_empty() { format!("exited {}", out.status) } else { why });
    }
    Ok(out.stdout.trim_end().to_string())
}

/// Tell the other plugins that something happened.
///
/// Delivered to every other loaded plugin as [`Event::Told`] in the same
/// tick, so one plugin noticing a thing and another reacting to it
/// happens in one frame rather than two.
///
/// It is a **broadcast**. There is no way to address one plugin, so
/// anything emitted is readable by every plugin installed — do not put
/// in it what you would not grant them. `secret-guard` emits that it
/// saw a credential and never what it was, which is the shape to copy.
pub fn emit(name: impl Into<String>, data: impl Into<String>) -> Command {
    Command::Emit(Emitted { name: name.into(), data: data.into() })
}

/// The pane the user is looking at, if a plugin needs to ask.
pub fn focused() -> Option<Pane> {
    host::panes().into_iter().find(|pane| pane.focused)
}
