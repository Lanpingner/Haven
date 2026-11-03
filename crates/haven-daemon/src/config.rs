//! The daemon's configuration file.
//!
//! Configuration lives on the server, not in each client's arguments, so one
//! session looks the same through a window and through a terminal and a
//! change reaches both. The file is re-read when its timestamp moves, which
//! is what makes editing a theme show up without reattaching.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use std::collections::BTreeMap;

use haven_core::Color;
use haven_proto::theme::{parse_color, Glyphs, Pair, Theme};
use haven_proto::KeyTable;
use serde::Deserialize;

/// A theme as written in a file.
///
/// Separate from the wire [`Theme`] on purpose. A file wants `"#1b1e28"` and
/// wants to leave out everything it does not care about; the wire wants a
/// fixed, fully-populated struct that postcard can encode — it is not
/// self-describing and cannot read an untagged colour. Letting the file
/// shape the wire type cost an afternoon's worth of decode errors.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeFile {
    pub name: Option<String>,
    /// Start from this built-in and change only what follows.
    pub base: Option<String>,
    pub bar_session: Option<PairFile>,
    pub bar: Option<PairFile>,
    pub tab: Option<PairFile>,
    pub tab_active: Option<PairFile>,
    pub bar_dim: Option<PairFile>,
    pub bar_alert: Option<PairFile>,
    pub command: Option<PairFile>,
    pub panel: Option<PairFile>,
    pub panel_border: Option<PairFile>,
    pub panel_title: Option<PairFile>,
    pub panel_dim: Option<PairFile>,
    pub pane_border: Option<PairFile>,
    pub selection: Option<PairFile>,
    pub glyphs: Option<Glyphs>,
}

/// A colour as a file may write it: `"#1b1e28"`, `"default"`, `12` or
/// `"12"`. Untagged works here because TOML is self-describing — the wire
/// form cannot do this, which is exactly why the two types are separate.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(untagged)]
pub enum ColorText {
    Index(u16),
    Text(String),
}

