//! The plugin browser: repositories, what is in them, and what moved.
//!
//! A tree like [the session tree](crate::chooser), and for the same
//! reason — the question is "what is there?", and everything at once
//! answers it better than something you have to expand. Repositories at
//! the top level, their plugins beneath, and beside the list a panel
//! about whatever is selected: a repository's key and its recent
//! changes, or a plugin's version, what it asks for and what it was
//! granted.
//!
//! It only shows. Installing from here would mean putting a capability
//! prompt inside a modal that is drawn over a pane, which is the one
//! place a question about permissions should not be — it has to be read,
//! and a modal is somewhere people press keys to make go away. So the
//! browser tells you the command and gets out of the way.
//!
//! Everything here arrives already verified from the daemon, which is
//! where the keys and the signatures are. The client is handed
//! conclusions: a repository whose index did not verify comes with its
//! problem in words, and there is nothing here that decides what to
//! trust.

use haven_proto::{CellSize, RepoInfo};

use crate::dialog::{DialogRow, Run};
use crate::frame::Widget;
use crate::theme::Theme;

/// What a row is about.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    Repo(usize),
    /// A plugin, by its repository and its place in that repository.
    Plugin(usize, usize),
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Browser {
    pub repos: Vec<RepoInfo>,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Typed text narrows the list, as it does in every other modal.
    pub filter: String,
    /// Open, but the listing has not arrived yet.
    ///
    /// The box opens on the keypress rather than a round trip later,
    /// because a modal that appears when the answer does spends that
    /// round trip feeding your keystrokes to the shell behind it. Caught
    /// by a test that pressed `u` and then `j` and found a `j` at the
    /// prompt.
    pub loading: bool,
}

impl Browser {
    /// Open, with nothing in it yet.
    pub fn loading() -> Self {
        Self { loading: true, ..Default::default() }
    }

    /// Built from what the daemon sent, keeping whatever was already
    /// typed into the box while it was waiting.
    ///
    /// Typing is what people do in that gap — the modal is there to be
    /// searched — and throwing the search away when the answer arrives
    /// loses it just as surely as not opening the box at all.
    pub fn filled(self, repos: Vec<RepoInfo>) -> Self {
        let filter = self.filter;
        let mut next = Self::new(repos);
        if !filter.is_empty() {
            next.filter = filter;
            // The selection `new` chose may not be something the filter
            // admits.
            next.resettle();
        }
        next
    }

    /// Built from what the daemon sent. Nothing is asked of it again:
    /// this is a listing, not a live view, and it is cheap to reopen.
    pub fn new(repos: Vec<RepoInfo>) -> Self {
        let mut rows = Vec::new();
        for (r, repo) in repos.iter().enumerate() {
            rows.push(Row::Repo(r));
            for (p, _) in repo.plugins.iter().enumerate() {
                rows.push(Row::Plugin(r, p));
            }
        }
        // Opens on the first thing worth looking at: a plugin with an
        // update waiting, if there is one. That is the question people
        // open this to answer.
        let selected = rows
            .iter()
            .position(|row| match row {
                Row::Plugin(r, p) => {
                    let plugin = &repos[*r].plugins[*p];
                    plugin.installed.as_ref().is_some_and(|v| *v != plugin.version)
                }
                Row::Repo(_) => false,
            })
            .unwrap_or(0);
        Self { repos, rows, selected, filter: String::new(), loading: false }
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    /// How many installed plugins a repository offers a new version of.
    pub fn updates(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| match row {
                Row::Plugin(r, p) => {
                    let plugin = &self.repos[*r].plugins[*p];
                    plugin.installed.as_ref().is_some_and(|v| *v != plugin.version)
                }
                Row::Repo(_) => false,
            })
            .count()
    }

