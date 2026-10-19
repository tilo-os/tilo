//! The machine-wide layer set as signed layers.
//!
//! tilo-vmd merges nothing into `/usr` itself. It names the `vm` and
//! `device` slots' layers, each with its endorsement, as the desired signed
//! layers ([`stage`]), and the system extension activation
//! (`tilo-extension-activate.service`, the production verifier) checks each
//! signature and tree before it merges them, at boot and on every change
//! ([`check_activation`] reads what it did).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tilo_layer_trust::{MANIFEST_FILE, PAYLOAD_LINK, SIGNATURE_FILE};
use tilo_vm_protocol::Provenance;
use tilo_vm_protocol::layer_hash::PROVENANCE;

use crate::endorse::{Endorsements, create_public_dir, write_public};
use crate::error::DaemonError;
use crate::fsutil::symlink;

fn name_of(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn component_of(layer: &Path) -> String {
    fs::read(layer.join(PROVENANCE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Provenance>(&bytes).ok())
        .map_or_else(|| "unknown".to_owned(), |p| p.component)
}

/// Remove the links an older tilo-vmd made in `/var/lib/extensions` for its
/// own merge: the activation refuses to merge anything while they are there.
/// Never anything another tool put there. `true` when one was removed.
///
/// # Errors
/// A link that cannot be removed.
pub fn remove_legacy_links(dir: &Path, layers_dir: &Path) -> Result<bool, DaemonError> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(false);
    };
    let mut removed = false;
    for entry in entries.flatten() {
        let link = entry.path();
        if fs::read_link(&link).is_ok_and(|target| target.starts_with(layers_dir)) {
            fs::remove_file(&link)
                .map_err(DaemonError::io(format!("unlink {}", link.display())))?;
            removed = true;
        }
    }
    Ok(removed)
}

/// Whether the activation unit at `unit` (the image's) merges signed
/// layers: it starts for a desired one. An image from before them has no
/// such condition, and its activation ignores the desired set.
pub fn activation_takes_signed_layers(unit: &Path) -> bool {
    fs::read_to_string(unit).is_ok_and(|text| {
        text.lines()
            .any(|line| line.trim() == "ConditionDirectoryNotEmpty=|/var/lib/tilo/signed-layers")
    })
}

/// Whether `entry` already names `layer` with exactly these files.
fn current(entry: &Path, layer: &Path, manifest: &[u8], signature: &[u8]) -> bool {
    fs::read_link(entry.join(PAYLOAD_LINK)).is_ok_and(|target| target == layer)
        && fs::read(entry.join(MANIFEST_FILE)).is_ok_and(|bytes| bytes == manifest)
        && fs::read(entry.join(SIGNATURE_FILE)).is_ok_and(|bytes| bytes == signature)
}

/// Make `layers` exactly the desired signed layers in `desired`, each entry
/// holding its endorsement and a link to the layer. Entries of layers no
/// longer named are removed (only tilo-vmd's: their payload is in
/// `layers_dir`). `true` when anything changed.
///
/// Every layer is checked for an endorsement before anything is written, so
/// a refusal leaves the desired set as it was.
///
/// # Errors
/// [`DaemonError::Unendorsed`] for a layer without a kept signature, and I/O
/// failures.
pub fn stage(
    desired: &Path,
    layers_dir: &Path,
    endorsements: &Endorsements,
    layers: &[PathBuf],
) -> Result<bool, DaemonError> {
    let mut wanted = Vec::new();
    for layer in layers {
        let Some(name) = name_of(layer) else {
            continue;
        };
        let (Ok(manifest), Ok(signature)) = (
            fs::read(endorsements.manifest_path(&name)),
            fs::read(endorsements.signature_path(&name)),
        ) else {
            return Err(DaemonError::Unendorsed {
                component: component_of(layer),
                layer: name,
            });
        };
        wanted.push((name, layer, manifest, signature));
    }
    create_public_dir(desired)?;
    let names: BTreeSet<&str> = wanted.iter().map(|(name, ..)| name.as_str()).collect();
    let mut changed = false;
    let entries =
        fs::read_dir(desired).map_err(DaemonError::io(format!("read {}", desired.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = fs::read_link(path.join(PAYLOAD_LINK))
            .is_ok_and(|target| target.starts_with(layers_dir));
        let name = entry.file_name().to_string_lossy().into_owned();
        if ours && !names.contains(name.as_str()) {
            fs::remove_dir_all(&path)
                .map_err(DaemonError::io(format!("remove {}", path.display())))?;
            changed = true;
        }
    }
    for (name, layer, manifest, signature) in wanted {
        let entry = desired.join(&name);
        if current(&entry, layer, &manifest, &signature) {
            continue;
        }
        create_public_dir(&entry)?;
        write_public(&entry.join(MANIFEST_FILE), &manifest)?;
        write_public(&entry.join(SIGNATURE_FILE), &signature)?;
        let link = entry.join(PAYLOAD_LINK);
        match fs::remove_file(&link) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(DaemonError::io(format!("unlink {}", link.display()))(error));
            }
            _ => {}
        }
        symlink(layer, &link).map_err(DaemonError::io(format!("link {}", link.display())))?;
        changed = true;
    }
    Ok(changed)
}

