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
    uuid::Uuid::parse_str(workspace).map_err(err)?;
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    // The name it is made with is part of making it: one change, which its history reads as the
    // workspace being made, never as someone naming it afterwards.
    founding_rows(
        &mut doc,
        workspace,
        name,
        &authority.user_public_key,
        &hex::encode(authority.device_public_key),
        roster,
    )?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64).map_err(err)?;
    super::commit_governance(&mut doc, super::now_secs(now), None);
    Ok(SeedState {
        workspace_changes: changes_of(&mut doc),
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
        "Shared browser workspace",
        &authority.user_public_key,
        &hex::encode(authority.device_public_key),
        roster,
    )?;
    super::commit_governance(&mut doc, unix_secs, None);
    Ok(changes_of(&mut doc))
}

/// The changes `doc` holds, encoded.
fn changes_of(doc: &mut AutoCommit) -> Vec<Vec<u8>> {
    doc.get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// The governance rows a founding installs in `doc`: the workspace it is, its
/// `name`, its owner `person`, its `founder`, the maps every device and
/// governor adds to, and the `roster`'s certificates.
fn founding_rows(
    doc: &mut AutoCommit,
    workspace: &str,
    name: &str,
    person: &[u8; 32],
    founder: &str,
    roster: &[DeviceCert],
) -> Result<(), String> {
    doc.put(ROOT, "workspace", workspace).map_err(err)?;
    doc.put(ROOT, "name", name).map_err(err)?;
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
        super::keeping_devices::WORKSPACE_KEEPING_DEVICES_KEY,
        super::app_versions::APP_VERSIONS,
        super::package_trust::ATTESTORS,
        super::package_trust::REVOKED,
        super::package_registries::REGISTRIES,
        super::calls::CALLS,
        tilo_desktop_model::projection::WORKSPACE_AUTOMATIONS_KEY,
        tilo_desktop_model::projection::WORKSPACE_PRESETS_KEY,
        tilo_desktop_model::projection::WORKSPACE_SANDBOX_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_HISTORY_KEY,
        tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY,
        super::CHECKPOINT_CHECKS_KEY,
        super::invitation_receipts::PARTS_KEY,
        crate::document_import::WORKSPACE_KEY,
        crate::removals::FIELD_V1,
        crate::standing::FIELD_V1,
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
        // The person, not their device: the label stays in the certificate.
        doc.put(&names, &id, "").map_err(err)?;
        let identity = GovernedIdentity {
            device_cert: certificate.clone(),
            revocations: vec![],
            admission: None,
            admissions: vec![],
            delegate: None,
            workspace_delegations: Vec::new(),
            removed: false,
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

#[cfg(any(test, feature = "test-support"))]
impl super::StatePeer {
    /// Name this copy's person, not a device, in its founder row: a copy of a
    /// Servers workspace as an earlier build founded it alike on each of its
    /// person's devices, which a test has founded again under one device.
    ///
    /// # Panics
    ///
    /// The copy holds no governance yet.
    #[expect(
        clippy::expect_used,
        reason = "a governed copy always holds its governance; one that does not is a programming error, as the doc says"
    )]
    pub fn name_person_as_founder(&mut self) {
        let mut doc = (**self
            .core
            .workspace_doc
            .as_ref()
            .expect("a governed copy holds its governance"))
        .clone();
        doc.put(ROOT, FOUNDER, hex::encode(self.core.owner))
            .expect("the founder row is a string");
        self.core.workspace_doc = Some(doc.into());
    }
}
