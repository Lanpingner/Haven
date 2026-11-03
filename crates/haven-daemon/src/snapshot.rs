//! Saving sessions to a file, and building them back.
//!
//! What survives a reboot is the *shape* of your work — which sessions, which
//! tabs, how the panes were arranged, and what directory each was in — not
//! the processes, which are gone. A restored pane is a fresh shell in the
//! right place, and a program is re-run only if the config says that program
//! is safe to re-run. Replaying whatever happened to be in the foreground
//! would mean a file on disk deciding what a machine executes at login.
//!
//! The format is its own set of types rather than the mux's. A save file is
//! read by a future version of Haven, so it must not change shape every time
//! the pane tree does.

use std::path::{Path, PathBuf};

use haven_mux::{Child, Direction, PaneNode, Session};
use haven_proto::PaneId;
use serde::{Deserialize, Serialize};

/// Bumped when the format changes in a way an older reader cannot handle.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub saved_unix: u64,
    #[serde(default, rename = "session")]
    pub sessions: Vec<SavedSession>,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SavedSession {
    pub name: String,
    /// Index into `windows`.
    #[serde(default)]
    pub active_window: usize,
    #[serde(default, rename = "window")]
    pub windows: Vec<SavedWindow>,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Only a name someone typed. A name that came from the running command
    /// describes a process that will not be there, so it is left to be
    /// worked out again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub active_pane: usize,
    /// The tree, with leaves numbered in the order `panes` lists them.
    pub layout: SavedNode,
    #[serde(default, rename = "pane")]
    pub panes: Vec<SavedPane>,
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct SavedPane {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// What was running, as a command line. Saved whether or not it will be
    /// re-run: a save file people can read is worth more than a tidy one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The virtual shell this pane was opened as, if it was one — e.g.
    /// `docker://run/archlinux:latest?keep`. Written only for a spec the
    /// backends recognise, never for an arbitrary program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
}

/// The pane tree as the file holds it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedNode {
    /// A pane, by its index in the window's `panes` list.
    Pane {
        index: usize,
    },
    Split {
        columns: bool,
        children: Vec<SavedChild>,
    },
}