/// What the activation did with `layers`: `Ok` when it merged every one.
/// `status` is `/run/tilo/signed-layers.json` after it ran (`None`: no
/// record), `ran` how the activation unit finished.
///
/// # Errors
/// [`DaemonError::LayerRefused`] for the first layer it refused; an
/// [`DaemonError::Apply`] when it merged a layer neither way, which is an
/// activation that does not know signed layers (an image from before them).
pub fn check_activation(
    layers: &[PathBuf],
    status: Option<&[u8]>,
    ran: Result<(), DaemonError>,
) -> Result<(), DaemonError> {
    let status: Value = status
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or(Value::Null);
    let listed = |key: &str| -> Vec<(String, String)> {
        status
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|layer| {
                let name = layer.get("name")?.as_str()?.to_owned();
                let reason = layer
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some((name, reason))
            })
            .collect()
    };
    let merged: BTreeSet<String> = listed("merged").into_iter().map(|(name, _)| name).collect();
    let refused = listed("refused");
    for layer in layers {
        let Some(name) = name_of(layer) else {
            continue;
        };
        if let Some((_, reason)) = refused.iter().find(|(refused, _)| *refused == name) {
            return Err(DaemonError::LayerRefused {
                layer: name,
                reason: reason.clone(),
            });
        }
        if !merged.contains(&name) {
            ran?;
            return Err(DaemonError::Apply {
                step: "tilo-extension activate".into(),
                detail: format!(
                    "layer {name} was neither merged nor refused: this image's activation does not merge signed layers; move the machine to a current dev image (tilo-vm vm upgrade)"
                ),
            });
        }
    }
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::LayerStore;
    use tilo_layer_trust::SecretKey;

    fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn staging_names_exactly_the_endorsed_layers_and_drops_the_raw_merge() {
        let root = tempfile::tempdir().unwrap();
        let store = LayerStore::new(root.path());
        let endorsements = Endorsements::new(root.path());
        let import = |content: &[u8]| {
            store
                .import_tar(
                    tarball(&[("usr/bin/shell", content)]).as_slice(),
                    &Provenance::local("shell"),
                    &mut |_| Ok(()),
                )
                .unwrap()
                .0
        };
        let (a, b) = (import(b"a"), import(b"b"));
        let desired = root.path().join("signed-layers");
        let layers_dir = root.path().join("layers");

        // Not endorsed: nothing is written.
        let error = stage(&desired, &layers_dir, &endorsements, &[store.path(&a)]).unwrap_err();
        assert!(
            matches!(error, DaemonError::Unendorsed { ref component, .. } if component == "shell")
        );
        assert!(!desired.join(&a).exists());

        let dev = SecretKey::from_seed(&[1; 32]);
        let authorities = vec![tilo_layer_trust::Authority {
            name: "dev".into(),
            key: dev.public(),
        }];
        for hash in [&a, &b] {
            let text = endorsements.prepare(&store, hash, 1).unwrap();
            endorsements
                .accept(hash, &dev.sign(text.as_bytes()), &authorities)
                .unwrap();
        }
        assert!(stage(&desired, &layers_dir, &endorsements, &[store.path(&a)]).unwrap());
        assert_eq!(
            fs::read_link(desired.join(&a).join(PAYLOAD_LINK)).unwrap(),
            store.path(&a)
        );
        assert!(desired.join(&a).join(SIGNATURE_FILE).is_file());
        assert!(
            !stage(&desired, &layers_dir, &endorsements, &[store.path(&a)]).unwrap(),
            "unchanged"
        );

        // Another tool's entry stays; ours of a layer no longer named goes.
        fs::create_dir_all(desired.join("other")).unwrap();
        assert!(stage(&desired, &layers_dir, &endorsements, &[store.path(&b)]).unwrap());
        assert!(!desired.join(&a).exists());
        assert!(desired.join(&b).is_dir());
        assert!(desired.join("other").is_dir());

        // The raw merge's links in /var/lib/extensions go; nobody else's.
        let extensions = root.path().join("extensions");
        fs::create_dir_all(&extensions).unwrap();
        symlink(&store.path(&a), &extensions.join(&a)).unwrap();
        symlink(Path::new("/elsewhere"), &extensions.join("foreign")).unwrap();
        assert!(remove_legacy_links(&extensions, &layers_dir).unwrap());
        assert!(fs::symlink_metadata(extensions.join(&a)).is_err());
        assert!(fs::symlink_metadata(extensions.join("foreign")).is_ok());
        assert!(!remove_legacy_links(&extensions, &layers_dir).unwrap());
    }

    #[test]
    fn the_shipped_activation_unit_takes_signed_layers() {
        let unit = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/lib/systemd/system/tilo-extension-activate.service");
        assert!(activation_takes_signed_layers(&unit));
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("tilo-extension-activate.service");
        fs::write(
            &older,
            "[Unit]\nConditionDirectoryNotEmpty=|/var/lib/extensions\n",
        )
        .unwrap();
        assert!(!activation_takes_signed_layers(&older));
        assert!(!activation_takes_signed_layers(&dir.path().join("missing")));
    }

    #[test]
    fn the_activation_must_merge_every_layer_or_say_why_not() {
        let layers = [PathBuf::from("/l/aaa"), PathBuf::from("/l/bbb")];
        let merged = br#"{"merged":[{"name":"aaa"},{"name":"bbb"}],"refused":[]}"#;
        check_activation(&layers, Some(merged), Ok(())).unwrap();

        let refused = br#"{"merged":[{"name":"aaa"}],"refused":[{"name":"bbb","reason":"usr/bin/shell has other bytes than the signed ones"}]}"#;
        let failed = Err(DaemonError::Apply {
            step: "systemctl start tilo-extension-activate.service".into(),
            detail: "exit 1".into(),
        });
        match check_activation(&layers, Some(refused), failed).unwrap_err() {
            DaemonError::LayerRefused { layer, reason } => {
                assert_eq!(layer, "bbb");
                assert!(reason.contains("other bytes"));
            }
            other => panic!("{other}"),
        }

        // An activation from before signed layers: skipped, no record.
        let error = check_activation(&layers, None, Ok(())).unwrap_err();
        assert!(
            error.to_string().contains("neither merged nor refused"),
            "{error}"
        );
    }
}
