//! Endorsements: the signed-layer manifests of the layers that merge into
//! the machine's `/usr` (the `vm` and `device` slots').
//!
//! tilo-vmd never holds a signing key. For a layer about to reach `/usr` it
//! seals the layer's files with fs-verity, reads them, and writes the
//! manifest that describes exactly those bytes ([`Endorsements::prepare`]);
//! tilo-vm on the build host checks the manifest describes the layer it
//! means (its content hash is the layer's name) and signs it with the
//! development key; tilo-vmd keeps the signature once it verifies against an
//! enrolled extension authority ([`Endorsements::accept`]). The system
//! extension activation checks all of it again at every merge.
//!
//! `<root>/endorsements/<hash>.json` and `<hash>.json.sig`.

use std::fs;
use std::path::{Path, PathBuf};

use tilo_layer_trust::keys::{is_authority_name, signed_by};
use tilo_layer_trust::manifest::{content_sha256, tilo_vm_extension_release};
use tilo_layer_trust::{Authority, LayerManifest, MANIFEST_TYPE, PublicKey, SCHEMA, describe};
use tilo_vm_protocol::layer_hash::RELEASE_DIR;
use tilo_vm_protocol::{AuthorityInfo, Provenance};

use crate::error::DaemonError;
use crate::layers::LayerStore;

/// Write a file anyone may read and only its owner write, as the activation
/// requires of everything it trusts.
pub fn write_public(path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    tilo_files::native::atomic::write_creating_parent_with_mode(path, bytes, 0o644)
        .map_err(DaemonError::io(format!("write {}", path.display())))
}

/// Create `dir` (and its parents) and give it mode 0755: nobody but its
/// owner may add to what the activation trusts.
pub fn create_public_dir(dir: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::create_dir_all(dir).map_err(DaemonError::io(format!("create {}", dir.display())))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
        .map_err(DaemonError::io(format!("chmod {}", dir.display())))
}

pub struct Endorsements {
    dir: PathBuf,
}

pub fn info(authority: &Authority) -> AuthorityInfo {
    AuthorityInfo {
        name: authority.name.clone(),
        fingerprint: authority.key.fingerprint(),
    }
}

/// The manifest's `revision`: what the layer was built from.
fn revision(provenance: &Provenance) -> String {
    match (&provenance.source_commit, &provenance.dirty_tree_hash) {
        (Some(commit), Some(dirty)) => format!("{commit}+dirty:{dirty}"),
        (Some(commit), None) => commit.clone(),
        (None, _) => provenance.authority.clone(),
    }
}

/// A provenance's base digest when it is one a manifest can carry.
fn base_digest(provenance: &Provenance) -> Option<String> {
    provenance.base_image_digest.clone().filter(|digest| {
        digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| tilo_layer_trust::is_hex(hex, 64))
    })
}

