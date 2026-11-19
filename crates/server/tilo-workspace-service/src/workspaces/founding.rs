//! A workspace's first history: the seed it starts from, and the record that
//! keeps it here.
//!
//! A workspace that is one person's own — the Servers workspace, whose id
//! follows from their root — is founded in their name, so every one of their
//! devices that makes it writes the same history and two of them that make it
//! before they see one another make one workspace. The founding names no
//! device: the device that makes it enters itself as the owner's own, on its
//! certificate, so it writes the workspace from then on.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_peer_state::{SeedState, StatePeer};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

use super::commit::{prepare_governance, publication, select_exact_admission};
use super::{
    DOCUMENT_VERSION, DeviceKeys, GovernedWorkspace, PUBLICATIONS, RECEIPTS,
    WorkspaceDocumentEntry, appended, collaboration, peer_parts,
};

/// The person in whose name `workspace` is founded, when it is one person's
/// own: their Servers workspace, under the id derived from their root.
pub(super) fn founded_by(workspace: Option<&str>, person: &[u8; 32]) -> Option<[u8; 32]> {
    let workspace = WorkspaceId::parse(workspace?)?;
    (workspace.to_string() == tilo_server_protocol::servers::admin_workspace_id(person))
        .then_some(*person)
}

/// The seed `workspace` starts from: the one history every one of `person`'s
/// devices makes when it is theirs, else the history the founding `authority`
/// makes with its `roster`.
///
/// # Errors
/// The workspace is not a UUID, or the document cannot be built.
pub(super) fn seed(
    workspace: &str,
    name: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    founded_by: Option<[u8; 32]>,
    now: SystemTime,
) -> Result<SeedState, String> {
    match founded_by {
        Some(person) => tilo_peer_state::person_workspace_seed(workspace, &person, name),
        None => tilo_peer_state::workspace_seed(workspace, authority, roster, name, now),
    }
}

/// Have `peer`, a workspace founded in its person's name, take in `authority`
/// as the owner's own device: an ordinary change every founding device makes
/// on its certificate, which the other foundings' changes merge with.
///
/// # Errors
/// The certificate is not the owner's device, is revoked, or does not hold up.
pub(super) fn enter(
    peer: &mut StatePeer,
    authority: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
    founded_by: Option<[u8; 32]>,
) -> Result<(), String> {
    if founded_by.is_some() {
        peer.admit_certificate(authority.clone(), key, now)?;
    }
    Ok(())
}

impl GovernedWorkspace {
    pub(super) fn create(
        dir: &Path,
        name: &str,
        authority: &DeviceCert,
        key: &SigningKey,
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        Self::create_in(
            dir,
            &uuid::Uuid::new_v4().to_string(),
            name,
            (authority, key),
            device,
            now,
        )
    }

    /// A new governed workspace under the identity `workspace`: a fresh one, or the
    /// workspace a workspace already is when it moves here from the device that
    /// made it (a workspace is its workspace, and keeps it when its governance
    /// moves).
    pub(super) fn create_in(
        dir: &Path,
        workspace: &str,
        name: &str,
        (authority, key): (&DeviceCert, &SigningKey),
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let name = tilo_peer_state::workspace_membership::valid_founding_name(name)?;
        let workspace = uuid::Uuid::parse_str(workspace)
            .map_err(|_| "a workspace is a UUID".to_owned())?
            .hyphenated()
            .to_string();
        if dir.join(format!("{workspace}.workspace")).exists() {
            return Err("this host has that workspace already".into());
        }
        let founded_by = founded_by(Some(&workspace), &authority.user_public_key);
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &workspace,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        let mut seed = seed(&workspace, name, authority, &roster, founded_by, now)?;
        if device.is_some() {
            // The workspace's records are sealed channels this node opens on
            // first use: a seed record would go out in the clear before its
            // channel exists, and every member who joins after the channel
            // opened refuses that cleartext. The shared desktop starts with
            // the first layout written to it, sealed, as in a workspace that
            // never had one (`prepare_desktop_document`).
            seed.documents.clear();
        }
        peer.install_seed(seed, key, now)?;
        prepare_governance(&mut peer, key, now)?;
        enter(&mut peer, authority, key, now, founded_by)?;
        let outgoing = peer
            .publication_mut()
            .durable_bootstrap_messages(key, now, 1)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let path = dir.join(format!("{workspace}.workspace"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = WorkspaceDocumentEntry {
            version: DOCUMENT_VERSION,
            workspace_id: WorkspaceId::parse(&workspace)
                .ok_or_else(|| "a workspace is named by its id".to_string())?,
            workspace: workspace.clone(),
            owner: authority.clone(),
            peer: parts.stage(&peer)?,
            generation: 1,
            outgoing,
            published: 0,
            collaboration: collaboration::CollaborationState::default(),
        };
        let (publications, _) = appended::Appended::open(&path, PUBLICATIONS, 0)?;
        let (receipts, _) = appended::Appended::open(&path, RECEIPTS, 0)?;
        let mut this = Self::assemble(record, peer, path, parts, (publications, receipts), device)?;
        this.persist()?;
        Ok(this)
    }
}
