//! `haven plugin …`: the command line over [`haven_plugin`].
//!
//! The library does the trusting and this does the asking. Nothing in
//! `haven-plugin` reads from a terminal — it could not be tested if it did
//! — so the one place a question is put to a person is here, which is also
//! the only place that knows whether anybody is listening.
//!
//! The vocabulary is apt's, because everybody already knows it:
//!
//! ```text
//! haven plugin repo add <url> --key <fingerprint> [--name <n>] [--key-file <f>]
//! haven plugin repo list
//! haven plugin repo remove <name>
//! haven plugin repo update [<name>]
//! haven plugin search [<text>]
//! haven plugin info <name>
//! haven plugin install <name> [--yes]
//! haven plugin remove <name>
//! haven plugin list
//! ```

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use haven_plugin::fetch::Tools;
use haven_plugin::index::{Capabilities, Plugin};
use haven_plugin::repo::{self, Paths, Source, Sources, Trust};
use haven_plugin::sign::{self, Key};
use haven_plugin::store::{self, Installed};

/// What the command line gives this module. A struct rather than more
/// arguments, because every subcommand wants a different three of them.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `repo`, `install`, … and whatever follows it.
    pub words: Vec<String>,
    pub name: Option<String>,
    pub key: Option<String>,
    pub key_file: Option<String>,
    /// Answer every grant prompt with yes. Required when nobody is there
    /// to answer one.
    pub yes: bool,
    /// Accept the key the repository itself serves, without being shown
    /// it first. For scripts, which have nobody to show it to.
    pub trust_repo_key: bool,
    /// `install --all`: everything the repositories offer.
    pub all: bool,
    /// Put the whole lot under one directory, for trying this out without
    /// touching a real configuration.
    pub root: Option<String>,
}

pub fn run(options: Options) -> ExitCode {
    let paths = match &options.root {
        Some(root) => Paths::under(root),
        None => Paths::standard(),
    };
    let words: Vec<&str> = options.words.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        [] | ["help"] => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ["repo", "add", url] => repo_add(url, &options, &paths),
        ["repo", "list"] => repo_list(&paths),
        ["repo", "remove", name] | ["repo", "rm", name] => repo_remove(name, &paths),
        ["repo", "update"] => repo_update(None, &paths),
        ["repo", "update", name] => repo_update(Some(name), &paths),
        ["search"] => search(None, &paths),
        ["search", text] => search(Some(text), &paths),
        ["info", name] => info(name, &paths),
        ["install"] if options.all => install_all(&options, &paths),
        ["install"] => Err("`install` needs a plugin name, or `--all`".to_string()),
        // Several at once, because "install these four" is a normal thing
        // to want and typing the command four times is not.
        ["install", names @ ..] => {
            let mut failed = 0;
            for name in names {
                if let Err(why) = install(name, &options, &paths) {
                    eprintln!("haven: {name}: {why}");
                    failed += 1;
                }
            }
            if failed > 0 {
                return ExitCode::FAILURE;
            }
            Ok(())
        }
        ["remove", name] | ["rm", name] => remove(name, &paths),
        ["list"] => list(&paths),
        ["repo"] => Err("`haven plugin repo` needs add, list, remove or update".to_string()),
        other => Err(format!("unknown: `haven plugin {}`", other.join(" "))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("haven: {why}");
            ExitCode::FAILURE
        }
    }
}