impl Endorsements {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("endorsements"),
        }
    }

    pub fn manifest_path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }

    pub fn signature_path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json.sig"))
    }

    /// The enrolled authority whose signature over the kept manifest of
    /// `hash` verifies, if any.
    pub fn signer<'a>(&self, hash: &str, authorities: &'a [Authority]) -> Option<&'a Authority> {
        let manifest = fs::read(self.manifest_path(hash)).ok()?;
        let signature = fs::read_to_string(self.signature_path(hash)).ok()?;
        signed_by(authorities, &manifest, &signature)
    }

    /// Seal the layer `hash` and write the manifest that describes it, for
    /// slot generation `generation`; an older signature is dropped. Returns
    /// the manifest's exact text, which is what gets signed.
    ///
    /// # Errors
    /// An unknown layer, a layer whose files no longer hash to its name or
    /// whose extension-release is not the importer's, and I/O failures.
    pub fn prepare(
        &self,
        layers: &LayerStore,
        hash: &str,
        generation: u64,
    ) -> Result<String, DaemonError> {
        let path = layers.require(hash)?;
        let provenance = layers.provenance(hash).ok_or_else(|| {
            DaemonError::InvalidLayer(format!("cached layer {hash} has no valid provenance"))
        })?;
        let entries = describe(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                DaemonError::InvalidLayer(error.to_string())
            } else {
                DaemonError::io(format!("seal layer {hash}"))(error)
            }
        })?;
        let damaged = || DaemonError::InvalidLayer(format!("cached layer {hash} is damaged"));
        if content_sha256(&entries) != hash {
            return Err(damaged());
        }
        let release = tilo_vm_extension_release(&provenance.component);
        let written = fs::read(
            path.join(RELEASE_DIR)
                .join(format!("extension-release.{hash}")),
        )
        .map_err(DaemonError::io(format!(
            "read layer {hash}'s extension-release"
        )))?;
        if written != release.as_bytes() {
            return Err(damaged());
        }
        let manifest = LayerManifest {
            kind: MANIFEST_TYPE.into(),
            schema: SCHEMA,
            name: hash.to_owned(),
            component: provenance.component.clone(),
            revision: revision(&provenance),
            content_sha256: hash.to_owned(),
            generation,
            base_image_digest: base_digest(&provenance),
            extension_release: release,
            entries,
        };
        manifest
            .check()
            .map_err(|error| DaemonError::InvalidLayer(format!("layer {hash}: {error}")))?;
        let bytes = manifest
            .to_bytes()
            .map_err(|error| DaemonError::InvalidLayer(error.to_string()))?;
        match fs::remove_file(self.signature_path(hash)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(DaemonError::io(format!(
                    "drop layer {hash}'s old signature"
                ))(error));
            }
            _ => {}
        }
        create_public_dir(&self.dir)?;
        write_public(&self.manifest_path(hash), &bytes)?;
        String::from_utf8(bytes).map_err(|error| DaemonError::InvalidLayer(error.to_string()))
    }

    /// Keep `signature` for the manifest of `hash` once it verifies against
    /// one of `authorities`; that authority.
    ///
    /// # Errors
    /// No manifest was prepared for the layer, or no enrolled authority made
    /// the signature.
    pub fn accept(
        &self,
        hash: &str,
        signature: &str,
        authorities: &[Authority],
    ) -> Result<Authority, DaemonError> {
        let manifest = fs::read(self.manifest_path(hash)).map_err(|_| {
            DaemonError::Authority(format!(
                "no manifest was prepared for layer {hash}; ask for the pending endorsements first"
            ))
        })?;
        let Some(authority) = signed_by(authorities, &manifest, signature) else {
            let enrolled: Vec<String> = authorities
                .iter()
                .map(|a| format!("{} {}", a.name, a.key.fingerprint()))
                .collect();
            return Err(DaemonError::Authority(if enrolled.is_empty() {
                "this machine has enrolled no extension authority, so it trusts no signature".into()
            } else {
                format!(
                    "the signature over layer {hash} is not by an enrolled authority (enrolled: {})",
                    enrolled.join(", ")
                )
            }));
        };
        write_public(&self.signature_path(hash), signature.as_bytes())?;
        Ok(authority.clone())
    }
}

