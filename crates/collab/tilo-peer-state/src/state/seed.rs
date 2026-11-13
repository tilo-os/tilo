//! Seeding a workspace document from nothing: the governance rows a founder
//! installs, and the shared desktop every member's layout lands in.
use super::GovernedIdentity;
use super::err;
use super::{SeedDocument, SeedState};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use collab_types::Ed25519PublicKey;
use collab_types::caps::Principal;
use tilo_user_identity::DeviceCert;

/// A new workspace governed by `authority`: its governance
/// ([`seed_workspace`]), named `name`, with no windows and no app records.
///
/// # Errors
///
/// Returns an error if `workspace` is not a UUID or the document cannot be
/// built.
pub fn workspace_seed(
    workspace: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    name: &str,
) -> Result<SeedState, String> {
    use tilo_automerge::ApplyIncoming as _;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    let changes = seed_workspace(workspace, authority, roster)?
        .into_iter()
        .map(automerge::Change::from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    doc.apply_incoming(changes).map_err(err)?;
    doc.put(ROOT, "name", name).map_err(err)?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64).map_err(err)?;
    doc.commit();
    Ok(SeedState {
        workspace_changes: doc
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        // The workspace's shared desktop: every member's layout lands in it.
        documents: vec![SeedDocument {
            app_id: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            record_id: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
            kind: tilo_desktop_model::ownership::DESKTOP_KIND.into(),
            bytes: AutoCommit::new().save(),
        }],
    })
}

/// The same native workspace CRDT keys projected by `WorkspaceTree::sync_workspace_doc`.
///
/// # Errors
///
/// Returns an error if `workspace` is not a UUID or the workspace document cannot be built.
pub fn seed_workspace(
    workspace: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
) -> Result<Vec<Vec<u8>>, String> {
    uuid::Uuid::parse_str(workspace).map_err(err)?;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    doc.put(ROOT, "room", workspace).map_err(err)?;
    doc.put(ROOT, "name", "Shared browser workspace")
        .map_err(err)?;
    let owner = Principal::person_from_key(Ed25519PublicKey::from_bytes(authority.user_public_key));
    doc.put(ROOT, "owner", owner.id()).map_err(err)?;
    doc.put_object(ROOT, "invitation_receipts_v1", ObjType::Map)
        .map_err(err)?;
    let names = doc.put_object(ROOT, "peers", ObjType::Map).map_err(err)?;
    let identities = doc
        .put_object(ROOT, "peer_identities_v1", ObjType::Map)
        .map_err(err)?;
    for certificate in roster {
        let id = hex::encode(certificate.device_public_key);
        doc.put(&names, &id, certificate.device_label.as_str())
            .map_err(err)?;
        let identity = GovernedIdentity {
            device_cert: certificate.clone(),
            revocations: vec![],
            admission: None,
            admissions: vec![],
            delegate: None,
            workspace_delegations: Vec::new(),
        };
        doc.put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(identity.encode().map_err(err)?),
        )
        .map_err(err)?;
    }
    doc.commit();
    Ok(doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect())
}