    /// The text a row is matched and drawn with.
    fn text(&self, row: &Row) -> String {
        match row {
            Row::Repo(r) => {
                let repo = &self.repos[*r];
                let count = repo.plugins.len();
                format!(
                    "{} ({count} plugin{}{})",
                    repo.name,
                    if count == 1 { "" } else { "s" },
                    if repo.checked { "" } else { ", key never checked" }
                )
            }
            Row::Plugin(r, p) => {
                let plugin = &self.repos[*r].plugins[*p];
                // A row is narrow and gets clipped, so an update says
                // `0.2.1 → 0.3.0` rather than "0.2.1 installed, 0.3.0
                // available" — which did not fit and was cut to "0.2.1
                // installed, 0.3.0", reading as something else entirely.
                // It is also the form the change log uses, so the two
                // agree.
                match &plugin.installed {
                    Some(have) if *have == plugin.version => {
                        format!("{} {}  installed", plugin.name, plugin.version)
                    }
                    Some(have) => format!("{}  {have} → {}", plugin.name, plugin.version),
                    None => format!("{} {}", plugin.name, plugin.version),
                }
            }
        }
    }

    pub fn visible(&self) -> Vec<(usize, &Row)> {
        if self.filter.is_empty() {
            return self.rows.iter().enumerate().collect();
        }
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                // A repository matches on its own name; a plugin on its
                // name, its summary, or the name of the repository it is
                // in — so typing a repository's name narrows to its
                // plugins rather than to one row nothing hangs beneath.
                let own = self.text(row).to_lowercase().contains(&needle);
                let extra = match row {
                    Row::Repo(_) => false,
                    Row::Plugin(r, p) => {
                        let plugin = &self.repos[*r].plugins[*p];
                        plugin.summary.to_lowercase().contains(&needle)
                            || self.repos[*r].name.to_lowercase().contains(&needle)
                    }
                };
                own || extra
            })
            .collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        let next = (at as isize + delta).rem_euclid(visible.len() as isize) as usize;
        self.selected = visible[next].0;
    }

    pub fn resettle(&mut self) {
        let visible = self.visible();
        if visible.iter().any(|(i, _)| *i == self.selected) {
            return;
        }
        self.selected = visible.first().map(|(i, _)| *i).unwrap_or(0);
    }

    /// The command that acts on what is selected, for the hint line.
    ///
    /// Shown rather than run: see the module note. A name is enough to
    /// retype, and it is the name you would have had to look up anyway.
    pub fn command(&self) -> Option<String> {
        match self.rows.get(self.selected)? {
            Row::Repo(r) => Some(format!("haven plugin repo update {}", self.repos[*r].name)),
            Row::Plugin(r, p) => {
                let plugin = &self.repos[*r].plugins[*p];
                Some(match &plugin.installed {
                    Some(have) if *have == plugin.version => {
                        format!("haven plugin remove {}", plugin.name)
                    }
                    Some(_) => format!("haven plugin install {}", plugin.name),
                    None => format!("haven plugin install {}", plugin.name),
                })
            }
        }
    }

    /// The panel beside the list: everything about the selected row that
    /// does not fit on it.
    fn panel(&self, width: usize, theme: &Theme) -> Vec<Vec<Run>> {
        if width == 0 {
            return Vec::new();
        }
        let plain = theme.panel.style();
        let dim = theme.panel_dim.style();
        let title = theme.panel_title.style();
        let mut out: Vec<Vec<Run>> = Vec::new();
        let mut line = |text: String, style| out.push(vec![(text, style)]);

        if self.loading {
            line(" looking…".into(), dim);
            return out;
        }
        let Some(row) = self.rows.get(self.selected) else {
            line(" nothing to show".into(), dim);
            return out;
        };
        match row {
            Row::Repo(r) => {
                let repo = &self.repos[*r];
                line(format!(" {}", repo.name), title);
                for part in wrap(&repo.url, width.saturating_sub(2)) {
                    line(format!(" {part}"), plain);
                }
                line(String::new(), plain);
                line(format!(" key {}", repo.key), plain);
                if !repo.checked {
                    // Said here as well as on the row. Whoever is reading
                    // the panel is reading about this repository, and it
                    // is the thing most worth knowing about it.
                    for part in wrap(
                        "never checked against a fingerprint from anywhere else",
                        width.saturating_sub(2),
                    ) {
                        line(format!(" {part}"), dim);
                    }
                }
                if let Some(problem) = &repo.problem {
                    line(String::new(), plain);
                    line(" problem".into(), title);
                    for part in wrap(problem, width.saturating_sub(2)) {
                        line(format!(" {part}"), plain);
                    }
                }
                if !repo.changes.is_empty() {
                    line(String::new(), plain);
                    line(" recent changes".into(), title);
                    // Only as many as the panel can hold: this is the
                    // last section, so it is the one that gets cut, and
                    // the newest are the ones worth keeping.
                    for change in repo.changes.iter().take(12) {
                        line(format!(" {}  {}", change.name, change.summary), plain);
                    }
                } else {
                    line(String::new(), plain);
                    line(" nothing has changed here yet".into(), dim);
                }
            }
            Row::Plugin(r, p) => {
                let repo = &self.repos[*r];
                let plugin = &repo.plugins[*p];
                line(format!(" {} {}", plugin.name, plugin.version), title);
                if !plugin.summary.is_empty() {
                    for part in wrap(&plugin.summary, width.saturating_sub(2)) {
                        line(format!(" {part}"), plain);
                    }
                }
                line(String::new(), plain);
                line(format!(" from {}", repo.name), dim);
                match &plugin.installed {
                    Some(have) if *have == plugin.version => {
                        line(format!(" installed: {have}"), plain)
                    }
                    Some(have) => {
                        line(format!(" installed: {have} — {} available", plugin.version), plain)
                    }
                    None => line(" not installed".into(), dim),
                }

                line(String::new(), plain);
                line(" asks to".into(), title);
                if plugin.asks.is_empty() {
                    line(" do nothing in particular".into(), dim);
                }
                for ask in &plugin.asks {
                    // Bulleted once, then indented. Re-bulleting every
                    // wrapped line turned "read what your panes are
                    // showing" into two permissions, which is the one
                    // thing a list of permissions must not do.
                    for (n, part) in wrap(ask, width.saturating_sub(4)).into_iter().enumerate() {
                        line(
                            if n == 0 { format!(" - {part}") } else { format!("   {part}") },
                            plain,
                        );
                    }
                }
                // Only when it differs: repeating the same list under two
                // headings teaches the reader to skip both.
                if plugin.installed.is_some() && plugin.granted != plugin.asks {
                    line(String::new(), plain);
                    line(" you granted".into(), title);
                    if plugin.granted.is_empty() {
                        line(" nothing".into(), dim);
                    }
                    for grant in &plugin.granted {
                        for (n, part) in
                            wrap(grant, width.saturating_sub(4)).into_iter().enumerate()
                        {
                            line(
                                if n == 0 { format!(" - {part}") } else { format!("   {part}") },
                                plain,
                            );
                        }
                    }
                }
            }
        }
        out
    }

    pub fn widget(&self, size: CellSize, theme: &Theme) -> Option<Widget> {
        let g = &theme.glyphs;
        let visible = self.visible();
        let rows: Vec<DialogRow> = visible
            .iter()
            .map(|(_, row)| {
                let (lead, style) = match row {
                    Row::Repo(r) => {
                        let repo = &self.repos[*r];
                        // A repository with a problem is marked where the
                        // eye already is, rather than only in the panel.
                        let mark = if repo.problem.is_some() { "! " } else { "  " };
                        (mark.to_string(), theme.panel_title.style())
                    }
                    Row::Plugin(r, p) => {
                        let plugin = &self.repos[*r].plugins[*p];
                        // Installed plugins in the ordinary colour and the
                        // rest dim, so what you have stands out from what
                        // is merely on offer. The update is said in the
                        // row's own text rather than in a colour, because
                        // a colour cannot say "0.2.1 → 0.3.0".
                        let style = if plugin.installed.is_some() {
                            theme.panel.style()
                        } else {
                            theme.panel_dim.style()
                        };
                        (format!("  {}", g.branch), style)
                    }
                };
                DialogRow::new(self.text(row))
                    .with_lead(vec![(lead, theme.panel_dim.style())])
                    .with_style(style)
            })
            .collect();
        let selected = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        let width = crate::dialog::panel_width(size);

        // The hint carries the command, because the browser does not run
        // one. A modal that showed you a plugin and no way to act on it
        // would be a dead end.
        let hint = match self.command() {
            Some(command) => format!(" {command} · esc"),
            None => " esc".to_string(),
        };

        crate::dialog::Dialog {
            title: "plugins",
            filter: &self.filter,
            total: self.rows.len(),
            rows,
            selected,
            panel: self.panel(width, theme),
            searchable: true,
            panel_slot: width > 0,
            hint: &hint,
            id: 5,
        }
        .widget(size, theme)
    }
}

