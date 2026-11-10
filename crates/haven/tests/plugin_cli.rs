//! `haven plugin …` end to end, against a repository on disk.
//!
//! The repository is `crates/haven-plugin/tests/fixtures`: a real
//! `plugins.yaml`, a detached signature GnuPG actually made, the public
//! key, and real tarballs in `pool/`. So these tests run the shipped
//! binary over the whole chain — pinned fingerprint, signature, index,
//! hash, artifact — with no network, no `gpg` installed and nothing
//! touching the user's own configuration.
//!
//! The fixtures live in the other crate because that is where the library
//! tests use them too, and one repository with two readers is better than
//! two that drift.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The fixture repository, as a URL `haven plugin` will accept: an
/// absolute path, which is one of the three kinds of repository.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../haven-plugin/tests/fixtures")
}

const PINNED: &str = "BA205FB38B5FBA0FFFB59CC1E1E1F35F2B2ED307";
const OTHER_KEY: &str = "E731DFA00209F24777BFCDD3FED72B8C1328B7E3";

struct Haven {
    root: PathBuf,
}

impl Haven {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "haven-plugin-cli-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        Self { root }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_haven"))
            .arg("plugin")
            .args(args)
            .arg("--root")
            .arg(&self.root)
            // Never a terminal, so a grant prompt refuses rather than
            // blocking a test run forever waiting on stdin.
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run haven")
    }

    /// Run, and require success. Returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "haven plugin {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run, and require failure. Returns stdout and stderr together,
    /// because which stream a complaint went to is not what is being
    /// tested.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "haven plugin {args:?} should have failed:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn add(&self) -> String {
        let repo = repository();
        self.ok(&["repo", "add", repo.to_str().unwrap(), "--key", PINNED, "--name", "test"])
    }
}

impl Drop for Haven {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_repository_is_added_verified_and_its_plugins_installed() {
    let haven = Haven::new("happy");
    let added = haven.add();
    assert!(added.contains("added test"), "{added}");
    // The fingerprint is shown, and so is the key's own idea of its name —
    // which has to be presented as a claim, not as an identity.
    assert!(added.contains("E1E1F35F2B2ED307"), "{added}");
    assert!(added.contains("repo@example.invalid"), "{added}");
    assert!(added.contains("2 plugins"), "{added}");

    let listed = haven.ok(&["repo", "list"]);
    assert!(listed.contains("test"), "{listed}");
    assert!(listed.contains("2 plugins"), "{listed}");

    let found = haven.ok(&["search", "branch"]);
    assert!(found.contains("gitline 0.2.1"), "{found}");
    assert!(!found.contains("urls"), "the search should narrow: {found}");

    let about = haven.ok(&["info", "gitline"]);
    assert!(about.contains("run: git"), "what it asks for has to be visible: {about}");
    assert!(about.contains("pane-focus"), "{about}");

    let installed = haven.ok(&["install", "gitline", "--yes"]);
    assert!(installed.contains("installed gitline 0.2.1"), "{installed}");
    assert!(
        haven.root.join("data/installed/gitline/gitline.wasm").is_file(),
        "the artifact was not unpacked"
    );

    let list = haven.ok(&["list"]);
    assert!(list.contains("gitline 0.2.1"), "{list}");
    assert!(list.contains("granted"), "what was granted has to be visible later: {list}");