fn repo_add(url: &str, options: &Options, paths: &Paths) -> Result<(), String> {
    // The URL is checked before anything else happens, so a typo is a
    // sentence rather than a failed clone.
    haven_plugin::fetch::kind_of(url).map_err(|e| e.to_string())?;

    let wanted = match &options.key {
        Some(text) => Some(sign::normalise_fingerprint(text).map_err(|e| e.to_string())?),
        None => None,
    };

    let name = match &options.name {
        // A repository's name is a directory under `repos/` and the word
        // you type to mean it, so it is held to the same rule a plugin's
        // name is. `--name ../../x` used to put the cache elsewhere.
        Some(name) => repo::check_name(name)?,
        None => repo::name_from_url(url)
            .ok_or_else(|| format!("cannot make a name out of `{url}` — pass `--name <n>`"))?,
    };

    let mut sources = Sources::load(paths).map_err(|e| e.to_string())?;
    if let Some(existing) = sources.get(&name) {
        return Err(format!(
            "there is already a repository called `{name}` ({}) — \
             remove it or pass `--name`",
            existing.url
        ));
    }

    // Where the key comes from, and what checked it.
    //
    // A key file, or a fingerprint on the command line, are both
    // *verified*: what identifies the key arrived from somewhere other
    // than the repository, so a substituted key is caught. Taking the
    // repository's own key on its own is not verification — it is `ssh`'s
    // bargain with host keys. Pinned from then on, so only the first
    // contact was ever exposed, and worth a great deal more than nothing;
    // but whoever answered at that moment is who Haven now trusts.
    let (key, trust) = match (&options.key_file, &wanted) {
        (Some(path), _) => {
            let armored = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let key = Key::parse(&armored).map_err(|e| e.to_string())?;
            if let Some(fingerprint) = &wanted {
                if key.fingerprint != *fingerprint {
                    return Err(format!(
                        "{path} holds key {}, not {}",
                        sign::short(&key.fingerprint),
                        sign::short(fingerprint)
                    ));
                }
            }
            // A key file is a key you already had and chose, which is
            // what a fingerprint is for. Nothing to add.
            (key, Trust::Fingerprint)
        }
        (None, Some(fingerprint)) => (
            repo::fetch_key(url, Some(fingerprint), &Tools, paths, &name)
                .map_err(|e| format!("fetching the repository's key: {e}"))?,
            Trust::Fingerprint,
        ),
        (None, None) => {
            let key = repo::fetch_key(url, None, &Tools, paths, &name)
                .map_err(|e| format!("fetching the repository's key: {e}"))?;
            if !accepted(url, &key, options.trust_repo_key)? {
                return Err("not added".to_string());
            }
            (key, Trust::FirstContact)
        }
    };

    repo::Keyring::new(paths.clone()).add(&key).map_err(|e| e.to_string())?;
    let source = Source {
        name: name.clone(),
        url: url.to_string(),
        fingerprint: key.fingerprint.clone(),
        trust,
    };
    sources.add(source.clone()).map_err(|e| e.to_string())?;
    sources.save(paths).map_err(|e| e.to_string())?;

    println!("added {name} ({url})");
    println!("  key {}", sign::short(&source.fingerprint));
    for id in &key.identities {
        println!("      {id}");
    }
    match trust {
        // The identities are the key's own claims about itself, so say so.
        Trust::Fingerprint => println!(
            "  (the name on a key is whatever its owner typed; \
             the fingerprint is the part that matters)"
        ),
        Trust::FirstContact => {
            println!("  (taken from the repository itself: pinned from now on, but never checked)")
        }
    }

    match repo::refresh(&source, &Tools, paths) {
        Ok(got) => {
            println!("  {} plugin{}", got.index.plugins.len(), plural(got.index.plugins.len()));
            Ok(())
        }
        // The repository is added either way: the key is pinned and the
        // source is written, so `repo update` can retry. A network that
        // was down at this moment is not a reason to lose the decision.
        Err(e) => {
            eprintln!("haven: added, but its index could not be read yet: {e}");
            Ok(())
        }
    }
}

fn repo_list(paths: &Paths) -> Result<(), String> {
    let sources = Sources::load(paths).map_err(|e| e.to_string())?;
    if sources.list.is_empty() {
        println!("no plugin repositories (add one with `haven plugin repo add`)");
        return Ok(());
    }
    for source in &sources.list {
        let state = match repo::cached(source, paths) {
            Ok(index) => format!("{} plugin{}", index.plugins.len(), plural(index.plugins.len())),
            Err(e) => e.to_string(),
        };
        println!("{}  {}", source.name, source.url);
        let checked = if source.trust.is_verified() {
            String::new()
        } else {
            // Said every time it is listed, not only when it was added:
            // "I meant to compare that and did not" is a thing that
            // happens, and a month later this listing is the only place
            // it can still be noticed.
            "  (never checked)".to_string()
        };
        println!("  key {}{checked}  {state}", sign::short(&source.fingerprint));
    }
    Ok(())
}

