//! The wasmtime version, for naming precompiled artifacts.
//!
//! A precompiled component is only loadable by the version of wasmtime
//! that produced it, so the version belongs in the filename: an upgrade
//! then misses the cache and recompiles, instead of finding a file it
//! cannot use.
//!
//! The version is read out of the lockfile rather than guessed. wasmtime
//! does not set `links`, so there is no `DEP_WASMTIME_VERSION` to read,
//! and `CARGO_PKG_VERSION` here is *this* crate's — which was the first
//! version of this script and was simply wrong. Loading a stale artifact
//! fails safely and it is recompiled, so the bug was invisible; a cache
//! key that does not include what it is keyed on is still a cache key
//! waiting to be believed.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let lock = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());

    let version = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|text| wasmtime_version(&text))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=WASMTIME_VERSION_TAG={version}");
}

/// The `version` of the `wasmtime` package in a lockfile.
///
/// A lockfile is TOML and this is not a TOML parser: it looks for the
/// `name = "wasmtime"` line and takes the `version` that follows it in
/// the same block. Pulling in a TOML dependency in a build script to
/// read one field would cost more than it is worth.
fn wasmtime_version(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "name = \"wasmtime\"" {
            continue;
        }
        for next in lines.by_ref() {
            let next = next.trim();
            // The next block has begun, so this entry had no version.
            if next.starts_with("[[package]]") {
                break;
            }
            if let Some(rest) = next.strip_prefix("version = \"") {
                return rest.strip_suffix('"').map(str::to_string);
            }
        }
        return None;
    }
    None
}