    let removed = haven.ok(&["remove", "gitline"]);
    assert!(removed.contains("removed gitline"), "{removed}");
    assert!(!haven.root.join("data/installed/gitline").exists(), "files left behind");
    assert!(haven.ok(&["list"]).contains("no plugins installed"));
}

#[test]
fn adding_a_repository_needs_a_fingerprint_and_a_real_one() {
    let haven = Haven::new("pin");
    let repo = repository();
    let url = repo.to_str().unwrap();

    // Without a key there is nothing for a signature to mean.
    let no_key = haven.fails(&["repo", "add", url]);
    assert!(no_key.contains("--key"), "{no_key}");

    // A short key id is forgeable, so it is refused rather than accepted
    // as a near-enough fingerprint.
    let short = haven.fails(&["repo", "add", url, "--key", "E1E1F35F2B2ED307"]);
    assert!(short.contains("fingerprint"), "{short}");

    // And a fingerprint that is not the repository's own key: the key is
    // fetched from the repository, so this is the check that makes
    // fetching it safe at all.
    let wrong = haven.fails(&["repo", "add", url, "--key", OTHER_KEY, "--name", "test"]);
    assert!(wrong.contains("pinned to"), "{wrong}");
    assert!(
        haven.ok(&["repo", "list"]).contains("no plugin repositories"),
        "a repository whose key did not match was added anyway"
    );
}

#[test]
fn an_index_signed_by_a_key_the_repository_is_not_pinned_to_is_refused() {
    // The fixture directory holds a second signature, made by a second
    // real key over the same index. Swapping it in is the attack that
    // pinning exists for: the signature is perfectly valid, and `gpg
    // --verify` with that key in the keyring calls it a good signature.
    let haven = Haven::new("wrong-signer");
    haven.add();

    let mirror = haven.root.join("mirror");
    copy_tree(&repository(), &mirror);
    std::fs::copy(mirror.join("plugins.yaml.other.sig"), mirror.join("plugins.yaml.sig"))
        .expect("swap the signature");

    // Point the source at the mirror by rewriting the sources file, which
    // is what a hostile DNS answer or a bad mirror amounts to.
    let sources = haven.root.join("config/sources.yaml");
    let text = std::fs::read_to_string(&sources).unwrap();
    let repointed = text.replace(repository().to_str().unwrap(), mirror.to_str().unwrap());
    std::fs::write(&sources, repointed).unwrap();

    let refused = haven.fails(&["repo", "update"]);
    assert!(refused.contains("was not made by"), "{refused}");
    assert!(refused.contains("E1E1F35F2B2ED307"), "it should name the pinned key: {refused}");
}

#[test]
fn a_changed_index_is_refused_and_the_last_good_one_survives() {
    let haven = Haven::new("tampered");
    haven.add();

    let mirror = haven.root.join("mirror");
    copy_tree(&repository(), &mirror);
    // One line appended to a signed file: the smallest possible change.
    let index = mirror.join("plugins.yaml");
    let mut text = std::fs::read_to_string(&index).unwrap();
    text.push_str("  - name: evil\n    version: 9.9.9\n    file: pool/evil.tar.gz\n");
    std::fs::write(&index, text).unwrap();

    let sources = haven.root.join("config/sources.yaml");
    let original = std::fs::read_to_string(&sources).unwrap();
    std::fs::write(
        &sources,
        original.replace(repository().to_str().unwrap(), mirror.to_str().unwrap()),
    )
    .unwrap();

    let refused = haven.fails(&["repo", "update"]);
    assert!(refused.contains("signature"), "{refused}");
    // And the index from before the attack is still the one in the cache,
    // which is what lets the rest of the commands keep working.
    let cached = std::fs::read_to_string(haven.root.join("data/repos/test/plugins.yaml")).unwrap();
    assert!(!cached.contains("evil"), "the tampered index reached the cache");
    assert!(haven.ok(&["search"]).contains("gitline"));
}

#[test]
fn an_artifact_that_is_not_the_one_the_index_named_is_not_installed() {
    let haven = Haven::new("swapped-artifact");
    haven.add();

    // The index is untouched and correctly signed; only the file it points
    // at has been replaced. This is a repository compromised after it
    // signed, or a mirror substituting one file.
    let mirror = haven.root.join("mirror");
    copy_tree(&repository(), &mirror);
    std::fs::copy(mirror.join("pool/urls-1.0.0.tar.gz"), mirror.join("pool/gitline-0.2.1.tar.gz"))
        .expect("swap the artifact");

    let sources = haven.root.join("config/sources.yaml");
    let original = std::fs::read_to_string(&sources).unwrap();
    std::fs::write(
        &sources,
        original.replace(repository().to_str().unwrap(), mirror.to_str().unwrap()),
    )
    .unwrap();

    let refused = haven.fails(&["install", "gitline", "--yes"]);
    assert!(refused.contains("does not match the index"), "{refused}");
    assert!(
        !haven.root.join("data/installed/gitline").exists(),
        "a plugin whose artifact did not match was unpacked"
    );
}

#[test]
fn a_plugin_that_wants_your_screen_is_not_installed_without_an_answer() {
    let haven = Haven::new("consent");
    haven.add();

    // stdin is /dev/null, so there is nobody to ask. The install has to
    // stop rather than help itself to the permission.
    let refused = haven.fails(&["install", "urls"]);
    assert!(refused.contains("read what your panes are showing"), "{refused}");
    assert!(refused.contains("password or token"), "the risk has to be spelled out: {refused}");
    assert!(refused.contains("nobody here to grant"), "{refused}");
    assert!(!haven.root.join("data/installed/urls").exists(), "installed without consent");

    // And with the answer given, it installs and the grant is recorded.
    let installed = haven.ok(&["install", "urls", "--yes"]);
    assert!(installed.contains("installed urls 1.0.0"), "{installed}");
    assert!(haven.ok(&["list"]).contains("read what your panes are showing"));
}

#[test]
fn removing_a_repository_leaves_what_was_installed_from_it_and_says_so() {
    let haven = Haven::new("orphans");
    haven.add();
    haven.ok(&["install", "gitline", "--yes"]);

    let removed = haven.ok(&["repo", "remove", "test"]);
    assert!(removed.contains("removed test"), "{removed}");
    assert!(removed.contains("gitline"), "an orphaned plugin has to be mentioned: {removed}");
    // Still installed and still on disk: removing a repository is not a
    // reason to break what is already working.
    assert!(haven.root.join("data/installed/gitline/gitline.wasm").is_file());
    assert!(haven.ok(&["list"]).contains("gitline 0.2.1"));
    // But there is nowhere to look it up any more.
    assert!(haven.fails(&["info", "gitline"]).contains("no plugin called"));
}

#[test]
fn commands_on_an_empty_installation_say_what_to_do_rather_than_failing() {
    let haven = Haven::new("empty");
    assert!(haven.ok(&["repo", "list"]).contains("no plugin repositories"));
    assert!(haven.ok(&["list"]).contains("no plugins installed"));
    assert!(haven.ok(&["search"]).contains("nothing found"));
    assert!(haven.ok(&["repo", "update"]).contains("no plugin repositories"));
    assert!(haven.fails(&["install", "gitline"]).contains("no plugin called"));
    assert!(haven.ok(&["help"]).contains("signed plugin repositories"));
}

#[test]
fn a_url_that_is_not_a_repository_is_refused_before_anything_is_fetched() {
    let haven = Haven::new("bad-url");
    // Plain HTTP, which is refused with a reason rather than silently
    // allowed because the signature would catch tampering anyway.
    let http = haven.fails(&["repo", "add", "http://example.invalid/haven", "--key", PINNED]);
    assert!(http.contains("HTTPS"), "{http}");
    // And something that is not a URL at all.
    let nonsense = haven.fails(&["repo", "add", "example.invalid/haven", "--key", PINNED]);
    assert!(nonsense.contains("not a repository"), "{nonsense}");
}

/// Copy a directory tree, which is how these tests make a mirror they can
/// tamper with without writing to the fixtures.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}
