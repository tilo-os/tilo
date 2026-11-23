//! A workspace's first history: the seed it starts from, and the record that
//! keeps it here.
//!
//! Every workspace is founded once, by the device that makes it, and names that
//! device as its founder. Every other device takes part through enrolment.
//! The person's own Servers workspace is no exception: its id follows from
//! their root so their devices find it, but only one device founds it. A
//! device that founds it again founds another history under the same id, and
//! a host that holds the first one refuses it.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_peer_state::StatePeer;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

use super::commit::{prepare_governance, publication, select_exact_admission};
use super::{
    DOCUMENT_VERSION, DeviceKeys, GovernedWorkspace, PUBLICATIONS, RECEIPTS,
    WorkspaceDocumentEntry, appended, collaboration, peer_parts,
};

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
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &workspace,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        let mut seed = tilo_peer_state::workspace_seed(&workspace, authority, &roster, name, now)?;
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
        let outgoing = peer
            .publication_mut()
            .bootstrap_messages(key, now)?
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
        this.moderation_history_checked
            .store(true, std::sync::atomic::Ordering::Relaxed);
        this.persist()?;
        Ok(this)
    }
}