impl Default for SavedNode {
    fn default() -> Self {
        Self::Pane { index: 0 }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SavedChild {
    pub weight: f32,
    pub node: SavedNode,
}

impl SavedNode {
    /// How many panes the tree holds.
    pub fn len(&self) -> usize {
        match self {
            Self::Pane { .. } => 1,
            Self::Split { children, .. } => children.iter().map(|c| c.node.len()).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Back to a mux tree, with `PaneId(index)` placeholders at the leaves.
    pub fn to_tree(&self) -> PaneNode {
        match self {
            Self::Pane { index } => PaneNode::Leaf(PaneId(*index as u32)),
            Self::Split { columns, children } => PaneNode::Split {
                dir: if *columns { Direction::Columns } else { Direction::Rows },
                children: children
                    .iter()
                    .map(|c| Child { node: c.node.to_tree(), weight: c.weight })
                    .collect(),
            },
        }
    }

    /// From a mux tree. `order` maps a pane id to its position in the saved
    /// list, so the file never refers to an id that will not exist again.
    fn from_tree(node: &PaneNode, order: &[PaneId]) -> Self {
        match node {
            PaneNode::Leaf(id) => {
                Self::Pane { index: order.iter().position(|p| p == id).unwrap_or(0) }
            }
            PaneNode::Split { dir, children } => Self::Split {
                columns: *dir == Direction::Columns,
                children: children
                    .iter()
                    .map(|c| SavedChild { weight: c.weight, node: Self::from_tree(&c.node, order) })
                    .collect(),
            },
        }
    }
}

/// Describe one session, asking `pane` for what each pane knows about itself.
pub fn describe<F>(session: &Session, mut pane: F) -> SavedSession
where
    F: FnMut(PaneId) -> SavedPane,
{
    SavedSession {
        name: session.name.clone(),
        active_window: session.active,
        windows: session
            .windows
            .iter()
            .map(|window| {
                let order = window.root.panes();
                SavedWindow {
                    // `title` is only ever set by a rename; the names that
                    // come from a running command are worked out afresh.
                    name: window.title.clone(),
                    active_pane: order.iter().position(|p| *p == window.active).unwrap_or(0),
                    layout: SavedNode::from_tree(&window.root, &order),
                    panes: order.iter().map(|p| pane(*p)).collect(),
                }
            })
            .collect(),
    }
}

impl Snapshot {
    pub fn new(sessions: Vec<SavedSession>) -> Self {
        Self { version: FORMAT_VERSION, saved_unix: crate::unix_now(), sessions }
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        let snapshot: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if snapshot.version > FORMAT_VERSION {
            return Err(format!(
                "the save file is version {}, this build understands {FORMAT_VERSION}",
                snapshot.version
            ));
        }
        Ok(snapshot)
    }

    /// Write atomically: a half-written save file is worse than none, and
    /// the autosave runs often enough to be caught mid-write by a crash.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let temp = path.with_extension("toml.new");
        std::fs::write(&temp, self.to_toml()?).map_err(|e| format!("{}: {e}", temp.display()))?;
        std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text)
    }
}

/// `$XDG_DATA_HOME/haven/sessions.toml`, falling back to `~/.local/share`.
pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("haven")
        .join("sessions.toml")
}

/// Whether a saved command should be started again.
///
/// An allowlist, and a short one. A save file is a file on disk that decides
/// what a machine runs when you log in; re-running whatever happened to be in
/// the foreground is how a restore turns into an exploit. The default list is
/// the one tmux-resurrect ships, for the same reason.
pub fn restorable(command: &[String], allowed: &[String]) -> bool {
    let Some(first) = command.first() else { return false };
    let name = Path::new(first).file_name().and_then(|n| n.to_str()).unwrap_or(first);
    allowed.iter().any(|a| a == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PaneNode {
        PaneNode::Split {
            dir: Direction::Columns,
            children: vec![
                Child { node: PaneNode::Leaf(PaneId(7)), weight: 1.0 },
                Child {
                    node: PaneNode::Split {
                        dir: Direction::Rows,
                        children: vec![
                            Child { node: PaneNode::Leaf(PaneId(9)), weight: 2.0 },
                            Child { node: PaneNode::Leaf(PaneId(4)), weight: 1.0 },
                        ],
                    },
                    weight: 3.0,
                },
            ],
        }
    }

    #[test]
    fn a_tree_survives_the_round_trip_with_its_weights() {
        let order = tree().panes();
        let saved = SavedNode::from_tree(&tree(), &order);
        let back = saved.to_tree();
        // Ids are positions now, not the originals: the point is that the
        // *shape* and the proportions come back.
        assert_eq!(back.panes(), vec![PaneId(0), PaneId(1), PaneId(2)]);
        match (&back, &tree()) {
            (PaneNode::Split { dir: a, children: x }, PaneNode::Split { dir: b, children: y }) => {
                assert_eq!(a, b);
                assert_eq!(x[0].weight, y[0].weight);
                assert_eq!(x[1].weight, y[1].weight);
            }
            _ => panic!("not a split"),
        }
        assert_eq!(saved.len(), 3);
    }

    #[test]
    fn the_file_is_toml_a_person_can_read_and_edit() {
        let snapshot = Snapshot::new(vec![SavedSession {
            name: "work".into(),
            active_window: 0,
            windows: vec![SavedWindow {
                name: Some("build".into()),
                active_pane: 1,
                layout: SavedNode::from_tree(&tree(), &tree().panes()),
                panes: vec![
                    SavedPane { cwd: Some("/tmp".into()), ..Default::default() },
                    SavedPane {
                        cwd: Some("/var".into()),
                        command: vec!["vim".into(), "a.txt".into()],
                        ..Default::default()
                    },
                    SavedPane {
                        cwd: Some("/src".into()),
                        program: Some("docker://run/alpine:latest?keep".into()),
                        ..Default::default()
                    },
                    SavedPane::default(),
                ],
            }],
        }]);
        let text = snapshot.to_toml().expect("serialize");
        assert!(text.contains("name = \"work\""), "{text}");
        assert_eq!(Snapshot::from_toml(&text).expect("parse"), snapshot);
    }

    #[test]
    fn a_newer_file_is_refused_rather_than_misread() {
        let text = "version = 99\nsaved_unix = 0\n";
        let err = Snapshot::from_toml(text).expect_err("should refuse");
        assert!(err.contains("version 99"), "{err}");
    }

    #[test]
    fn an_empty_file_is_an_empty_snapshot_not_an_error() {
        // Restoring before anything was ever saved is ordinary.
        let snapshot = Snapshot::from_toml("version = 1\nsaved_unix = 0\n").expect("parse");
        assert!(snapshot.sessions.is_empty());
    }

    #[test]
    fn only_allowed_commands_come_back() {
        let allowed: Vec<String> = ["vim", "less"].iter().map(|s| s.to_string()).collect();
        assert!(restorable(&["vim".into(), "a.txt".into()], &allowed));
        // Matched on the program's name, so a full path still counts.
        assert!(restorable(&["/usr/bin/vim".into()], &allowed));
        assert!(!restorable(&["rm".into(), "-rf".into(), "/".into()], &allowed));
        assert!(!restorable(&[], &allowed));
    }

    #[test]
    fn a_half_written_file_never_replaces_a_good_one() {
        let dir = std::env::temp_dir().join(format!("haven-snap-{}", std::process::id()));
        let path = dir.join("sessions.toml");
        Snapshot::new(vec![SavedSession { name: "first".into(), ..Default::default() }])
            .write(&path)
            .expect("write");
        assert!(Snapshot::read(&path).expect("read").sessions[0].name == "first");
        // The temporary is gone, so nothing is left to be read by mistake.
        assert!(!path.with_extension("toml.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