fn repo_remove(name: &str, paths: &Paths) -> Result<(), String> {
    let mut sources = Sources::load(paths).map_err(|e| e.to_string())?;
    let Some(gone) = sources.remove(name) else {
        return Err(format!("no repository called `{name}`"));
    };
    sources.save(paths).map_err(|e| e.to_string())?;

    // The key goes only if nothing else is pinned to it. Two repositories
    // run by the same people share a key, and removing one must not
    // silently break the other.
    let shared = sources.list.iter().any(|s| s.fingerprint == gone.fingerprint);
    if !shared {
        repo::Keyring::new(paths.clone()).forget(&gone.fingerprint).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(paths.repo_dir(name));

    // Plugins installed from it are left alone, and said out loud: they
    // still work, they just have nowhere to update from.
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;
    let orphaned: Vec<&str> =
        installed.list.iter().filter(|r| r.repo == name).map(|r| r.name.as_str()).collect();
    println!("removed {name}");
    if !orphaned.is_empty() {
        println!("  still installed, with nowhere to update from: {}", orphaned.join(", "));
    }
    Ok(())
}

fn repo_update(only: Option<&str>, paths: &Paths) -> Result<(), String> {
    let sources = Sources::load(paths).map_err(|e| e.to_string())?;
    let chosen: Vec<&Source> = match only {
        Some(name) => {
            vec![sources.get(name).ok_or_else(|| format!("no repository called `{name}`"))?]
        }
        None => sources.list.iter().collect(),
    };
    if chosen.is_empty() {
        println!("no plugin repositories (add one with `haven plugin repo add`)");
        return Ok(());
    }
    // Every repository is tried, and the failures are collected rather
    // than returned: one repository being down is not a reason to leave
    // the other five un-updated.
    let mut failed = 0;
    for source in chosen {
        match repo::refresh(source, &Tools, paths) {
            Ok(got) if got.changed => println!(
                "{}: {} plugin{}",
                source.name,
                got.index.plugins.len(),
                plural(got.index.plugins.len())
            ),
            Ok(_) => println!("{}: unchanged", source.name),
            Err(e) => {
                failed += 1;
                eprintln!("haven: {}: {e}", source.name);
            }
        }
    }
    if failed > 0 {
        return Err(format!(
            "{failed} repositor{} could not be updated",
            if failed == 1 { "y" } else { "ies" }
        ));
    }
    Ok(())
}

/// Every plugin in every repository, with the repository it is in.
fn available(paths: &Paths) -> Result<Vec<(Source, Plugin)>, String> {
    let sources = Sources::load(paths).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for source in &sources.list {
        // A repository with no readable index is skipped rather than
        // fatal: `search` across five repositories should still work when
        // one of them has never been updated.
        if let Ok(index) = repo::cached(source, paths) {
            for plugin in index.plugins {
                out.push((source.clone(), plugin));
            }
        }
    }
    Ok(out)
}

fn search(text: Option<&str>, paths: &Paths) -> Result<(), String> {
    let needle = text.map(str::to_lowercase);
    let found: Vec<(Source, Plugin)> = available(paths)?
        .into_iter()
        .filter(|(_, p)| match &needle {
            None => true,
            Some(needle) => {
                p.name.to_lowercase().contains(needle)
                    || p.summary.as_deref().unwrap_or("").to_lowercase().contains(needle)
            }
        })
        .collect();
    if found.is_empty() {
        println!("nothing found (have you run `haven plugin repo update`?)");
        return Ok(());
    }
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;
    for (source, plugin) in found {
        let here = match installed.get(&plugin.name) {
            Some(r) if r.version == plugin.version => "  [installed]",
            Some(_) => "  [installed, other version]",
            None => "",
        };
        println!("{} {}  ({}){here}", plugin.name, plugin.version, source.name);
        if let Some(summary) = &plugin.summary {
            println!("    {summary}");
        }
    }
    Ok(())
}

fn info(name: &str, paths: &Paths) -> Result<(), String> {
    let (source, plugin) = find(name, paths)?;
    println!("{} {}", plugin.name, plugin.version);
    if let Some(summary) = &plugin.summary {
        println!("  {summary}");
    }
    println!("  from {} ({})", source.name, source.url);
    println!("  artifact {}  sha256 {}", plugin.file, plugin.sha256);
    if !plugin.events.is_empty() {
        println!("  wakes on {}", plugin.events.join(", "));
    }
    println!("  asks to {}", plugin.capabilities);
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;
    if let Some(record) = installed.get(name) {
        println!("  installed: {} from {}", record.version, record.repo);
        println!("  granted {}", record.granted);
    }
    Ok(())
}

fn find(name: &str, paths: &Paths) -> Result<(Source, Plugin), String> {
    let mut matches: Vec<(Source, Plugin)> =
        available(paths)?.into_iter().filter(|(_, p)| p.name == name).collect();
    match matches.len() {
        0 => Err(format!(
            "no plugin called `{name}` in any repository \
             (try `haven plugin repo update`)"
        )),
        1 => Ok(matches.remove(0)),
        // Two repositories offering the same name is not something to
        // resolve by precedence. Precedence is how you install the wrong
        // `gitline` from the repository you added for one other thing.
        _ => Err(format!(
            "`{name}` is in more than one repository ({}) — remove one, or install from \
             a repository you trust for it",
            matches.iter().map(|(s, _)| s.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Everything the repositories offer that is not already installed.
///
/// Prompts for each plugin that asks for something, rather than taking
/// `--all` as permission for all of it: three of the plugins Haven ships
/// want to read what your panes are showing, and "install everything"
/// is not the same sentence as "let everything read my screen". `--yes`
/// still answers for you, as it does everywhere else.
///
/// One plugin failing does not stop the rest. A repository that is down,
/// an artifact that does not match its hash, a grant declined — each is
/// one line and the next plugin is tried.
fn install_all(options: &Options, paths: &Paths) -> Result<(), String> {
    let offered = available(paths)?;
    if offered.is_empty() {
        println!("nothing to install (have you run `haven plugin repo update`?)");
        return Ok(());
    }
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;

    // A name offered by two repositories cannot be chosen for you — see
    // `find`. Counted and named at the end rather than stopping the run.
    let mut ambiguous: Vec<&str> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (_, plugin) in &offered {
        if seen.contains(&plugin.name.as_str()) {
            if !ambiguous.contains(&plugin.name.as_str()) {
                ambiguous.push(&plugin.name);
            }
        } else {
            seen.push(&plugin.name);
        }
    }

    let mut added = 0;
    let mut already = 0;
    let mut refused = 0;
    let mut needs_terminal = 0;
    let mut failed = 0;
    for (_, plugin) in &offered {
        if ambiguous.contains(&plugin.name.as_str()) {
            continue;
        }
        if installed
            .get(&plugin.name)
            .is_some_and(|r| r.version == plugin.version && r.sha256 == plugin.sha256)
        {
            already += 1;
            continue;
        }
        match install_one(&plugin.name, options, paths) {
            Ok(()) => added += 1,
            // Each of these reads differently in the summary, which is
            // why the reason is carried rather than parsed back out of a
            // message.
            Err(NotInstalled::Declined) => refused += 1,
            Err(NotInstalled::NeedsTerminal) => needs_terminal += 1,
            Err(NotInstalled::Failed(why)) => {
                eprintln!("haven: {}: {why}", plugin.name);
                failed += 1;
            }
        }
    }

    println!();
    print!("{added} installed, {already} already there");
    if refused > 0 {
        print!(", {refused} declined");
    }
    if failed > 0 {
        print!(", {failed} failed");
    }
    println!();
    if needs_terminal > 0 {
        // Said once rather than per plugin: nine copies of the same
        // paragraph is not nine pieces of information.
        println!(
            "{needs_terminal} want permissions and there was nobody to ask. Run this in a \
             terminal to be asked about each, or `--yes` to grant what they ask for."
        );
    }
    for name in &ambiguous {
        println!(
            "  `{name}` is in more than one repository, so it was skipped — \
             install it by name from the one you want"
        );
    }
    // A declined grant is a decision and exits cleanly. Something that
    // could not be asked, or that broke, is an obstacle: a script running
    // `install --all` and getting a clean exit having installed nothing
    // would be told the opposite of what happened.
    match (failed, needs_terminal) {
        (0, 0) => Ok(()),
        (0, n) => Err(format!("{n} could not be asked about")),
        (f, 0) => Err(format!("{f} did not install")),
        (f, n) => Err(format!("{f} did not install and {n} could not be asked about")),
    }
}

fn install(name: &str, options: &Options, paths: &Paths) -> Result<(), String> {
    install_one(name, options, paths).map_err(|e| e.to_string())
}

fn install_one(name: &str, options: &Options, paths: &Paths) -> Result<(), NotInstalled> {
    let (source, plugin) = find(name, paths).map_err(NotInstalled::Failed)?;
    let installed = Installed::load(paths).map_err(|e| NotInstalled::Failed(e.to_string()))?;

    if let Some(record) = installed.get(name) {
        if record.version == plugin.version && record.sha256 == plugin.sha256 {
            println!("{} {} is already installed", plugin.name, plugin.version);
            // Still worth compiling, and this is where it was missed:
            // `install` returned here before warming, so installing all
            // sixteen in a loop — the obvious way to try them — left
            // every cache empty and the first terminal paying for all of
            // it. `warm` does nothing when the artifact is already there.
            warm(record, paths);
            return Ok(());
        }
    }

    // Everything it would newly be allowed to do, which for a first
    // install is everything it asks for.
    let asking = installed.newly_asked(&plugin);
    let granted = if asking.is_empty() {
        plugin.capabilities.clone()
    } else {
        match agreed(&plugin, &asking, options.yes) {
            Consent::Granted => plugin.capabilities.clone(),
            Consent::Declined => return Err(NotInstalled::Declined),
            Consent::NoTerminal => return Err(NotInstalled::NeedsTerminal),
        }
    };

    let record = store::install(&source, &plugin, &granted, &Tools, paths)
        .map_err(|e| NotInstalled::Failed(e.to_string()))?;
    println!("installed {} {} from {}", record.name, record.version, record.repo);
    if !granted.is_empty() {
        println!("  granted {granted}");
    }
    // Loaded when a session is made, so an open terminal does not
    // already have it. Worth one line: otherwise the obvious conclusion
    // from "installed" and nothing on the bar is that it is broken.
    println!("  (open a new terminal, or restart the daemon, to load it)");
    Ok(())
}

/// Show the key a repository served, and get an accept or a refusal.
///
/// This is the one moment in the whole chain where a person is the
/// verification. There is nothing to compare the key against — that is
/// the situation — so the fingerprint is printed the way GnuPG prints
/// one, in groups, because the only useful thing to do with it is hold it
/// beside a fingerprint from somewhere else.
fn accepted(url: &str, key: &Key, already: bool) -> Result<bool, String> {
    println!("{url} offers this key:");
    println!();
    println!("  {}", grouped(&key.fingerprint));
    for id in &key.identities {
        println!("  {id}");
    }
    println!();
    println!(
        "Nothing has checked this. The name on a key is whatever its owner typed,\n\
         and the key was served by the repository it would vouch for — so accepting\n\
         it trusts whoever answered just now. Compare the fingerprint against one\n\
         from somewhere else (the project's page, a mail, a colleague) if you can.\n\
         \n\
         Haven will pin it: from now on only this key can sign this repository."
    );
    if already {
        println!("\naccepted (--trust-repo-key)");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err("there is nobody here to look at that key — pass `--key <fingerprint>` to \
             check it against one you already have, or `--trust-repo-key` to accept \
             whatever the repository serves"
            .to_string());
    }
    print!("\naccept this key? [y/N] ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// A fingerprint in fours, the way `gpg` prints one, so it can be
/// compared against a published one by eye without counting characters.
fn grouped(fingerprint: &str) -> String {
    let upper = fingerprint.to_uppercase();
    let groups: Vec<String> =
        upper.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).to_string()).collect();
    // The wider gap in the middle is GnuPG's, and it is what makes two
    // fingerprints side by side comparable at a glance.
    let (first, second) = groups.split_at(groups.len().div_ceil(2));
    format!("{}  {}", first.join(" "), second.join(" "))
}

/// Why an install did not happen, for a caller that has to tell the
/// difference.
///
/// `install --all` reports these separately, and the only honest way to
/// do that is to carry the reason. Matching on the text of an error
/// message works until somebody improves the wording.
#[derive(Debug)]
enum NotInstalled {
    /// The user said no.
    Declined,
    /// It needs permissions and there was nobody to ask.
    NeedsTerminal,
    Failed(String),
}

impl std::fmt::Display for NotInstalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => write!(f, "not installed"),
            Self::NeedsTerminal => write!(
                f,
                "it asks for permissions and there is nobody here to grant them — run this \
                 in a terminal, or pass `--yes` if you have read what it wants"
            ),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Put the grant to the user, and get a yes or a no.
#[derive(Debug, PartialEq, Eq)]
enum Consent {
    Granted,
    Declined,
    /// Nobody was there to be asked.
    NoTerminal,
}

fn agreed(plugin: &Plugin, asking: &Capabilities, yes: bool) -> Consent {
    println!("{} {} wants to:", plugin.name, plugin.version);
    for line in asking.lines() {
        println!("  - {line}");
    }
    if asking.read_pane_content {
        // The one that needs saying in words rather than as a bullet.
        println!(
            "\nReading your panes means everything that scrolls through them: \
             command lines, output, and any password or token that appears there."
        );
    }
    if yes {
        println!("granted (--yes)");
        return Consent::Granted;
    }
    if !std::io::stdin().is_terminal() {
        return Consent::NoTerminal;
    }
    print!("\nallow this? [y/N] ");
    if std::io::stdout().flush().is_err() {
        return Consent::NoTerminal;
    }
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Consent::NoTerminal;
    }
    if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        Consent::Granted
    } else {
        Consent::Declined
    }
}

/// Compile the plugin now, so that opening a terminal does not.
///
/// A component takes most of a second to compile and a session loads
/// every plugin it has, so sixteen of them meant ten seconds before the
/// first prompt appeared — found by installing all sixteen and waiting.
/// The compiled form is cached beside the plugin either way; this only
/// decides who waits for it, and the end of an install is a far better
/// place than the start of a terminal.
///
/// Best effort, and quiet about failing: the plugin is installed and
/// will work, it will simply be compiled when it is first loaded.
fn warm(record: &haven_plugin::store::Record, paths: &Paths) {
    let dir = store::plugin_dir(paths, &record.name);
    let Ok(wasm) = std::fs::read(dir.join(&record.entry)) else { return };
    let Ok(engine) = haven_plugin_host::Engine::new() else { return };
    // Silent when it was already compiled: `install` is run in loops and
    // a line per plugin saying nothing happened is noise.
    let already = std::fs::read_dir(&dir)
        .map(|entries| {
            entries.flatten().any(|e| e.file_name().to_string_lossy().starts_with("compiled-"))
        })
        .unwrap_or(false);
    if already {
        return;
    }
    match engine.precompile(&record.name, &wasm, &dir) {
        Ok(()) => println!("  compiled, so the first terminal does not have to"),
        // Worth one line: it means something is wrong with the component
        // that only shows up when it is loaded, and the user would
        // rather hear it now than have a plugin that never appears.
        Err(e) => eprintln!("haven: {e}"),
    }
}

fn remove(name: &str, paths: &Paths) -> Result<(), String> {
    let gone = store::remove(name, paths).map_err(|e| e.to_string())?;
    println!("removed {} {}", gone.name, gone.version);
    Ok(())
}

fn list(paths: &Paths) -> Result<(), String> {
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;
    if installed.list.is_empty() {
        println!("no plugins installed");
        return Ok(());
    }
    // Which of them a repository has moved past, so `list` answers "is
    // there anything to update" without a second command.
    let newer: Vec<(String, String)> = available(paths)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, p)| {
            let record = installed.get(&p.name)?;
            (record.version != p.version).then(|| (p.name.clone(), p.version.clone()))
        })
        .collect();
    for record in &installed.list {
        print!("{} {}  ({})", record.name, record.version, record.repo);
        if let Some((_, version)) = newer.iter().find(|(name, _)| *name == record.name) {
            print!("  → {version} available");
        }
        println!();
        if !record.granted.is_empty() {
            println!("    granted {}", record.granted);
        }
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

const USAGE: &str = "\
haven plugin — signed plugin repositories

repositories:
  haven plugin repo add <url> --key <fingerprint> [--name <n>] [--key-file <f>]
  haven plugin repo list
  haven plugin repo remove <name>
  haven plugin repo update [<name>]

plugins:
  haven plugin search [<text>]      what the repositories offer
  haven plugin info <name>          where it comes from, and what it asks for
  haven plugin install <name>...    fetch, verify and unpack them
  haven plugin install --all        everything the repositories offer
  haven plugin remove <name>
  haven plugin list                 what is installed

options:
  --key <fingerprint>  the one key allowed to sign this repository's index.
                       40 hex digits, however you paste them. A short key id
                       is refused: those can be forged
  --key-file <path>    an armored public key on disk, instead of the one the
                       repository serves
  --trust-repo-key     accept the key the repository serves without being
                       shown it. For scripts; interactively you are shown the
                       fingerprint and asked
  --name <n>           what to call the repository (default: from its URL)
  --all                with `install`, every plugin not already installed.
                       Still asks about each one that wants something
  --yes                grant what a plugin asks for without being asked
  --root <dir>         keep sources, keys and plugins under <dir> instead of
                       the usual places

A repository is a git repository, an HTTPS directory, or a directory on this
machine. Which, is read from the URL. It holds `plugins.yaml` and a detached
signature `plugins.yaml.sig`, and may hold its public key as `key.asc`.

With `--key` the repository's key is checked against a fingerprint you got from
somewhere else, which is what makes the signature mean anything. Without it, the
key is imported from the repository and shown to you to accept — pinned from
then on, but never checked, so only the first contact was ever exposed. `repo
list` marks those.
";
