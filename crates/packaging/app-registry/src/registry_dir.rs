//! A self-hosted registry backed by a directory on disk — the
//! reference implementation of "self-hostable" from the design.
//!
//! Layout:
//!
//! ```text
//! <dir>/
//!   index.json     # the signed RegistryIndex (served as-is over file://)
//!   blobs/         # iroh-blobs FsStore of the published tapp blobs
//! ```
//!
//! A long-lived provider node ([`crate::transport::BlobNode::persistent`]
//! rooted at `<dir>/blobs`) serves the blobs; its address is baked into
//! each entry's `BlobTicket` when the entry is published. The
//! `index.json` is plain static content — copy it behind any HTTPS
//! server to turn a `file://` registry into an `https://` one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::index::RegistryIndex;

/// Path to the served index document.
pub fn index_path(dir: &Path) -> PathBuf {
    dir.join("index.json")
}

/// Path to the iroh-blobs store directory the provider node serves.
pub fn blobs_dir(dir: &Path) -> PathBuf {
    dir.join("blobs")
}

/// Read the index; error if the directory has none.
///
/// # Errors
///
/// Returns an error if `index.json` can't be read or fails to decode.
pub fn read_index(dir: &Path) -> Result<RegistryIndex> {
    let p = index_path(dir);
    let bytes = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
    RegistryIndex::from_json(&bytes).with_context(|| format!("decode {}", p.display()))
}

/// Load the index, or start a fresh one titled `registry_id` if the
/// directory has none yet.
///
/// # Errors
///
/// Returns an error if `index.json` exists but fails to read or decode.
pub fn load_or_new_index(dir: &Path, registry_id: &str) -> Result<RegistryIndex> {
    match std::fs::read(index_path(dir)) {
        Ok(bytes) => RegistryIndex::from_json(&bytes).context("decode existing index.json"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RegistryIndex::new(registry_id)),
        Err(e) => Err(e).context("read index.json"),
    }
}

/// Write the index atomically (tmp + rename).
///
/// # Errors
///
/// Returns an error if `dir` can't be created, the index fails to
/// encode, or the atomic write fails.
pub fn write_index(dir: &Path, index: &RegistryIndex) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let bytes = index.to_json().context("encode index.json")?;
    let target = index_path(dir);
    tilo_files::native::atomic::write(&target, &bytes)
        .with_context(|| format!("write {}", target.display()))?;
    Ok(())
}
