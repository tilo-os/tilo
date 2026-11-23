//! The person's root, held by this device, for host retirement.

use anyhow::Context;
use tilo_user_identity::StatementSigner;

/// Sign a revocation of `device` with `root` and append it to the server
/// state's revocation log at `state_root`, where every start of the host reads
/// it. Returns the entry as signed, and whether the log changed.
///
/// # Errors
///
/// Returns an error if the entry cannot be signed, or the log cannot be read
/// or replaced.
pub fn revoke_device(
    state_root: &std::path::Path,
    root: &dyn StatementSigner,
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
    let appended = crate::workspace_files::append_revocation(state_root, &entry)
        .with_context(|| format!("append to {}", state_root.display()))?;
    Ok((entry, appended))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    tilo_test_env::scenario! {
        fn a_host_record_is_signed_only_by_a_usable_root;

        phase the_root_in_the_identity_store_revokes_a_device;
        // The person's root is kept only in their identity store; a
        // revocation it signs reaches the log every server start reads.
        {
            let temp = tempfile::tempdir().unwrap();
            let identities = temp.path().join("identities");
            let person = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
            tilo_identity::provisioning::import_single_device_root(
                &identities,
                &person,
                "",
                tilo_identity::provisioning::IdentityStorage::Files,
            )
            .unwrap();
            let root = tilo_identity::store::load_person_root(&identities).unwrap();
            assert_eq!(root.public_key(), person.verifying_key().to_bytes());
            let state = temp.path().join("state");
            std::fs::create_dir_all(&state).unwrap();
            let (entry, appended) = revoke_device(
                &state,
                &root,
                [62; 32],
                tilo_user_identity::RevocationReason::Lost,
            )
            .unwrap();
            assert!(appended);
            tilo_user_identity::verify_revocation_entry(&entry).unwrap();
            assert_eq!(entry.user_public_key, person.verifying_key().to_bytes());
            assert_eq!(
                crate::workspace_files::load_revocations(&state).unwrap(),
                vec![entry]
            );
        }

        phase host_record_unavailable_provider_does_not_create_a_root_file;
        {
            let temp = tempfile::tempdir().unwrap();
            let identities = temp.path().join("identities");
            let paths = tilo_device_identity::IdentityPaths::under(temp.path().join("tilo"));
            tilo_identity::provisioning::ensure_collaboration_identity(
                &identities,
                &paths,
                tilo_identity::provisioning::IdentityStorage::Files,
            )
            .unwrap();
            let record = tilo_identity::store::resolve_active_identity(&identities).unwrap();
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
            metadata["secret_ref"]["backend"] = "fixture-unavailable-provider".into();
            std::fs::write(&record, serde_json::to_vec(&metadata).unwrap()).unwrap();
            assert!(tilo_identity::store::load_person_root(&identities).is_err());
        }

        phase host_record_public_projection_cannot_sign;
        {
            let temp = tempfile::tempdir().unwrap();
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
                tilo_identity::store::resolve_active_identity(&identities),
                Some(record)
            );
            assert!(tilo_identity::store::load_person_root(&identities).is_err());
        }
    }
}
