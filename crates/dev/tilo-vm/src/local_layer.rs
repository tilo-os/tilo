//! Validate frozen local component bytes before using the ordinary native importer.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path};

use sha2::{Digest, Sha256};
use tilo_vm_protocol::manifest::Manifest;
use tilo_vm_protocol::{Provenance, layer_hash};

use crate::cli::LocalLayerImport;
use crate::error::CliError;

const MAX_ARCHIVE: u64 = 8 << 30;
const MAX_PROVENANCE: u64 = 64 << 10;
const MAX_ENTRIES: usize = 100_000;

pub(crate) struct Prepared {
    pub archive: tempfile::NamedTempFile,
    pub provenance: Provenance,
    pub hash: String,
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::InvalidInput(format!("local layer: {}", message.into()))
}

fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn bounded_file(path: &Path, bound: u64) -> Result<File, CliError> {
    let metadata = std::fs::symlink_metadata(path).map_err(CliError::io("inspect local input"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > bound {
        return Err(invalid("input must be a bounded regular file"));
    }
    File::open(path).map_err(CliError::io("open local input"))
}

fn freeze(request: &LocalLayerImport) -> Result<tempfile::NamedTempFile, CliError> {
    if !digest(&request.archive_sha256, 64) {
        return Err(invalid("a qualified producer archive SHA-256 is required"));
    }
    let mut source = bounded_file(&request.archive, MAX_ARCHIVE)?;
    let mut frozen =
        tempfile::NamedTempFile::new().map_err(CliError::io("freeze local archive"))?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 << 10];
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(CliError::io("read local archive"))?;
        if count == 0 {
            break;
        }
        bytes = bytes.saturating_add(count as u64);
        if bytes > MAX_ARCHIVE {
            return Err(invalid("archive grew past its bound"));
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| invalid("archive read exceeds its buffer"))?;
        hash.update(chunk);
        frozen
            .write_all(chunk)
            .map_err(CliError::io("freeze archive bytes"))?;
    }
    if hex::encode(hash.finalize()) != request.archive_sha256 {
        return Err(invalid(
            "archive differs from the qualified producer SHA-256",
        ));
    }
    frozen
        .rewind()
        .map_err(CliError::io("rewind frozen archive"))?;
    Ok(frozen)
}

fn allowed(path: &Path, files: &[String], directory: bool) -> bool {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return false;
    }
    files.iter().any(|file| {
        let target = Path::new(file.trim_start_matches('/').trim_end_matches('/'));
        if directory {
            (file.ends_with('/') && path.starts_with(target))
                || (path != target && target.starts_with(path))
        } else {
            (!file.ends_with('/') && path == target)
                || (file.ends_with('/') && path.starts_with(target) && path != target)
        }
    })
}

fn unpack(archive: &mut File, tree: &Path, files: &[String]) -> Result<(), CliError> {
    let mut reader = tar::Archive::new(archive);
    reader.set_preserve_permissions(true);
    let mut seen = HashSet::new();
    for item in reader
        .entries()
        .map_err(CliError::io("read component entries"))?
    {
        let mut entry = item.map_err(CliError::io("read component entry"))?;
        let path = entry
            .path()
            .map_err(CliError::io("read component path"))?
            .into_owned();
        let kind = entry.header().entry_type();
        if seen.len() >= MAX_ENTRIES
            || !seen.insert(path.clone())
            || (!kind.is_file() && !kind.is_dir())
            || !allowed(&path, files, kind.is_dir())
            || entry
                .header()
                .mode()
                .map_err(CliError::io("read component mode"))?
                & 0o7000
                != 0
        {
            return Err(invalid(
                "archive has duplicate, linked, privileged or undeclared entries",
            ));
        }
        if !entry
            .unpack_in(tree)
            .map_err(CliError::io("unpack local component"))?
        {
            return Err(invalid("component entry escapes its private staging tree"));
        }
    }
    for file in files {
        let path = tree.join(file.trim_start_matches('/'));
        let present = if file.ends_with('/') {
            path.is_dir()
        } else {
            path.is_file()
        };
        if !present {
            return Err(invalid(format!(
                "archive is missing declared component file {file}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn prepare(request: &LocalLayerImport) -> Result<Prepared, CliError> {
    let manifest = Manifest::load(&request.manifest)
        .map_err(|error| invalid(error.to_string()))?
        .ok_or_else(|| invalid("component manifest is missing"))?;
    let component = manifest
        .components
        .get(&request.component)
        .ok_or_else(|| invalid("component is not declared in the manifest"))?;
    if component.boot || component.image_assembled || component.paths.is_empty() {
        return Err(invalid(
            "boot and signed image-assembled cohorts use their maintained workflows",
        ));
    }
    let mut bytes = Vec::new();
    bounded_file(&request.provenance, MAX_PROVENANCE)?
        .take(MAX_PROVENANCE + 1)
        .read_to_end(&mut bytes)
        .map_err(CliError::io("read local provenance"))?;
    if bytes.len() as u64 > MAX_PROVENANCE {
        return Err(invalid("provenance grew past its bound"));
    }
    let provenance: Provenance = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("invalid provenance: {error}")))?;
    if provenance.authority != "local"
        || provenance.component != request.component
        || !provenance
            .source_commit
            .as_deref()
            .is_some_and(|s| digest(s, 40))
        || !provenance
            .cargo_lock_hash
            .as_deref()
            .is_some_and(|s| digest(s, 64))
        || provenance.dirty_tree_hash.is_some()
        || provenance.toolchain.as_deref().is_none_or(str::is_empty)
        || provenance.target.as_deref().is_none_or(str::is_empty)
    {
        return Err(invalid(
            "exact-source local producer provenance is required; CI authority is not inferred",
        ));
    }
    let mut archive = freeze(request)?;
    let tree = tempfile::tempdir().map_err(CliError::io("stage local component"))?;
    unpack(archive.as_file_mut(), tree.path(), &component.paths)?;
    let layer_hash =
        layer_hash::tree_hash(tree.path()).map_err(CliError::io("hash local component"))?;
    archive
        .rewind()
        .map_err(CliError::io("rewind verified local archive"))?;
    Ok(Prepared {
        archive,
        provenance,
        hash: layer_hash,
    })
}

#[cfg(test)]
mod tests;
