//! The development trust key: tilo-vm's layers reach a machine's `/usr`
//! only as signed layers, verified by the same system extension activation
//! that merges a downloaded extension.
//!
//! The key lives on the build host only, beside the registry
//! (`keys/dev-trust.key`, an Ed25519 key in PKCS#8 PEM, mode 0600, and its
//! public half `keys/dev-trust.pub`). A machine trusts it once its owner
//! enrolled the public half as the extension authority `dev`
//! (`tilo-vm trust enroll`, which writes
//! `/etc/tilo/extension-authorities.d/dev.pub`). A VM tilo-vm runs itself
//! enrolls it at its first swap; a real machine never does without being
//! asked.
//!
//! Before any request that changes the `vm` or `device` slot (a swap, a
//! userspace apply, a rollback, a slot restart), tilo-vm asks the daemon
//! which layers wait for a signature ([`changes_usr`]), checks that each
//! manifest names exactly the layer it was asked for and hashes to it, and
//! signs it ([`sign_pending`]).

use std::fs;
use std::path::{Path, PathBuf};

use tilo_layer_trust::{LayerManifest, SecretKey};
use tilo_vm_protocol::{DEVICE_SLOT, Endorsement, PendingEndorsement, Request, VM_SLOT};

use crate::error::CliError;

/// Where the development key lives: beside the registry, with the VMs' SSH
/// keys.
pub fn key_path(registry_path: &Path) -> PathBuf {
    registry_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("keys")
        .join("dev-trust.key")
}

pub fn public_path(key: &Path) -> PathBuf {
    key.with_extension("pub")
}

/// The key at `path`, if there is one.
///
/// # Errors
/// A key file that cannot be read or is not an Ed25519 private key.
pub fn load(path: &Path) -> Result<Option<SecretKey>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => SecretKey::from_pem(&text)
            .map(Some)
            .map_err(|error| CliError::Trust(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CliError::io(format!("read {}", path.display()))(error)),
    }
}

/// The key at `path`, made on first use; `true` when it was just made.
///
/// # Errors
/// As [`load`], no randomness, or the key cannot be written.
pub fn load_or_create(path: &Path) -> Result<(SecretKey, bool), CliError> {
    if let Some(key) = load(path)? {
        return Ok((key, false));
    }
    let key = SecretKey::generate().map_err(|error| CliError::Trust(error.to_string()))?;
    tilo_files::native::atomic::write_creating_parent_with_mode(path, key.to_pem().as_bytes(), 0o600)
        .map_err(CliError::io(format!("write {}", path.display())))?;
    let public = public_path(path);
    tilo_files::native::atomic::write_creating_parent(&public, key.public().to_pem().as_bytes())
        .map_err(CliError::io(format!("write {}", public.display())))?;
    Ok((key, true))
}

/// The slot a request changes and the layers it brings, when the slot is
/// one whose layers merge into the machine's `/usr`.
pub fn changes_usr(request: &Request) -> Option<(String, Vec<String>)> {
    let (slot, layers) = match request {
        Request::Swap(swap) => (swap.slot.clone(), vec![swap.layer.clone()]),
        Request::UserspaceApply(apply) => (
            apply.slot.clone(),
            apply.members.iter().map(|m| m.layer.clone()).collect(),
        ),
        Request::Rollback(rollback) => (rollback.slot.clone(), Vec::new()),
        Request::SlotRestart(restart) => (restart.name.clone(), Vec::new()),
        _ => return None,
    };
    (slot == VM_SLOT || slot == DEVICE_SLOT).then_some((slot, layers))
}