/// Enroll `pem` as the authority `name` in `dir`. `Ok(false)` when exactly
/// this key already is that authority.
///
/// # Errors
/// A name that cannot be an authority's, a key that is not an Ed25519
/// public key, and I/O failures.
pub fn enroll(dir: &Path, name: &str, pem: &str) -> Result<(PublicKey, bool), DaemonError> {
    if !is_authority_name(name) {
        return Err(DaemonError::Authority(format!(
            "{name:?} cannot name an authority: 1-32 of a-z, 0-9 and -"
        )));
    }
    let key =
        PublicKey::from_pem(pem).map_err(|error| DaemonError::Authority(error.to_string()))?;
    let path = dir.join(format!("{name}.pub"));
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|text| PublicKey::from_pem(&text).ok());
    if current.as_ref() == Some(&key) {
        return Ok((key, false));
    }
    create_public_dir(dir)?;
    write_public(&path, key.to_pem().as_bytes())?;
    Ok((key, true))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn a_prepared_manifest_names_the_layer_and_takes_only_an_enrolled_signature() {
        let root = tempfile::tempdir().unwrap();
        let store = LayerStore::new(root.path());
        let mut provenance = Provenance::local("shell");
        provenance.source_commit = Some("1a2b3c4".into());
        let (hash, _) = store
            .import_tar(
                tarball(&[
                    ("usr/bin/shell", b"v1"),
                    ("usr/share/tilo/shell.toml", b"k"),
                ])
                .as_slice(),
                &provenance,
                &mut |_| Ok(()),
            )
            .unwrap();
        let endorsements = Endorsements::new(root.path());
        let text = endorsements.prepare(&store, &hash, 4).unwrap();
        let manifest = LayerManifest::parse(text.as_bytes()).unwrap();
        // The signer's check: the manifest's content hash is tilo-vm's layer
        // identity, computed by the protocol crate from the tree itself.
        assert_eq!(manifest.content_sha256, hash);
        assert_eq!(
            tilo_vm_protocol::layer_hash::tree_hash(&store.path(&hash)).unwrap(),
            hash
        );
        assert_eq!(manifest.name, hash);
        assert_eq!(manifest.component, "shell");
        assert_eq!(manifest.revision, "1a2b3c4");
        assert_eq!(manifest.generation, 4);
        // The activation's own check passes on the store's layer.
        let owner = std::os::unix::fs::MetadataExt::uid(&fs::metadata(root.path()).unwrap());
        tilo_layer_trust::verify_payload(&store.path(&hash), &manifest, owner).unwrap();

        let dev = SecretKey::from_seed(&[5; 32]);
        let stranger = SecretKey::from_seed(&[6; 32]);
        let authorities = vec![Authority {
            name: "dev".into(),
            key: dev.public(),
        }];
        assert!(endorsements.signer(&hash, &authorities).is_none());
        let refused = endorsements
            .accept(&hash, &stranger.sign(text.as_bytes()), &authorities)
            .unwrap_err();
        assert!(matches!(refused, DaemonError::Authority(_)), "{refused}");
        assert!(
            endorsements
                .accept(&hash, &dev.sign(text.as_bytes()), &[])
                .is_err()
        );
        let authority = endorsements
            .accept(&hash, &dev.sign(text.as_bytes()), &authorities)
            .unwrap();
        assert_eq!(authority.name, "dev");
        assert_eq!(
            endorsements.signer(&hash, &authorities).unwrap().name,
            "dev"
        );

        // Preparing again (a new generation) drops the old signature.
        endorsements.prepare(&store, &hash, 5).unwrap();
        assert!(endorsements.signer(&hash, &authorities).is_none());
    }

    #[test]
    fn a_layer_changed_in_the_store_is_not_described() {
        let root = tempfile::tempdir().unwrap();
        let store = LayerStore::new(root.path());
        let (hash, _) = store
            .import_tar(
                tarball(&[("usr/bin/shell", b"v1")]).as_slice(),
                &Provenance::local("shell"),
                &mut |_| Ok(()),
            )
            .unwrap();
        let binary = store.path(&hash).join("usr/bin/shell");
        fs::remove_file(&binary).unwrap();
        fs::write(&binary, b"changed").unwrap();
        let error = Endorsements::new(root.path())
            .prepare(&store, &hash, 1)
            .unwrap_err();
        assert!(error.to_string().contains("damaged"), "{error}");
    }

    #[test]
    fn enrolling_writes_the_key_once() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::from_seed(&[9; 32]).public();
        let (enrolled, changed) = enroll(dir.path(), "dev", &key.to_pem()).unwrap();
        assert!(changed);
        assert_eq!(enrolled, key);
        let (_, again) = enroll(dir.path(), "dev", &key.to_pem()).unwrap();
        assert!(!again);
        let owner = std::os::unix::fs::MetadataExt::uid(&fs::metadata(dir.path()).unwrap());
        let (authorities, problems) = tilo_layer_trust::load_authorities(dir.path(), owner);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(authorities[0].key, key);
        assert!(enroll(dir.path(), "../x", &key.to_pem()).is_err());
        assert!(enroll(dir.path(), "dev", "not a key").is_err());
    }
}