impl ColorText {
    fn parse(&self) -> Result<Color, String> {
        match self {
            Self::Index(n) if *n < 256 => Ok(Color::Indexed(*n as u8)),
            Self::Index(n) => Err(format!("palette index {n} is above 255")),
            Self::Text(s) => parse_color(s).ok_or_else(|| format!("bad colour {s:?}")),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PairFile {
    pub fg: Option<ColorText>,
    pub bg: Option<ColorText>,
    pub bold: Option<bool>,
}

impl PairFile {
    fn apply(&self, base: Pair) -> Result<Pair, String> {
        let colour = |given: &Option<ColorText>, fallback: Color| -> Result<Color, String> {
            match given {
                Some(text) => text.parse(),
                None => Ok(fallback),
            }
        };
        Ok(Pair {
            fg: colour(&self.fg, base.fg)?,
            bg: colour(&self.bg, base.bg)?,
            bold: self.bold.unwrap_or(base.bold),
        })
    }
}

impl ThemeFile {
    /// Fold the file onto a base theme. Anything the file omits is kept, so
    /// changing one accent colour is three lines rather than forty.
    pub fn resolve(&self) -> Result<Theme, String> {
        let mut theme = match &self.base {
            Some(name) => {
                Theme::builtin(name).ok_or_else(|| format!("unknown base theme {name:?}"))?
            }
            None => Theme::default(),
        };
        if let Some(name) = &self.name {
            theme.name = name.clone();
        }
        macro_rules! role {
            ($field:ident) => {
                if let Some(given) = &self.$field {
                    theme.$field = given
                        .apply(theme.$field)
                        .map_err(|e| format!("{}: {e}", stringify!($field)))?;
                }
            };
        }
        role!(bar_session);
        role!(bar);
        role!(tab);
        role!(tab_active);
        role!(bar_dim);
        role!(bar_alert);
        role!(command);
        role!(panel);
        role!(panel_border);
        role!(panel_title);
        role!(panel_dim);
        role!(pane_border);
        role!(selection);
        if let Some(glyphs) = &self.glyphs {
            theme.glyphs = glyphs.clone();
        }
        Ok(theme)
    }
}

/// `theme = "nord"`, or an inline table that overrides whatever it names.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(untagged)]
pub enum ThemeSpec {
    Named(String),
    Inline(Box<ThemeFile>),
}

impl Default for ThemeSpec {
    fn default() -> Self {
        Self::Named("haven".into())
    }
}

/// `[keys]` in the config.
///
/// Entries are `"<spec>" = "<command>"`, with the same command names the
/// command line takes — a binding that could do something a typed command
/// could not would be a second vocabulary to learn.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysFile {
    /// What arms the prefix. `C-a` unless this says otherwise.
    pub prefix: Option<String>,
    /// Bound after the prefix. An empty command unbinds.
    #[serde(default)]
    pub bind: BTreeMap<String, String>,
    /// Bound with no prefix at all, which is how someone who dislikes the
    /// prefix gets `C-t` for a new tab.
    #[serde(default)]
    pub root: BTreeMap<String, String>,
    /// Tables of your own, each armed by `switch-table <name>` and staying
    /// armed until something switches away. `[keys.table.resize]` with
    /// `h`/`j`/`k`/`l` in it is the obvious use.
    #[serde(default)]
    pub table: BTreeMap<String, BTreeMap<String, String>>,
}

impl KeysFile {
    pub fn table(&self) -> KeyTable {
        KeyTable {
            mouse: true,
            // Empty means "whatever the client's default is". The daemon has
            // no business holding a copy of that: it would be a second place
            // for it to be wrong.
            prefix: self.prefix.clone().unwrap_or_default(),
            tables: std::iter::once((
                "prefix".to_string(),
                self.bind.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ))
            .chain(std::iter::once((
                "root".to_string(),
                self.root.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            )))
            .chain(self.table.iter().map(|(name, entries)| {
                (name.clone(), entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            }))
            .collect(),
        }
    }
}

/// The `[containers]` section: where a pane's shell may run other than here.
///
/// Three keys and no more. `cli` is the one that matters most and looks the
/// least important: `podman` takes every argument Haven passes unchanged, so
/// this line is the whole of podman support.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainersFile {
    pub enabled: bool,
    pub cli: String,
    /// Distros offered even when they are not pulled yet. Replaces the
    /// built-in list rather than adding to it, so a list you wrote is the
    /// list you get.
    pub images: Vec<String>,
}

impl Default for ContainersFile {
    fn default() -> Self {
        let defaults = haven_backend::Settings::default();
        Self { enabled: defaults.enabled, cli: defaults.cli, images: defaults.images }
    }
}

impl From<&ContainersFile> for haven_backend::Settings {
    fn from(file: &ContainersFile) -> Self {
        Self { cli: file.cli.clone(), images: file.images.clone(), enabled: file.enabled }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeSpec,
    pub keys: KeysFile,
    /// Programs a restore is allowed to start again.
    ///
    /// An allowlist, and short on purpose: a save file decides what runs when
    /// a session comes back, and re-running whatever happened to be in the
    /// foreground is how a restore turns into an exploit.
    pub restore_programs: Vec<String>,
    /// Save every session this often, in seconds. Zero is off. Without it a
    /// save file is only as good as the last time someone remembered.
    pub autosave_seconds: u64,
    /// Where a pane's shell may run other than on this machine.
    pub containers: ContainersFile,
    /// Whether clients handle the mouse at all.
    ///
    /// Off hands it back to whatever terminal `haven attach` is running in,
    /// so its own selection and scrollback work as they did. In a window
    /// there is no outer terminal, and off simply means no mouse.
    pub mouse: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeSpec::default(),
            keys: KeysFile::default(),
            containers: ContainersFile::default(),
            mouse: true,
            // tmux-resurrect's default list, for the same reasons: editors
            // and pagers, which re-open a file and do nothing else.
            restore_programs: [
                "vim", "nvim", "vi", "emacs", "nano", "helix", "hx", "less", "man", "tail", "top",
                "htop", "btop", "watch",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            autosave_seconds: 900,
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// The theme this configuration asks for, and the reason if it could not
    /// be had. A broken config keeps the interface usable — refusing to paint
    /// anything because a colour is misspelt would be a poor trade — but it
    /// must say so rather than look like the theme being wrong.
    pub fn theme(&self, dir: &Path) -> (Theme, Option<String>) {
        match &self.theme {
            ThemeSpec::Inline(file) => match file.resolve() {
                Ok(theme) => (theme, None),
                Err(e) => (Theme::default(), Some(e)),
            },
            ThemeSpec::Named(name) => match Theme::builtin(name) {
                Some(theme) => (theme, None),
                None => match load_named(name, dir) {
                    Ok(theme) => (theme, None),
                    Err(e) => (Theme::default(), Some(e)),
                },
            },
        }
    }
}

/// A name in the themes directory, or a path.
fn load_named(name: &str, dir: &Path) -> Result<Theme, String> {
    let path = if name.contains('/') || name.ends_with(".toml") {
        PathBuf::from(name)
    } else {
        dir.join(format!("{name}.toml"))
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ThemeFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut theme = file.resolve().map_err(|e| format!("{}: {e}", path.display()))?;
    if theme.name == Theme::default().name {
        theme.name = name.to_string();
    }
    Ok(theme)
}

/// Watches the config file and the theme it points at, so a change to either
/// reaches attached clients.
#[derive(Debug)]
pub struct ConfigWatcher {
    path: PathBuf,
    dir: PathBuf,
    /// The theme file, when the config names one: editing the colours has to
    /// be noticed as much as editing which theme is chosen.
    theme_path: Option<PathBuf>,
    stamps: (Option<SystemTime>, Option<SystemTime>),
    config: Config,
    theme: Theme,
    keys: KeyTable,
    pub problem: Option<String>,
}

fn stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl ConfigWatcher {
    pub fn new(path: PathBuf, dir: PathBuf) -> Self {
        let mut w = Self {
            path,
            dir,
            theme_path: None,
            stamps: (None, None),
            config: Config::default(),
            theme: Theme::default(),
            keys: KeyTable::default(),
            problem: None,
        };
        w.reload();
        w
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn keys(&self) -> &KeyTable {
        &self.keys
    }

    pub fn mouse(&self) -> bool {
        self.config.mouse
    }

    pub fn restore_programs(&self) -> &[String] {
        &self.config.restore_programs
    }

    pub fn autosave_seconds(&self) -> u64 {
        self.config.autosave_seconds
    }

    pub fn containers(&self) -> haven_backend::Settings {
        (&self.config.containers).into()
    }

    /// Re-read if anything moved. Returns whether the theme changed, so the
    /// daemon only pushes when there is something to push.
    pub fn poll(&mut self) -> bool {
        let now = (stamp(&self.path), self.theme_path.as_deref().and_then(stamp));
        if now == self.stamps {
            return false;
        }
        let before = (self.theme.clone(), self.keys.clone());
        self.reload();
        (self.theme.clone(), self.keys.clone()) != before
    }

    fn reload(&mut self) {
        self.problem = None;
        self.config = match std::fs::read_to_string(&self.path) {
            Ok(text) => match Config::from_toml(&text) {
                Ok(config) => config,
                Err(e) => {
                    self.problem = Some(format!("{}: {e}", self.path.display()));
                    Config::default()
                }
            },
            // No config file is the normal case, not a problem to report.
            Err(_) => Config::default(),
        };

        self.theme_path = match &self.config.theme {
            ThemeSpec::Named(name) if Theme::builtin(name).is_none() => {
                Some(if name.contains('/') || name.ends_with(".toml") {
                    PathBuf::from(name)
                } else {
                    self.dir.join(format!("{name}.toml"))
                })
            }
            _ => None,
        };

        let (theme, problem) = self.config.theme(&self.dir);
        self.theme = theme;
        self.keys = KeyTable { mouse: self.config.mouse, ..self.config.keys.table() };
        if self.problem.is_none() {
            self.problem = problem;
        }
        self.stamps = (stamp(&self.path), self.theme_path.as_deref().and_then(stamp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("haven-cfg-{}-{tag}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn a_named_builtin_is_resolved() {
        let config = Config::from_toml(r#"theme = "nord""#).expect("parse");
        let (theme, problem) = config.theme(Path::new("/nonexistent"));
        assert_eq!(theme.name, "nord");
        assert!(problem.is_none());
    }

    #[test]
    fn an_inline_theme_overrides_only_what_it_names() {
        let config = Config::from_toml(
            r##"
            [theme]
            name = "mine"
            bar_session = { fg = "#000000", bg = "#ff8800", bold = true }
            "##,
        )
        .expect("parse");
        let (theme, _) = config.theme(Path::new("/nonexistent"));
        assert_eq!(theme.name, "mine");
        assert_eq!(theme.selection, Theme::haven().selection, "an omitted role changed");
    }

    #[test]
    fn a_missing_theme_is_reported_but_still_paints() {
        let config = Config::from_toml(r#"theme = "no-such-theme""#).expect("parse");
        let (theme, problem) = config.theme(Path::new("/nonexistent"));
        assert_eq!(theme, Theme::default(), "a broken theme should still paint");
        assert!(problem.is_some(), "and should say why");
    }

    #[test]
    fn keys_come_out_of_the_config_as_a_table() {
        let config = Config::from_toml(
            r#"
            [keys]
            prefix = "C-b"
            bind = { "|" = "split-v", "x" = "" }
            root = { "C-t" = "new-window" }
            "#,
        )
        .expect("parse");
        let table = config.keys.table();
        let named = |name: &str| -> Vec<(String, String)> {
            table.tables.iter().find(|(n, _)| n == name).map(|(_, e)| e.clone()).unwrap_or_default()
        };
        assert_eq!(table.prefix, "C-b");
        assert!(named("prefix").contains(&("|".into(), "split-v".into())));
        assert!(named("prefix").contains(&("x".into(), String::new())), "unbind was dropped");
        assert_eq!(named("root"), vec![("C-t".into(), "new-window".into())]);
    }

    #[test]
    fn a_table_of_your_own_rides_alongside_the_two_built_in_ones() {
        let config = Config::from_toml(
            r#"
            [keys.table.resize]
            h = "resize-left"
            Escape = "switch-table root"
            "#,
        )
        .expect("parse");
        let table = config.keys.table();
        let names: Vec<&str> = table.tables.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"prefix") && names.contains(&"root"), "{names:?}");
        let resize = table
            .tables
            .iter()
            .find(|(n, _)| n == "resize")
            .map(|(_, e)| e.clone())
            .expect("the table is missing");
        assert!(resize.contains(&("h".into(), "resize-left".into())));
    }

    #[test]
    fn no_keys_section_leaves_the_prefix_to_the_client() {
        // An empty prefix means "whatever the default is". The daemon holding
        // its own copy of that would be a second place for it to be wrong.
        let config = Config::from_toml("").expect("parse");
        assert_eq!(config.keys.table().prefix, "");
    }

    #[test]
    fn the_mouse_can_be_switched_off() {
        let config = Config::from_toml("mouse = false").expect("parse");
        assert!(!config.mouse);
        assert!(Config::from_toml("").expect("parse").mouse, "the mouse is on by default");
    }

    #[test]
    fn switching_the_mouse_off_is_pushed_to_clients() {
        let dir = tmp("mouse");
        let path = dir.join("config.toml");
        std::fs::write(&path, "mouse = true\n").unwrap();
        let mut w = ConfigWatcher::new(path.clone(), dir.clone());
        assert!(w.keys().mouse);

        std::fs::write(&path, "mouse = false\n").unwrap();
        filetime_bump(&path);
        assert!(w.poll(), "a client still grabbing the mouse would be the bug");
        assert!(!w.keys().mouse);
    }

    #[test]
    fn editing_a_binding_is_noticed() {
        let dir = tmp("keys");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[keys]\nbind = { v = \"split-v\" }\n").unwrap();
        let mut w = ConfigWatcher::new(path.clone(), dir.clone());
        let prefix = |w: &ConfigWatcher| -> Vec<(String, String)> {
            w.keys()
                .tables
                .iter()
                .find(|(n, _)| n == "prefix")
                .map(|(_, e)| e.clone())
                .unwrap_or_default()
        };
        assert_eq!(prefix(&w), vec![("v".to_string(), "split-v".to_string())]);

        std::fs::write(&path, "[keys]\nbind = { v = \"split-h\" }\n").unwrap();
        filetime_bump(&path);
        assert!(w.poll(), "a changed binding must reach attached clients");
        assert_eq!(prefix(&w), vec![("v".to_string(), "split-h".to_string())]);
    }

    #[test]
    fn editing_the_config_is_noticed() {
        let dir = tmp("edit");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"nord\"\n").unwrap();
        let mut w = ConfigWatcher::new(path.clone(), dir.clone());
        assert_eq!(w.theme().name, "nord");
        assert!(!w.poll(), "nothing changed, nothing to push");

        // Timestamps have one-second resolution on some filesystems, so the
        // stamp is moved explicitly rather than by writing quickly.
        std::fs::write(&path, "theme = \"gruvbox\"\n").unwrap();
        filetime_bump(&path);
        assert!(w.poll(), "the edit was not noticed");
        assert_eq!(w.theme().name, "gruvbox");
        assert!(!w.poll(), "a second poll should report no further change");
    }

    #[test]
    fn editing_the_theme_file_itself_is_noticed() {
        let dir = tmp("themefile");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"mine\"\n").unwrap();
        let theme_file = dir.join("mine.toml");
        std::fs::write(&theme_file, "name = \"mine\"\n").unwrap();

        let mut w = ConfigWatcher::new(path, dir.clone());
        assert_eq!(w.theme().name, "mine");

        // Changing which theme is chosen is the obvious case; changing the
        // colours inside the chosen theme is the one that is easy to miss.
        std::fs::write(&theme_file, "name = \"mine\"\nbar = { fg = 1, bg = 2 }\n").unwrap();
        filetime_bump(&theme_file);
        assert!(w.poll(), "editing the theme file was not noticed");
        assert_eq!(w.problem, None, "the edited theme did not load");
        assert_eq!(w.theme().bar.fg, Color::Indexed(1));
    }

    /// Push a file's mtime forward, since two writes in the same second can
    /// otherwise look identical.
    fn filetime_bump(path: &Path) {
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(later).unwrap();
    }
}
