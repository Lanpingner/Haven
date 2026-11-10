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
use haven_plugin::repo::{self, Paths, Source, Sources};
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
        ["install", name] => install(name, &options, &paths),
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

    let Some(wanted) = &options.key else {
        return Err("`repo add` needs `--key <fingerprint>`: the fingerprint is what makes the \
             repository's signature mean anything, and it has to reach you from somewhere \
             other than the repository itself"
            .to_string());
    };
    let fingerprint = sign::normalise_fingerprint(wanted).map_err(|e| e.to_string())?;

    let name = match &options.name {
        Some(name) => name.clone(),
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

    // The key: from a file if one was given, otherwise from the repository
    // itself. Either way it is checked against the fingerprint, which is
    // what makes fetching it safe.
    let key = match &options.key_file {
        Some(path) => {
            let armored = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let key = Key::parse(&armored).map_err(|e| e.to_string())?;
            if key.fingerprint != fingerprint {
                return Err(format!(
                    "{path} holds key {}, not {}",
                    sign::short(&key.fingerprint),
                    sign::short(&fingerprint)
                ));
            }
            key
        }
        None => repo::fetch_key(url, &fingerprint, &Tools, paths, &name)
            .map_err(|e| format!("fetching the repository's key: {e}"))?,
    };

    repo::Keyring::new(paths.clone()).add(&key).map_err(|e| e.to_string())?;
    let source = Source { name: name.clone(), url: url.to_string(), fingerprint };
    sources.add(source.clone()).map_err(|e| e.to_string())?;
    sources.save(paths).map_err(|e| e.to_string())?;

    println!("added {name} ({url})");
    println!("  key {}", sign::short(&source.fingerprint));
    for id in &key.identities {
        println!("      {id}");
    }
    // The identities are the key's own claims about itself, so say so.
    println!("  (the name on a key is whatever its owner typed; the fingerprint is the part that matters)");

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
            Err(e) => format!("no usable index: {e}"),
        };
        println!("{}  {}", source.name, source.url);
        println!("  key {}  {state}", sign::short(&source.fingerprint));
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

fn install(name: &str, options: &Options, paths: &Paths) -> Result<(), String> {
    let (source, plugin) = find(name, paths)?;
    let installed = Installed::load(paths).map_err(|e| e.to_string())?;

    if let Some(record) = installed.get(name) {
        if record.version == plugin.version && record.sha256 == plugin.sha256 {
            println!("{} {} is already installed", plugin.name, plugin.version);
            return Ok(());
        }
    }

    // Everything it would newly be allowed to do, which for a first
    // install is everything it asks for.
    let asking = installed.newly_asked(&plugin);
    let granted = if asking.is_empty() {
        plugin.capabilities.clone()
    } else {
        if !agreed(&plugin, &asking, options.yes)? {
            return Err("not installed".to_string());
        }
        plugin.capabilities.clone()
    };

    let record =
        store::install(&source, &plugin, &granted, &Tools, paths).map_err(|e| e.to_string())?;
    println!("installed {} {} from {}", record.name, record.version, record.repo);
    if !granted.is_empty() {
        println!("  granted {granted}");
    }
    // Said plainly, because it is the state of this work and a user who
    // installs something expecting it to do something deserves to know.
    println!("  (nothing loads plugins yet: this is the distribution half)");
    Ok(())
}

/// Put the grant to the user, and get a yes or a no.
fn agreed(plugin: &Plugin, asking: &Capabilities, yes: bool) -> Result<bool, String> {
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
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "`{}` asks for permissions and there is nobody here to grant them — \
             run this in a terminal, or pass `--yes` if you have read the above",
            plugin.name
        ));
    }
    print!("\nallow this? [y/N] ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
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
  haven plugin install <name>       fetch, verify and unpack it
  haven plugin remove <name>
  haven plugin list                 what is installed

options:
  --key <fingerprint>  the one key allowed to sign this repository's index.
                       40 hex digits, however you paste them. A short key id
                       is refused: those can be forged
  --key-file <path>    an armored public key on disk. Without it the key is
                       fetched from the repository itself and checked against
                       --key, which is what makes the fingerprint the thing
                       that matters
  --name <n>           what to call the repository (default: from its URL)
  --yes                grant what a plugin asks for without being asked
  --root <dir>         keep sources, keys and plugins under <dir> instead of
                       the usual places

A repository is a git repository, an HTTPS directory, or a directory on this
machine. Which, is read from the URL. It holds `plugins.yaml` and a detached
signature `plugins.yaml.sig`, and may hold its public key as `key.asc`.
";