/// Break text to a width on word boundaries, because a panel is narrow
/// and a URL or a capability line will not fit.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if word.chars().count() > width {
            // A single word longer than the panel — a URL, usually. Cut
            // it rather than overflow the box, which would corrupt the
            // frame rather than merely look bad.
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let mut rest = word;
            while rest.chars().count() > width {
                let head: String = rest.chars().take(width).collect();
                out.push(head);
                rest =
                    &rest[rest.char_indices().nth(width).map(|(i, _)| i).unwrap_or(rest.len())..];
            }
            line = rest.to_string();
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_proto::{PluginChange, PluginInfo};

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn plugin(name: &str, version: &str, installed: Option<&str>) -> PluginInfo {
        PluginInfo {
            name: name.into(),
            version: version.into(),
            summary: format!("what {name} does"),
            installed: installed.map(str::to_string),
            asks: vec!["run: git".into()],
            granted: vec!["run: git".into()],
        }
    }

    fn repos() -> Vec<RepoInfo> {
        vec![
            RepoInfo {
                name: "community".into(),
                url: "https://plugins.example.com/haven".into(),
                key: "E1E1F35F2B2ED307".into(),
                checked: true,
                problem: None,
                plugins: vec![
                    plugin("gitline", "0.3.0", Some("0.2.1")),
                    plugin("urls", "1.0.0", None),
                ],
                changes: vec![PluginChange {
                    name: "gitline".into(),
                    summary: "0.2.1 → 0.3.0".into(),
                    when: 100,
                }],
            },
            RepoInfo {
                name: "work".into(),
                url: "/srv/haven".into(),
                key: "AAAABBBBCCCCDDDD".into(),
                checked: false,
                problem: Some("`work` has no index yet — run `haven plugin repo update`".into()),
                plugins: Vec::new(),
                changes: Vec::new(),
            },
        ]
    }

    #[test]
    fn the_tree_is_repositories_with_their_plugins_under_them() {
        let browser = Browser::new(repos());
        assert_eq!(
            browser.rows,
            vec![Row::Repo(0), Row::Plugin(0, 0), Row::Plugin(0, 1), Row::Repo(1)]
        );
        // A repository with nothing in it still gets a row: it is the one
        // you most need to see, because something is wrong with it.
        assert!(browser.rows.contains(&Row::Repo(1)));
    }

    #[test]
    fn it_opens_on_the_thing_you_came_to_see() {
        // An update waiting is the question people open this to answer.
        let browser = Browser::new(repos());
        assert_eq!(browser.rows[browser.selected], Row::Plugin(0, 0));
        assert_eq!(browser.updates(), 1);

        // With nothing to update, it opens at the top rather than
        // somewhere arbitrary.
        let mut quiet = repos();
        quiet[0].plugins[0].installed = Some("0.3.0".into());
        let browser = Browser::new(quiet);
        assert_eq!(browser.selected, 0);
        assert_eq!(browser.updates(), 0);
    }

    #[test]
    fn a_row_says_what_state_its_plugin_is_in() {
        let browser = Browser::new(repos());
        let text = |row| browser.text(&row);
        // Compact, because the row is narrow and was being clipped
        // mid-phrase; and the same arrow the change log uses.
        assert_eq!(text(Row::Plugin(0, 0)), "gitline  0.2.1 → 0.3.0");
        assert!(text(Row::Plugin(0, 1)).ends_with("1.0.0"), "not installed says nothing extra");
        let mut same = repos();
        same[0].plugins[0].installed = Some("0.3.0".into());
        assert!(Browser::new(same).text(&Row::Plugin(0, 0)).contains("installed"));
    }

    #[test]
    fn a_repository_whose_key_was_never_checked_says_so_on_its_own_row() {
        // Not only in the panel: the row is what gets read.
        let browser = Browser::new(repos());
        assert!(browser.text(&Row::Repo(1)).contains("key never checked"));
        assert!(!browser.text(&Row::Repo(0)).contains("never checked"));
    }

    #[test]
    fn filtering_a_repositorys_name_keeps_its_plugins() {
        // Narrowing to one row with nothing under it would be useless.
        let mut browser = Browser::new(repos());
        browser.filter = "community".into();
        browser.resettle();
        let shown: Vec<&Row> = browser.visible().iter().map(|(_, r)| *r).collect();
        assert!(shown.contains(&&Row::Plugin(0, 0)), "{shown:?}");
        assert!(shown.contains(&&Row::Plugin(0, 1)), "{shown:?}");
        assert!(!shown.contains(&&Row::Repo(1)));

        // And a plugin is found by what it does, not only by its name.
        browser.filter = "what urls does".into();
        browser.resettle();
        let shown: Vec<&Row> = browser.visible().iter().map(|(_, r)| *r).collect();
        assert_eq!(shown, vec![&Row::Plugin(0, 1)]);
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_the_selection_somewhere_real() {
        let mut browser = Browser::new(repos());
        browser.filter = "zzzz".into();
        browser.resettle();
        assert!(browser.visible().is_empty());
        // And moving in an empty list does not panic or wrap to nowhere.
        browser.move_by(1);
        browser.move_by(-1);
    }

    #[test]
    fn moving_wraps_in_both_directions() {
        let mut browser = Browser::new(repos());
        browser.selected = 0;
        browser.move_by(-1);
        assert_eq!(browser.selected, browser.rows.len() - 1);
        browser.move_by(1);
        assert_eq!(browser.selected, 0);
    }

    #[test]
    fn the_hint_is_the_command_for_what_is_selected() {
        // The browser does not install; it says what would.
        let mut browser = Browser::new(repos());
        browser.selected = 1; // gitline, 0.2.1 installed, 0.3.0 available
        assert_eq!(browser.command().unwrap(), "haven plugin install gitline");
        browser.selected = 2; // urls, not installed
        assert_eq!(browser.command().unwrap(), "haven plugin install urls");
        browser.selected = 0; // the repository
        assert_eq!(browser.command().unwrap(), "haven plugin repo update community");

        let mut installed = repos();
        installed[0].plugins[1].installed = Some("1.0.0".into());
        let mut browser = Browser::new(installed);
        browser.selected = 2;
        assert_eq!(browser.command().unwrap(), "haven plugin remove urls");
    }

    #[test]
    fn the_panel_is_about_whatever_is_selected() {
        let theme = Theme::default();
        let mut browser = Browser::new(repos());

        browser.selected = 0;
        let shown = flat(&browser.panel(40, &theme));
        assert!(shown.contains("community"), "{shown}");
        assert!(shown.contains("key E1E1F35F2B2ED307"), "{shown}");
        assert!(shown.contains("recent changes"), "{shown}");
        assert!(shown.contains("0.2.1 → 0.3.0"), "{shown}");

        browser.selected = 1;
        let shown = flat(&browser.panel(40, &theme));
        assert!(shown.contains("gitline 0.3.0"), "{shown}");
        assert!(shown.contains("installed: 0.2.1"), "{shown}");
        assert!(shown.contains("asks to"), "what it wants has to be here: {shown}");
        assert!(shown.contains("run: git"), "{shown}");

        // A repository that could not be read says why, where the reason
        // fits.
        browser.selected = 3;
        let shown = flat(&browser.panel(40, &theme));
        assert!(shown.contains("problem"), "{shown}");
        assert!(shown.contains("repo update"), "{shown}");
        assert!(shown.contains("never checked"), "{shown}");
    }

    #[test]
    fn a_narrow_screen_gets_no_panel_rather_than_a_broken_one() {
        let theme = Theme::default();
        let browser = Browser::new(repos());
        assert!(browser.panel(0, &theme).is_empty());
        // And the dialog still draws.
        assert!(browser.widget(size(40, 10), &theme).is_some());
    }

    /// The box has to open on the keypress, not when the answer gets
    /// back: the round trip in between is a round trip of keystrokes
    /// going to the pane behind it. Found by pressing `u` then `j` and
    /// finding a `j` at the shell prompt.
    #[test]
    fn a_browser_that_is_still_waiting_draws_and_eats_keys() {
        let theme = Theme::default();
        let browser = Browser::loading();
        assert!(browser.loading);
        // It draws, so it is on screen and therefore consuming keys.
        assert!(browser.widget(size(80, 24), &theme).is_some());
        let shown = flat(&browser.panel(40, &theme));
        assert!(shown.contains("looking"), "a blank panel reads as an empty answer: {shown}");
        // And nothing in it panics while it is empty.
        let mut browser = browser;
        browser.move_by(1);
        assert_eq!(browser.command(), None);
    }

    #[test]
    fn what_was_typed_while_it_loaded_is_still_the_search_afterwards() {
        // Typing is what people do in the round trip before the listing
        // lands. Discarding it when the answer arrives loses the input
        // just as surely as not opening the box until then.
        let mut waiting = Browser::loading();
        waiting.filter.push_str("urls");
        let filled = waiting.filled(repos());
        assert_eq!(filled.filter, "urls");
        assert!(!filled.loading);
        // And the selection is something the filter actually shows.
        let shown: Vec<&Row> = filled.visible().iter().map(|(_, r)| *r).collect();
        assert_eq!(shown, vec![&Row::Plugin(0, 1)]);
        assert_eq!(filled.rows[filled.selected], Row::Plugin(0, 1));
    }

    #[test]
    fn nothing_at_all_is_a_browser_that_knows_it() {
        let browser = Browser::new(Vec::new());
        assert!(browser.is_empty());
        assert!(browser.rows.is_empty());
        assert_eq!(browser.command(), None);
        assert_eq!(browser.updates(), 0);
        // It must still draw: an empty modal is better than a panic.
        let theme = Theme::default();
        assert!(browser.widget(size(80, 24), &theme).is_some());
    }

    /// A permission that is too long for the panel is one permission,
    /// not two. Re-bulleting each wrapped line said "read what your
    /// panes are" and "showing" as separate things it wanted.
    #[test]
    fn a_wrapped_permission_is_still_one_permission() {
        let theme = Theme::default();
        let mut repos = repos();
        repos[0].plugins[1].asks = vec!["read what your panes are showing".into()];
        let mut browser = Browser::new(repos);
        browser.selected = 2;
        let shown = flat(&browser.panel(24, &theme));
        let bullets = shown.lines().filter(|l| l.trim_start().starts_with("- ")).count();
        assert_eq!(bullets, 1, "one permission, one bullet:\n{shown}");
        // And the continuation is still there, lined up under the first.
        assert!(shown.contains("showing"), "{shown}");
    }

    #[test]
    fn wrapping_breaks_on_words_and_cuts_what_cannot_be_broken() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
        assert!(wrap("anything", 0).is_empty());
        // A URL is one word and longer than the panel. Overflowing the
        // box would corrupt the frame, not just look wrong.
        let long = wrap("https://plugins.example.com/haven/very/deep/path", 10);
        assert!(long.iter().all(|l| l.chars().count() <= 10), "{long:?}");
        assert_eq!(long.concat(), "https://plugins.example.com/haven/very/deep/path");
        // Multi-byte characters must not be split mid-character.
        let wide = wrap("ααααααααααααα", 5);
        assert!(wide.iter().all(|l| l.chars().count() <= 5), "{wide:?}");
    }

    fn flat(panel: &[Vec<Run>]) -> String {
        panel
            .iter()
            .map(|line| line.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
