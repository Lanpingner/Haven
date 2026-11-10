//! Who signed an index, and whether that is who we trust.
//!
//! The shape is apt's. A repository is added once, with a key, and the
//! fingerprint of that key is written down. From then on every index
//! fetched from that repository must carry a detached signature made by
//! that key — not by "a key in the keyring", which is the mistake apt
//! itself spent years undoing. A repository that can sign for itself
//! cannot sign for another one.
//!
//! The index in turn carries a SHA-256 for every artifact it lists, so one
//! signature over one small file covers everything installed from it. That
//! is the whole chain: pinned fingerprint → signature → index → hash →
//! artifact. Each link is checked here or in [`crate::store`], and an
//! artifact whose hash does not match the signed index is never unpacked.
//!
//! Verification is [rpgp], not a `gpg` child process: a terminal should not
//! need GnuPG installed to update a plugin, and the keyring is Haven's own
//! file, so adding a repository never touches the user's personal one.
//!
//! [rpgp]: https://docs.rs/pgp

use std::fmt;

use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::types::KeyDetails;
use sha2::{Digest, Sha256};

/// A key Haven has been told to trust, in the form it is kept on disk.
#[derive(Clone, Debug)]
pub struct Key {
    /// The primary key's fingerprint, lowercase hex, no spaces. This is
    /// what a source pins and what a user is shown.
    pub fingerprint: String,
    /// The user ids the key carries, for showing at `repo add` time. They
    /// are the key's own claims about itself and are worth exactly as much
    /// as that: shown, never matched on.
    pub identities: Vec<String>,
    armored: String,
    parsed: SignedPublicKey,
}

impl Key {
    /// Read an armored public key, the `gpg --armor --export` form.
    ///
    /// The key is self-checked on the way in: every subkey's binding
    /// signature has to be made by the primary. Without that, anyone may
    /// staple their own signing subkey onto a copy of a trusted key, and
    /// the fingerprint a user pinned — the *primary's* — would still match.
    pub fn parse(armored: &str) -> Result<Self, Error> {
        let (parsed, _) = SignedPublicKey::from_string(armored)
            .map_err(|e| Error::Key(format!("not an armored public key: {e}")))?;
        parsed
            .verify_bindings()
            .map_err(|e| Error::Key(format!("the key does not vouch for its own subkeys: {e}")))?;
        // A user id is bytes, not text: it is whatever the key's owner
        // typed, in whatever encoding, and it is only ever shown.
        let identities = parsed
            .details
            .users
            .iter()
            .map(|u| String::from_utf8_lossy(u.id.id()).to_string())
            .collect();
        Ok(Self {
            fingerprint: fingerprint_of(&parsed),
            identities,
            armored: armored.to_string(),
            parsed,
        })
    }

    /// The armored form, to write into Haven's keyring.
    pub fn armored(&self) -> &str {
        &self.armored
    }

    /// Check a detached signature over `data`.
    ///
    /// The signing key may be the primary or any of its subkeys — GnuPG
    /// signs with a subkey by default, so a verifier that only tries the
    /// primary rejects almost every real key. What the caller pinned is the
    /// primary's fingerprint either way.
    pub fn verify(&self, signature: &str, data: &[u8]) -> Result<(), Error> {
        let (sig, _) = DetachedSignature::from_string(signature)
            .map_err(|e| Error::Signature(format!("not an armored signature: {e}")))?;
        if sig.verify(&self.parsed, data).is_ok() {
            return Ok(());
        }
        for sub in &self.parsed.public_subkeys {
            if sig.verify(sub, data).is_ok() {
                return Ok(());
            }
        }
        Err(Error::Signature(format!(
            "the signature was not made by {} or any of its subkeys",
            short(&self.fingerprint)
        )))
    }
}

/// Lowercase hex, which is the one form everything here compares.
fn fingerprint_of(key: &SignedPublicKey) -> String {
    key.fingerprint().as_bytes().iter().fold(String::new(), |mut out, b| {
        out.push_str(&format!("{b:02x}"));
        out
    })
}

/// Normalise a fingerprint as a user might type it: `gpg` prints them in
/// groups, people paste them with spaces, and `0x` is a common prefix.
/// Case is not significant in hex and neither is the grouping.
pub fn normalise_fingerprint(text: &str) -> Result<String, Error> {
    let cleaned: String =
        text.chars().filter(|c| !c.is_whitespace() && *c != ':').collect::<String>().to_lowercase();
    // `strip_prefix`, not `trim_start_matches`: the latter eats as many
    // as it finds, so `0x0x<40 hex>` normalised instead of being refused.
    let cleaned = cleaned.strip_prefix("0x").unwrap_or(&cleaned).to_string();
    // 40 for v4, 64 for v6. A shorter "fingerprint" is a key id, and
    // pinning a key id is how you get pinned to a key somebody else made:
    // 64-bit ids are forgeable and 32-bit ones are trivially so.
    if !matches!(cleaned.len(), 40 | 64) {
        return Err(Error::Key(format!(
            "`{text}` is {} characters; a fingerprint is 40 (or 64), and a short key id \
             is not enough to pin a key to",
            cleaned.len()
        )));
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Key(format!("`{text}` is not hex")));
    }
    Ok(cleaned)
}

/// The last sixteen hex digits, which is what a fingerprint is shown as
/// when the whole thing would not earn its width.
pub fn short(fingerprint: &str) -> String {
    let tail = fingerprint.len().saturating_sub(16);
    fingerprint[tail..].to_uppercase()
}

/// Hex SHA-256 of some bytes, the form an index writes.
pub fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut out, b| {
        out.push_str(&format!("{b:02x}"));
        out
    })
}

