//! Seeding a workspace document from nothing: the governance rows a founder
//! installs, and the shared desktop every member's layout lands in.
use super::GovernedIdentity;
use super::err;
use super::{SeedDocument, SeedState, SystemTime};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use tilo_user_identity::DeviceCert;

/// The workspace document's key naming the device that created it
/// ([`crate::PeerGovernance::founder`]).
pub(crate) const FOUNDER: &str = "founder";

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
    now: SystemTime,
) -> Result<SeedState, String> {
    use tilo_automerge::ApplyIncoming as _;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    let changes = seed_workspace_at(workspace, authority, roster, super::now_secs(now))?
        .into_iter()
        .map(automerge::Change::from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    doc.apply_incoming(changes).map_err(err)?;
    doc.put(ROOT, "name", name).map_err(err)?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64).map_err(err)?;
    super::commit_governance(&mut doc, super::now_secs(now));
    Ok(SeedState {
        workspace_changes: doc
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        // The workspace's shared desktop: every member's layout lands in it.
        documents: vec![SeedDocument {
            app_id: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            document_id: tilo_desktop_model::ownership::DESKTOP_DOCUMENT.into(),
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
    seed_workspace_at(workspace, authority, roster, super::wall_secs())
}

/// [`seed_workspace`] at `unix_secs`, the time the workspace is made.
///
/// # Errors
///
/// Returns an error if `workspace` is not a UUID or the document cannot be built.
pub fn seed_workspace_at(
    workspace: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    unix_secs: i64,
) -> Result<Vec<Vec<u8>>, String> {
    uuid::Uuid::parse_str(workspace).map_err(err)?;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    founding_rows(
        &mut doc,
        workspace,
        &authority.user_public_key,
        &hex::encode(authority.device_public_key),
        roster,
    )?;
    super::commit_governance(&mut doc, unix_secs);
    Ok(changes_of(&mut doc))
}

/// The seed of a workspace that is a person's own, founded in their name: the
/// one history every one of their devices makes, before any of them is known to
/// any other.
///
/// The workspace's id follows from the person's root, so two of their devices
/// that found it before they see one another found the same workspace. The
/// history here is the same on both — one change under an actor derived from
/// the workspace and the person, with the person as its owner and its founder,
/// and no device in it — so the two foundings are one change, and the writes
/// each device makes afterwards merge into it rather than competing for its
/// root. Each device names itself among the workspace's holders with an
/// ordinary signed change ([`crate::PeerGovernanceMut::author_keeping_device`]).
///
/// # Errors
///
/// Returns an error if `workspace` is not a UUID or the document cannot be
/// built.
pub fn person_workspace_seed(
    workspace: &str,
    person: &[u8; 32],
    name: &str,
) -> Result<SeedState, String> {
    uuid::Uuid::parse_str(workspace).map_err(err)?;
    let mut doc = AutoCommit::new().with_actor(founding_actor(workspace, person));
    founding_rows(&mut doc, workspace, person, &hex::encode(person), &[])?;
    doc.put(ROOT, "name", name).map_err(err)?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64).map_err(err)?;
    doc.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    Ok(SeedState {
        workspace_changes: changes_of(&mut doc),
        // The workspace's shared desktop is no part of its founding history: a
        // layout every founding device wrote would be one document two devices
        // published at once. It starts with the first layout written to it.
        documents: Vec::new(),
    })
}

/// The Automerge actor a person-founded workspace's founding change is made
/// under: derived from the person and the workspace alone, so every device that
/// founds that workspace writes the same change and none writes a later one as
/// it.
#[must_use]
pub fn founding_actor(workspace: &str, person: &[u8; 32]) -> automerge::ActorId {
    let mut material = workspace.as_bytes().to_vec();
    material.extend_from_slice(person);
    automerge::ActorId::from(
        blake3::derive_key("tilo workspace founding actor v1", &material).to_vec(),
    )
}

/// The changes `doc` holds, encoded.
fn changes_of(doc: &mut AutoCommit) -> Vec<Vec<u8>> {
    doc.get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// The governance rows a founding installs in `doc`: the workspace it is, its
/// owner `person`, its `founder`, the maps every device and governor adds to,
/// and the `roster`'s certificates.
fn founding_rows(
    doc: &mut AutoCommit,
    workspace: &str,
    person: &[u8; 32],
    founder: &str,
    roster: &[DeviceCert],
) -> Result<(), String> {
    doc.put(ROOT, "workspace", workspace).map_err(err)?;
    doc.put(ROOT, "name", "Shared browser workspace")
        .map_err(err)?;
    doc.put(ROOT, "owner", hex::encode(person)).map_err(err)?;
    doc.put(ROOT, FOUNDER, founder).map_err(err)?;
    doc.put_object(ROOT, super::invitation_receipts::RECEIPTS_KEY, ObjType::Map)
        .map_err(err)?;
    // Maps of rows each device writes for itself exist from the founding, so
    // no device's first row makes a map another device's would replace.
    for rows in super::device_rows::FOUNDED {
        doc.put_object(ROOT, rows, ObjType::Map).map_err(err)?;
    }
    // So are the maps of governance rows each governor adds to, and of rows
    // each device keeps for itself in its person's name.
    for rows in [
        collab_types::workspace_grant::WORKSPACE_GRANTS_KEY,
        collab_types::work_delegation::WORK_DELEGATIONS_KEY,
        super::keeping_devices::WORKSPACE_KEEPING_DEVICES_KEY,
        super::app_versions::APP_VERSIONS,
        super::bot_members::PROGRAMS,
        super::bot_members::REQUESTS,
        tilo_desktop_model::projection::WORKSPACE_AUTOMATIONS_KEY,
        tilo_desktop_model::projection::WORKSPACE_PRESETS_KEY,
        tilo_desktop_model::projection::WORKSPACE_SANDBOX_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_HISTORY_KEY,
        tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY,
        super::invitation_receipts::PARTS_KEY,
        crate::document_import::WORKSPACE_KEY,
        crate::removals::FIELD_V1,
        crate::coverage::FIELD_V2,
        crate::durability::FIELD_V2,
    ] {
        doc.put_object(ROOT, rows, ObjType::Map).map_err(err)?;
    }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::person_workspace_seed;

    /// Two of a person's devices that found their workspace before they see
    /// one another write the same history, so the two are one change and merge
    /// into one workspace.
    #[test]
    fn a_workspace_founded_in_its_persons_name_is_the_same_history_on_every_device() {
        let workspace = "6f2f3d5a-8c1d-4e2b-9a7c-0d1e2f3a4b5c";
        let person = [7; 32];
        let first = person_workspace_seed(workspace, &person, "Servers").unwrap();
        let second = person_workspace_seed(workspace, &person, "Servers").unwrap();
        assert_eq!(
            first.workspace_changes, second.workspace_changes,
            "the founding is one history on both devices"
        );
        let other = person_workspace_seed(workspace, &[8; 32], "Servers").unwrap();
        assert_ne!(
            other.workspace_changes, first.workspace_changes,
            "another person's workspace is another history"
        );
    }
}
