//! The person's root, held by this device, for enrollment and host retirement.

use anyhow::{Context, bail};
use tilo_user_identity::{IdentityPaths, StatementSigner};

/// The person's root held here, on top of the identity crate's signer: every
/// statement this crate issues is signed through [`StatementSigner`], and the
/// pairing handshake's answer, which is domain-separated bytes rather than a
/// statement's digest, is signed by [`RootSigner::sign_message`].
pub trait RootSigner: StatementSigner {
    /// Ed25519-sign `message` as the person's root, without exporting it.
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]>;
}

impl RootSigner for ed25519_dalek::SigningKey {
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
        use ed25519_dalek::Signer as _;
        Ok(self.sign(message).to_bytes())
    }
}

impl RootSigner for runtime_identity::SigningIdentity {
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
        self.sign(message)
            .map_err(|error| anyhow::anyhow!("sign with the identity record: {error}"))
    }
}

/// Open the root signer, falling back to an identity record only for an absent seed.
pub fn load_root(
    identity_root: &std::path::Path,
    identity_dir: Option<&std::path::Path>,
) -> anyhow::Result<Box<dyn RootSigner>> {
    const MISSING_ROOT: &str =
        "this device does not hold the identity root needed to approve enrollment";
    let paths = IdentityPaths::under(identity_root);
    let seed_absent = matches!(
        std::fs::symlink_metadata(&paths.user_privkey),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    let Some(identity_dir) = identity_dir.filter(|_| seed_absent) else {
        let root = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)
            .context(MISSING_ROOT)?;
        return Ok(Box::new(root));
    };
    let record = runtime_identity::resolve_active_identity(identity_dir).with_context(|| {
        format!(
            "{MISSING_ROOT}: {} holds no active identity",
            identity_dir.display()
        )
    })?;
    let identity = runtime_identity::load_from_path(record.clone()).map_err(|error| {
        anyhow::anyhow!(
            "{MISSING_ROOT}: cannot open {}: {error} (run this as the account that owns the \
             identity, in a session that can reach its secret store)",
            record.display()
        )
    })?;
    if !identity.is_single_device() {
        bail!(
            "{}",
            tilo_identity_host::certification_ceremony::cannot_sign_alone()
        );
    }
    Ok(Box::new(identity))
}

/// Sign a revocation of `device` with `root` and append it to the relay
/// state's revocation log at `state_root`, where every start of the host reads
/// it. Returns the entry as signed, and whether the log changed.
///
/// # Errors
///
/// Returns an error if the entry cannot be signed, or the log cannot be read
/// or replaced.
pub fn revoke_device(
    state_root: &std::path::Path,
    root: &dyn RootSigner,
    device: [u8; 32],
    reason: tilo_user_identity::RevocationReason,
) -> anyhow::Result<(tilo_user_identity::RevocationEntry, bool)> {
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.public_key(),
        revoked_device_public_key: device,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).context("sign the revocation")?;
    let appended = crate::store::append_revocation(state_root, &entry)
        .with_context(|| format!("append to {}", state_root.display()))?;
    Ok((entry, appended))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn host_record_does_not_bypass_an_unusable_root_file() {
        let temp = tempfile::tempdir().unwrap();
        let identities = temp.path().join("identities");
        let root = temp.path().join("tilo");
        let paths = IdentityPaths::under(&root);
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identities,
            &paths,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        assert!(load_root(&root, Some(&identities)).is_ok());
        std::fs::write(&paths.user_privkey, b"not a signing key").unwrap();
        assert!(load_root(&root, Some(&identities)).is_err());
    }

    #[test]
    fn host_record_unavailable_provider_does_not_create_a_root_file() {
        let temp = tempfile::tempdir().unwrap();
        let identities = temp.path().join("identities");
        let root = temp.path().join("tilo");
        let paths = IdentityPaths::under(&root);
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identities,
            &paths,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let record = runtime_identity::resolve_active_identity(&identities).unwrap();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        metadata["secret_ref"]["backend"] = "fixture-unavailable-provider".into();
        std::fs::write(&record, serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert!(load_root(&root, Some(&identities)).is_err());
        assert!(!paths.user_privkey.exists());
    }

    #[test]
    fn host_record_public_projection_cannot_sign() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tilo");
        let identities = temp.path().join("identities");
        std::fs::create_dir_all(&identities).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[75; 32])
            .verifying_key()
            .to_bytes();
        let record = identities.join("primary.json");
        let public = serde_json::json!({
            "schema_version": 2, "display_name": "Fixture", "avatar_color": [1, 2, 3],
            "created_at": "2026-10-08T00:00:00Z", "threshold_identity": {
                "single_device": false, "group_public_key": key,
                "threshold": 1, "total_shares": 1, "share_index": 1,
                "secret_bytes": [],
            },
        });
        std::fs::write(&record, serde_json::to_vec(&public).unwrap()).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            runtime_identity::resolve_active_identity(&identities),
            Some(record)
        );
        assert!(load_root(&root, Some(&identities)).is_err());
        assert!(!IdentityPaths::under(&root).user_privkey.exists());
    }
}
