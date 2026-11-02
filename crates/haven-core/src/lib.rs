//! Terminal state machine: cells, styles, grid, VT interpretation.
//!
//! This crate is deliberately free of rendering, fonts, pty and protocol
//! concerns — it is a pure state machine you drive with byte slices, which is
//! what makes the golden-screen tests in `docs/PLAN.md` §17 cheap.

pub mod charset;
pub mod color;
pub mod grid;
pub mod style;
pub mod term;

pub use charset::{Charset, Charsets};
pub use color::{Color, Palette, Rgb, Role};
pub use grid::{Cell, CellFlags, Cursor, EraseMode, Grid, Row};
pub use style::{Attrs, Style, StyleId, StyleTable, Underline};
pub use term::{Emulator, Modes, MouseReport, Terminal};
