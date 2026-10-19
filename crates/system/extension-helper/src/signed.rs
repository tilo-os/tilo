//! Signed layers: `/usr` trees an authority the owner enrolled has signed.
//!
//! The desired set is `/var/lib/tilo/signed-layers/<name>/` (tilo-vmd writes
//! it on a development machine): `manifest.json`, its detached signature
//! `manifest.json.sig`, and `payload`, a link to the layer's tree. Nothing
//! there is trusted. At every activation, for each entry, this checks the
//! signature over the manifest's exact bytes against the authorities in
//! `/etc/tilo/extension-authorities.d/` before parsing anything, then the
//! manifest, then the tree against it (`tilo_layer_trust::verify_payload`:
//! files by their kernel-measured fs-verity digest or by hashing, links,
//! executable bits, nothing added or missing, no set-id bits, nothing
//! writable by others). Only then is the layer linked as
//! `/run/extensions/<name>` for the merge. `/run` is empty at boot, so a
//! layer reaches `/usr` only through a verification at this boot.
//!
//! A refused layer is left out, and named with the reason in
//! `/run/tilo/signed-layers.json` and the journal; the others still merge,
//! and activation exits 1 so the refusal is visible as a failed unit.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tilo_layer_trust::keys::signed_by;
use tilo_layer_trust::manifest::MAX_MANIFEST_BYTES;
use tilo_layer_trust::{
    AUTHORITIES_DIR, Authority, LayerManifest, MANIFEST_FILE, PAYLOAD_LINK, SIGNATURE_FILE,
    SIGNED_LAYERS_DIR, SIGNED_LAYERS_STATUS, is_layer_name, load_authorities, verify_payload,
};

use crate::machine::{Layout, write_json};
use crate::{At, Result};

/// A signature file larger than this is not a signature.
const MAX_SIGNATURE_BYTES: u64 = 4096;

pub fn desired_dir(layout: &Layout) -> PathBuf {
    layout.at(SIGNED_LAYERS_DIR)
}

pub fn status_path(layout: &Layout) -> PathBuf {
    layout.at(SIGNED_LAYERS_STATUS)
}

pub fn authorities_dir(layout: &Layout) -> PathBuf {
    layout.at(AUTHORITIES_DIR)
}

/// Where a verified layer is linked for the merge.
pub fn link(layout: &Layout, name: &str) -> PathBuf {
    layout.at("/run/extensions").join(name)
}

/// The account everything trusted here must belong to: root on a device,
/// whoever owns the temporary root in a test.
fn owner(layout: &Layout) -> Result<u32> {
    Ok(fs::metadata(&layout.root).at(&layout.root)?.uid())
}

/// The names the last activation linked.
fn previously_linked(layout: &Layout) -> Vec<String> {
    let Ok(bytes) = fs::read(status_path(layout)) else {
        return Vec::new();
    };
    let Ok(Value::Object(status)) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    status
        .get("merged")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|layer| layer.get("name").and_then(Value::as_str))
        .filter(|name| is_layer_name(name))
        .map(str::to_owned)
        .collect()
}