/// Check bytes against the hash an index gave for them.
pub fn check_sha256(data: &[u8], expected: &str) -> Result<(), Error> {
    let got = sha256(data);
    // Case-insensitively, because a hand-written index may use either, and
    // in constant time, because there is no reason not to.
    if eq_ignore_case_constant_time(&got, expected) {
        return Ok(());
    }
    Err(Error::Hash { expected: expected.to_lowercase(), got })
}

/// Compare two hex strings without an early return.
///
/// Nothing here is secret, so this is not load-bearing — but a hash
/// comparison that short-circuits is a habit worth not having, and the
/// strings are 64 bytes.
fn eq_ignore_case_constant_time(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x.to_ascii_lowercase() ^ y.to_ascii_lowercase();
    }
    diff == 0
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Key(String),
    Signature(String),
    /// The artifact is not the one the signed index described.
    Hash {
        expected: String,
        got: String,
    },
    /// A signature that verifies, by a key that is not the pinned one.
    WrongKey {
        pinned: String,
        signed_by: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(why) => write!(f, "{why}"),
            Self::Signature(why) => write!(f, "{why}"),
            Self::Hash { expected, got } => write!(
                f,
                "the file does not match the index: it hashes to {}, the index says {}",
                short(got),
                short(expected)
            ),
            Self::WrongKey { pinned, signed_by } => write!(
                f,
                "signed by {}, but this repository is pinned to {}",
                short(signed_by),
                short(pinned)
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    const REPO_FPR: &str = "ba205fb38b5fba0fffb59cc1e1e1f35f2b2ed307";

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn text(name: &str) -> String {
        String::from_utf8(fixture(name)).expect("utf-8")
    }

    fn repo_key() -> Key {
        Key::parse(&text("repo-key.asc")).expect("the fixture key should parse")
    }

    /// The fixtures are made by GnuPG itself — `--quick-generate-key`,
    /// `--armor --export`, `--detach-sign --armor` — because the thing being
    /// tested is interoperation with what people actually have on their
    /// machines, not with rpgp's own output.
    #[test]
    fn a_signature_made_by_gpg_verifies_and_names_its_key() {
        let key = repo_key();
        assert_eq!(key.fingerprint, REPO_FPR);
        assert!(
            key.identities.iter().any(|id| id.contains("example.invalid")),
            "the key's own user id should be readable: {:?}",
            key.identities
        );
        key.verify(&text("plugins.yaml.sig"), &fixture("plugins.yaml")).expect("should verify");
    }

    #[test]
    fn a_changed_index_does_not_verify() {
        let key = repo_key();
        let sig = text("plugins.yaml.sig");
        let mut index = fixture("plugins.yaml");
        index.extend_from_slice(b"  - name: evil\n    version: 9.9.9\n");
        let e = key.verify(&sig, &index).expect_err("appending to a signed file must be caught");
        assert!(matches!(e, Error::Signature(_)), "{e}");
    }

    #[test]
    fn a_signature_from_another_key_is_not_this_repositorys() {
        // The repository is pinned to one key. A valid signature by some
        // other key — and this one *is* valid, by a real key — is exactly
        // the attack pinning exists to stop.
        let key = repo_key();
        let other = Key::parse(&text("other-key.asc")).expect("the other key should parse");
        assert_ne!(other.fingerprint, key.fingerprint);
        other
            .verify(&text("plugins.yaml.other.sig"), &fixture("plugins.yaml"))
            .expect("the other key really did sign it");
        let e = key
            .verify(&text("plugins.yaml.other.sig"), &fixture("plugins.yaml"))
            .expect_err("a signature by another key must not pass");
        assert!(matches!(e, Error::Signature(_)), "{e}");
    }

    #[test]
    fn rubbish_in_place_of_a_key_or_a_signature_is_an_error_not_a_panic() {
        for bad in ["", "not a key", "-----BEGIN PGP PUBLIC KEY BLOCK-----\nxx\n", "\0\0\0"] {
            assert!(Key::parse(bad).is_err(), "{bad:?} should not be a key");
        }
        let key = repo_key();
        for bad in ["", "-----BEGIN PGP SIGNATURE-----\n\nnope\n-----END PGP SIGNATURE-----\n"] {
            assert!(key.verify(bad, b"data").is_err(), "{bad:?} should not be a signature");
        }
    }

    #[test]
    fn a_fingerprint_is_accepted_however_it_was_pasted() {
        let spaced = "BA20 5FB3 8B5F BA0F  FFB5 9CC1 E1E1 F35F 2B2E D307";
        assert_eq!(normalise_fingerprint(spaced).unwrap(), REPO_FPR);
        assert_eq!(normalise_fingerprint(&REPO_FPR.to_uppercase()).unwrap(), REPO_FPR);
        assert_eq!(normalise_fingerprint(&format!("0x{REPO_FPR}")).unwrap(), REPO_FPR);
    }

    #[test]
    fn a_short_key_id_is_refused_rather_than_padded() {
        // A 64-bit key id can be collided on a laptop, and a 32-bit one in
        // seconds. Accepting one would make the pin decorative.
        for bad in ["E1E1F35F2B2ED307", "2B2ED307", "", "zz20 5fb3"] {
            let e = normalise_fingerprint(bad).expect_err("{bad} should be refused");
            assert!(matches!(e, Error::Key(_)), "{e}");
        }
    }

    #[test]
    fn a_hash_names_both_sides_when_it_does_not_match() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        check_sha256(b"abc", &sha256(b"abc").to_uppercase()).expect("case should not matter");
        let e = check_sha256(b"abc", &sha256(b"abd")).expect_err("different bytes, same hash?");
        let shown = e.to_string();
        assert!(shown.contains("does not match the index"), "{shown}");
    }
}