/// Sign each pending manifest, after checking it describes exactly the
/// layer it is for: named after it, and its entries hash to it (tilo-vm's
/// layer identity, which the builder computed from its own bytes).
///
/// # Errors
/// A manifest that is malformed or describes another layer.
pub fn sign_pending(
    key: &SecretKey,
    pending: &[PendingEndorsement],
) -> Result<Vec<Endorsement>, CliError> {
    pending
        .iter()
        .map(|pending| {
            let manifest = LayerManifest::parse(pending.manifest.as_bytes()).map_err(|error| {
                CliError::Trust(format!("the manifest for layer {}: {error}", pending.layer))
            })?;
            if manifest.name != pending.layer || manifest.content_sha256 != pending.layer {
                return Err(CliError::Trust(format!(
                    "the manifest for layer {} describes layer {}",
                    pending.layer, manifest.content_sha256
                )));
            }
            Ok(Endorsement {
                layer: pending.layer.clone(),
                signature: key.sign(pending.manifest.as_bytes()),
            })
        })
        .collect()
}

/// How a remote session signs layers for the machine's `/usr`.
#[derive(Debug, Clone)]
pub struct Trust {
    pub key: PathBuf,
    /// The machine's name, for messages.
    pub vm: String,
    /// A VM tilo-vm runs itself takes this host's key at its first swap; a
    /// real machine only through `tilo-vm trust enroll`.
    pub auto_enroll: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_layer_trust::manifest::{content_sha256, tilo_vm_extension_release};
    use tilo_layer_trust::{Entry, MANIFEST_TYPE, SCHEMA};

    fn manifest_for(entries: Vec<Entry>) -> (String, String) {
        let hash = content_sha256(&entries);
        let manifest = LayerManifest {
            kind: MANIFEST_TYPE.into(),
            schema: SCHEMA,
            name: hash.clone(),
            component: "shell".into(),
            revision: "local".into(),
            content_sha256: hash.clone(),
            generation: 1,
            base_image_digest: None,
            extension_release: tilo_vm_extension_release("shell"),
            entries,
        };
        (hash, String::from_utf8(manifest.to_bytes().unwrap()).unwrap())
    }

    #[test]
    fn only_a_manifest_of_the_asked_layer_is_signed() {
        let key = SecretKey::from_seed(&[1; 32]);
        let entries = vec![Entry::File {
            path: "usr/bin/shell".into(),
            sha256: "a".repeat(64),
            verity: None,
            executable: true,
        }];
        let (hash, manifest) = manifest_for(entries);
        let signed = sign_pending(
            &key,
            &[PendingEndorsement {
                layer: hash.clone(),
                manifest: manifest.clone(),
            }],
        )
        .unwrap();
        assert!(key.public().verifies(manifest.as_bytes(), &signed[0].signature));

        // A daemon describing another layer than the one asked for.
        let error = sign_pending(
            &key,
            &[PendingEndorsement {
                layer: "b".repeat(64),
                manifest,
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("describes layer"), "{error}");
        assert!(
            sign_pending(
                &key,
                &[PendingEndorsement {
                    layer: hash,
                    manifest: "{}".into(),
                }],
            )
            .is_err()
        );
    }

    #[test]
    fn only_the_vm_and_device_slots_change_usr() {
        let swap = |slot: &str| {
            Request::Swap(tilo_vm_protocol::Swap {
                slot: slot.into(),
                component: "shell".into(),
                layer: "a".repeat(64),
                info: None,
            })
        };
        assert_eq!(
            changes_usr(&swap("device")),
            Some(("device".into(), vec!["a".repeat(64)]))
        );
        assert!(changes_usr(&swap("vm")).is_some());
        assert!(changes_usr(&swap("review-a")).is_none());
        let rollback = Request::Rollback(tilo_vm_protocol::Rollback {
            slot: "vm".into(),
            to: None,
        });
        assert_eq!(changes_usr(&rollback), Some(("vm".into(), Vec::new())));
    }

    #[cfg(unix)]
    #[test]
    fn the_key_is_made_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(&dir.path().join("vms.toml"));
        assert!(load(&path).unwrap().is_none());
        let (key, made) = load_or_create(&path).unwrap();
        assert!(made);
        let (again, made_again) = load_or_create(&path).unwrap();
        assert!(!made_again);
        assert_eq!(again.public(), key.public());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let public = fs::read_to_string(public_path(&path)).unwrap();
        assert_eq!(public, key.public().to_pem());
    }
}