fn desired_names(layout: &Layout) -> Vec<String> {
    let Ok(entries) = fs::read_dir(desired_dir(layout)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Whether there is anything to merge or to take away.
pub fn wanted(layout: &Layout) -> bool {
    !desired_names(layout).is_empty() || !previously_linked(layout).is_empty()
}

/// What one activation did with the desired set.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Linked into `/run/extensions` for the merge.
    pub linked: BTreeSet<String>,
    merged: Vec<Value>,
    refused: Vec<Value>,
    authorities: Vec<Value>,
    problems: Vec<String>,
}

impl Outcome {
    pub fn refused_any(&self) -> bool {
        !self.refused.is_empty()
    }

    fn status(&self) -> Value {
        json!({
            "schema": 1,
            "merged": self.merged,
            "refused": self.refused,
            "authorities": self.authorities,
            "problems": self.problems,
        })
    }
}

fn read_bounded(
    path: &Path,
    limit: u64,
    owner: u32,
    what: &str,
) -> std::result::Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("its {what} cannot be read: {error}"))?;
    if !meta.file_type().is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "its {what} is not a regular file owned by uid {owner} that only its owner can write"
        ));
    }
    if meta.len() > limit {
        return Err(format!("its {what} is larger than {limit} bytes"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(limit).read_to_end(&mut bytes))
        .map_err(|error| format!("its {what} cannot be read: {error}"))?;
    Ok(bytes)
}

/// Check one desired entry: the verified tree and what the status says of it.
fn verify_entry(
    entry: &Path,
    name: &str,
    authorities: &[Authority],
    owner: u32,
) -> std::result::Result<(PathBuf, Value), String> {
    if !is_layer_name(name) {
        return Err("it is not named as an extension can be".to_owned());
    }
    let meta = fs::symlink_metadata(entry).map_err(|error| error.to_string())?;
    if !meta.file_type().is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "its entry is not a directory owned by uid {owner} that only its owner can write"
        ));
    }
    let signature_path = entry.join(SIGNATURE_FILE);
    if fs::symlink_metadata(&signature_path).is_err() {
        return Err("it is unsigned".to_owned());
    }
    let bytes = read_bounded(
        &entry.join(MANIFEST_FILE),
        MAX_MANIFEST_BYTES,
        owner,
        "manifest",
    )?;
    let signature = read_bounded(&signature_path, MAX_SIGNATURE_BYTES, owner, "signature")?;
    let signature = String::from_utf8_lossy(&signature);
    let Some(authority) = signed_by(authorities, &bytes, &signature) else {
        return Err(if authorities.is_empty() {
            "its signature cannot be checked: no extension authority is enrolled".to_owned()
        } else {
            let enrolled: Vec<String> = authorities
                .iter()
                .map(|a| format!("{} {}", a.name, a.key.fingerprint()))
                .collect();
            format!(
                "it is not signed by an enrolled extension authority (enrolled: {})",
                enrolled.join(", ")
            )
        });
    };
    let manifest = LayerManifest::parse(&bytes).map_err(|error| error.to_string())?;
    if manifest.name != name {
        return Err(format!(
            "its manifest names another layer, {}",
            manifest.name
        ));
    }
    let payload = fs::canonicalize(entry.join(PAYLOAD_LINK))
        .map_err(|error| format!("its payload cannot be found: {error}"))?;
    let check = verify_payload(&payload, &manifest, owner).map_err(|error| error.to_string())?;
    let record = json!({
        "name": name,
        "component": manifest.component,
        "revision": manifest.revision,
        "generation": manifest.generation,
        "base_image_digest": manifest.base_image_digest,
        "authority": authority.name,
        "fingerprint": authority.key.fingerprint(),
        "files_by_verity": check.by_verity,
        "files_hashed": check.by_hash,
    });
    Ok((payload, record))
}

fn remove_link(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::remove_file(path).at(path),
        _ => Ok(()),
    }
}

/// Verify the desired set and link exactly the layers that pass.
///
/// # Errors
/// The links or the status cannot be written; a refused layer is not an
/// error, it is in the outcome.
pub fn activate(layout: &Layout) -> Result<Outcome> {
    let owner = owner(layout)?;
    for name in previously_linked(layout) {
        remove_link(&link(layout, &name))?;
    }
    let (authorities, problems) = load_authorities(&authorities_dir(layout), owner);
    let mut outcome = Outcome {
        authorities: authorities
            .iter()
            .map(|a| json!({"name": a.name, "fingerprint": a.key.fingerprint()}))
            .collect(),
        problems,
        ..Outcome::default()
    };
    for problem in &outcome.problems {
        eprintln!("tilo-extension: extension authority not taken: {problem}");
    }
    let desired = desired_dir(layout);
    for name in desired_names(layout) {
        match verify_entry(&desired.join(&name), &name, &authorities, owner) {
            Ok((payload, record)) => {
                let target = link(layout, &name);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).at(parent)?;
                }
                if let Err(error) = symlink(&payload, &target) {
                    let reason = format!("{} cannot be linked: {error}", target.display());
                    eprintln!("tilo-extension: signed layer {name} refused: {reason}");
                    outcome
                        .refused
                        .push(json!({"name": name, "reason": reason}));
                    continue;
                }
                outcome.linked.insert(name);
                outcome.merged.push(record);
            }
            Err(reason) => {
                eprintln!("tilo-extension: signed layer {name} refused: {reason}");
                outcome
                    .refused
                    .push(json!({"name": name, "reason": reason}));
            }
        }
    }
    write_status(layout, &outcome)?;
    Ok(outcome)
}

/// Take every layer of `outcome` out again: the merge was refused as a
/// whole (something else is in the search path).
///
/// # Errors
/// A link or the status cannot be written.
pub fn refuse_all(layout: &Layout, outcome: &mut Outcome, reason: &str) -> Result<()> {
    for name in std::mem::take(&mut outcome.linked) {
        remove_link(&link(layout, &name))?;
    }
    for record in std::mem::take(&mut outcome.merged) {
        let name = record.get("name").cloned().unwrap_or(Value::Null);
        outcome
            .refused
            .push(json!({"name": name, "reason": reason}));
    }
    write_status(layout, outcome)
}

fn write_status(layout: &Layout, outcome: &Outcome) -> Result<()> {
    let status = outcome.status();
    let empty = |key: &str| {
        status
            .get(key)
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    };
    let path = status_path(layout);
    if empty("merged") && empty("refused") && desired_names(layout).is_empty() {
        // Nothing desired and nothing linked: no record to keep.
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(crate::Error::os(&error, Some(&path)))
            }
            _ => Ok(()),
        };
    }
    write_json(&path, &status, 0o644)
}

/// The status a caller reads after an activation.
pub fn read_status(layout: &Layout) -> Map<String, Value> {
    fs::read(status_path(layout))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}
