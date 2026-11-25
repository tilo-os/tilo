//! A workspace's documents held by a replica this process keeps itself: what a page holds over
//! its own storage, and any device that holds a workspace without a service beside a store.
//!
//! The replica ([`tilo_peer_state::StatePeer`]) and the device it acts as are the holder's; what
//! a read, a listing and a write do with them is [`crate::documents`]'. The holder reads and
//! writes through [`DeviceDocuments`] and [`DeviceWrites`], so the session's own roles and the
//! apps of any embedder reach it as they reach a service's copy.
//!
//! What it keeps is the replica's durable state ([`HeldDocuments::durable`]), saved and restored
//! by whoever embeds it with the storage it already has: a page's durable store keeps it beside
//! the peer record it keeps its owner in.

use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use tilo_peer_state::owner::WriteRefusal;
use tilo_peer_state::replica::Replica;
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentWrite,
};
use tilo_peer_state::{ReceiveOutcome, StatePeer, TransportOrigin};

use tilo_lock::MutexRecover as _;

use super::device_keys::DeviceKeys;
use super::on_its_own;

/// The most bytes of one document a read hands out.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// One workspace's documents, held by a replica and read as the device it acts as, whose key
/// signs the writes this process makes of its own where it has one.
pub struct HeldDocuments {
    workspace: String,
    replica: Replica,
    device: [u8; 32],
    signer: Option<Arc<dyn DeviceSigner>>,
    keys: Option<DeviceKeys>,
    opened: super::Opened,
}

impl HeldDocuments {
    /// The documents of `workspace`, held in `peer` and read and written as `signer`'s device,
    /// with `keys` where that device takes part in the workspace's channels.
    #[must_use]
    pub fn new(
        workspace: &str,
        peer: StatePeer,
        signer: Arc<dyn DeviceSigner>,
        keys: Option<DeviceKeys>,
    ) -> Self {
        let device = signer.device_public_key();
        Self::over(workspace, Replica::new(peer), device, Some(signer), keys)
    }

    /// The documents of `workspace`, held in the replica its embedder keeps and admits into,
    /// read as the device `device` and written as its `signer` where this process has the key
    /// that signs its changes.
    #[must_use]
    pub fn over(
        workspace: &str,
        replica: Replica,
        device: [u8; 32],
        signer: Option<Arc<dyn DeviceSigner>>,
        keys: Option<DeviceKeys>,
    ) -> Self {
        Self {
            workspace: workspace.to_owned(),
            replica,
            device,
            signer,
            keys,
            opened: super::Opened::default(),
        }
    }

    /// The replica as it stands, for whoever saves it: the state its other holders sync from.
    ///
    /// # Errors
    ///
    /// The replica does not encode.
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        self.replica.read(StatePeer::durable)
    }

    /// The replica, for an embedder that reads and changes it itself.
    #[must_use]
    pub fn replica(&self) -> Replica {
        self.replica.clone()
    }

    /// Take in `message`, a frame of the workspace by the device `from` (its key), as a service's
    /// copy takes a member's write in: the caller authenticated the device that carried it.
    ///
    /// # Errors
    ///
    /// The frame is not admitted: its author is no device the workspace certifies, it is
    /// malformed, or this device cannot decide it.
    pub fn receive(
        &self,
        from: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        now: web_time::SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.replica
            .write(|peer| {
                peer.receive_from(TransportOrigin::RetainedServerDelta(from), message, now)
            })
            .map_err(|error| error.to_string())
    }

    /// The copy `workspace` is held for.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Whether `workspace` is this holder's.
    fn holds_workspace(&self, workspace: &str) -> bool {
        workspace == self.workspace
    }
}

impl DeviceDocuments for HeldDocuments {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        if !self.holds_workspace(workspace) {
            return Ok(DocumentRead::Absent);
        }
        let peer = self.replica.read(StatePeer::clone);
        let keys = self.keys.clone();
        let device = self.device;
        let since = since.unwrap_or_default().to_vec();
        let preparing = peer.clone();
        let route = (app.to_owned(), resource.to_owned());
        let named = workspace.to_owned();
        let prepared = on_its_own(async move {
            super::prepare(
                &preparing,
                keys.as_ref(),
                &named,
                (route.0.as_str(), route.1.as_str()),
                device,
            )
            .await
        });
        let prepared = prepared?;
        let opening = self.opened.of(workspace, app, resource);
        let mut held = opening.lock_recover();
        let mut kept = held.take();
        let read = super::finish(
            &peer,
            (app, resource),
            device,
            &since,
            MAX_DOCUMENT_BYTES,
            prepared,
            &mut kept,
        );
        let size = kept.as_ref().map_or(0, |(_, kept)| kept.size());
        if kept.is_some() {
            *held = kept;
        }
        drop(held);
        self.opened.kept((workspace, app, resource), size);
        read
    }

    fn readable(&self, workspace: &str, app: &str, resource: &str) -> bool {
        if !self.holds_workspace(workspace) {
            return false;
        }
        let peer = self.replica.read(StatePeer::clone);
        let keys = self.keys.clone();
        let device = self.device;
        let route = (app.to_owned(), resource.to_owned());
        let named = workspace.to_owned();
        on_its_own(async move {
            super::readable(
                &peer,
                keys.as_ref(),
                &named,
                (route.0.as_str(), route.1.as_str()),
                device,
                tilo_clock::wall(),
            )
            .await
        })
    }

    fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        if !self.holds_workspace(workspace) {
            return Err("this device holds no such workspace".to_owned());
        }
        Ok(self.replica.read(super::listing))
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.holds_workspace(workspace)
            && self.replica.read(|peer| super::holds(peer, app, resource))
    }

    fn workspaces(&self) -> Vec<String> {
        vec![self.workspace.clone()]
    }
}

impl DeviceWrites for HeldDocuments {
    fn write(
        &self,
        workspace: &str,
        changes: &[u8],
        writing: tilo_peer_state::workspace_documents::DocumentWriting<'_>,
    ) -> DocumentWrite {
        let Some(signer) = self.signer.clone() else {
            return Box::pin(async {
                Err(WriteRefusal::Refused(
                    "this holder has no device key to sign its writes".to_owned(),
                ))
            });
        };
        if !self.holds_workspace(workspace) {
            return Box::pin(async {
                Err(WriteRefusal::Refused(
                    "this device holds no such workspace".to_owned(),
                ))
            });
        }
        tilo_peer_state::owner::admit_local_write(
            self.replica.clone(),
            None,
            self.device,
            signer,
            workspace,
            tilo_peer_state::owner::LocalWrite {
                app: writing.app,
                document: writing.document,
                readers: writing.readers,
                changes,
                founding: writing.founding,
                snapshot: writing.snapshot,
                sealed: writing.sealed,
                commit: None,
                now: tilo_clock::wall(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    //! A holder without a service: it holds a workspace, takes in an admitted change, reads the
    //! record and its listing, and commits its own write.

    use std::sync::Arc;

    use automerge::transaction::{CommitOptions, Transactable as _};
    use automerge::{AutoCommit, ROOT, ReadDoc as _, Value};
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};
    use tilo_peer_state::workspace_documents::{
        DeviceDocuments as _, DeviceWrites as _, DocumentRead,
    };
    use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    use web_time::{Duration, SystemTime};

    use super::HeldDocuments;

    const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2_000))
            .expect("a time after the epoch")
    }

    /// The `n`th device of the person whose root key is `root`, certified by it.
    fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).expect("the root certifies its device");
        (key, cert)
    }

    /// What the layout's shared desktop shows at `front`.
    fn front(bytes: &[u8]) -> Option<String> {
        let document = AutoCommit::load(bytes).expect("the record loads");
        match document.get(ROOT, "front").expect("the record reads") {
            Some((Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    /// `bytes` with the layout's front at `front`, committed as a change of `key`'s device.
    fn write_front(
        bytes: &[u8],
        key: &SigningKey,
        front: &str,
        person: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut document = AutoCommit::load(bytes).expect("the record loads");
        document.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = document.get_heads();
        document
            .put(ROOT, "front", front)
            .expect("the layout's front");
        document.commit_with(
            CommitOptions::default()
                .with_message(tilo_desktop_model::shared_resource::change_message(person)),
        );
        (document.save_after(&heads), document.save())
    }

    /// `change` as device `key`'s signed write of the shared desktop.
    fn write_message(
        key: &SigningKey,
        change: &[u8],
        sequence: u64,
        now: SystemTime,
    ) -> (neighbour_wire::NeighbourMsg, [u8; 32]) {
        let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let mut envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(change.to_vec())
                .encode()
                .expect("a change encodes"),
            collab_types::document_artifact_id(DESKTOP_APP, WORKSPACE, DESKTOP_DOCUMENT),
            author,
            collab_types::document_attestation_id(DESKTOP_APP, WORKSPACE, DESKTOP_DOCUMENT),
            collab_types::OpKind::Change,
            now,
            sequence,
        );
        collab_types::sign_op(&mut envelope, key).expect("the device signs its change");
        (
            neighbour_wire::NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(author.as_bytes()),
                app_id: DESKTOP_APP.into(),
                cluster_id: WORKSPACE.into(),
                document_id: DESKTOP_DOCUMENT.into(),
                envelope: Box::new(envelope),
                readers: None,
            },
            *author.as_bytes(),
        )
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one holder's reads, its own write and a checkpoint of it, read top to bottom"
    )]
    fn a_holder_holds_reads_takes_in_and_writes_a_workspaces_documents() {
        let now = now();
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = device(&root, 2);
        let (other_key, other) = device(&root, 3);
        let mut peer = StatePeer::new(
            WORKSPACE,
            cert.clone(),
            vec![cert.clone(), other.clone()],
            RevocationList::new(),
            now,
        )
        .expect("a replica of the workspace");
        let mut desktop = AutoCommit::new();
        desktop.put(ROOT, "front", "Main").expect("the front");
        desktop.commit_with(
            CommitOptions::default()
                .with_message(tilo_desktop_model::shared_resource::change_message("ada")),
        );
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &cert, &[cert.clone(), other])
                    .expect("the workspace's governance"),
                documents: vec![SeedDocument {
                    app_id: DESKTOP_APP.into(),
                    document_id: DESKTOP_DOCUMENT.into(),
                    kind: DESKTOP_KIND.into(),
                    bytes: desktop.save(),
                }],
            },
            &key,
            now,
        )
        .expect("the workspace and its layout are installed");
        peer.select_exact_admissions()
            .expect("the admissions are selected");

        let holder = HeldDocuments::new(WORKSPACE, peer, Arc::new(key), None);
        assert_eq!(holder.workspaces(), vec![WORKSPACE.to_owned()]);

        let DocumentRead::Whole { bytes, .. } = holder
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
            .expect("the layout reads")
        else {
            panic!("a document this device holds reads whole");
        };
        assert_eq!(front(&bytes).as_deref(), Some("Main"));

        // Another device of the person writes the same record: the holder takes the change in as
        // the workspace admitted it.
        let current = holder
            .replica()
            .read(|peer| peer.document(DESKTOP_APP, DESKTOP_DOCUMENT))
            .expect("the holder keeps the record");
        let person = hex::encode(holder.replica().read(tilo_peer_state::StatePeer::owner));
        let (change, _) = write_front(&current, &other_key, "Elsewhere", &person);
        let (message, from) = write_message(&other_key, &change, 1, now);
        let outcome = holder
            .receive(from, message, now)
            .expect("the workspace admits the other device's change");
        assert!(outcome.changed, "the change landed");

        let DocumentRead::Whole { bytes, .. } = holder
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
            .expect("the layout reads")
        else {
            panic!("a document this device holds reads whole");
        };
        assert_eq!(front(&bytes).as_deref(), Some("Elsewhere"));
        assert!(
            holder
                .listing(WORKSPACE)
                .expect("the workspace is held")
                .contains(&(DESKTOP_APP.to_owned(), DESKTOP_DOCUMENT.to_owned()))
        );

        // Its own device's write is committed through the same core a service writes through.
        let current = holder
            .replica()
            .read(|peer| peer.document(DESKTOP_APP, DESKTOP_DOCUMENT))
            .expect("the holder keeps the record");
        let person = hex::encode(holder.replica().read(tilo_peer_state::StatePeer::owner));
        let (change, _) = write_front(&current, &SigningKey::from_bytes(&[2; 32]), "Mine", &person);
        let written = futures::executor::block_on(holder.write(
            WORKSPACE,
            &change,
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding: false,
                snapshot: false,
                sealed: None,
            },
        ));
        assert!(
            written.is_ok(),
            "the device's own write is taken: {}",
            written
                .err()
                .map(|refused| refused.to_string())
                .unwrap_or_default()
        );

        let DocumentRead::Whole { bytes, .. } = holder
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
            .expect("the layout reads")
        else {
            panic!("a document this device holds reads whole");
        };
        assert_eq!(front(&bytes).as_deref(), Some("Mine"));

        // The record whole over one the workspace holds already is a checkpoint, not a founding:
        // a page's first write after a reload sends it, and it is taken.
        let current = holder
            .replica()
            .read(|peer| peer.document(DESKTOP_APP, DESKTOP_DOCUMENT))
            .expect("the holder keeps the record");
        let mut checkpointed = AutoCommit::load(&current).expect("the record opens");
        checkpointed.set_actor(automerge::ActorId::from(
            SigningKey::from_bytes(&[2; 32])
                .verifying_key()
                .to_bytes()
                .to_vec(),
        ));
        checkpointed
            .put(ROOT, "front", "Checkpoint")
            .expect("the layout's front");
        checkpointed.commit();
        let written = futures::executor::block_on(holder.write(
            WORKSPACE,
            &checkpointed.save(),
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding: false,
                snapshot: true,
                sealed: None,
            },
        ));
        assert!(
            written.is_ok(),
            "a checkpoint of a record the workspace holds is taken: {}",
            written
                .err()
                .map(|refused| refused.to_string())
                .unwrap_or_default()
        );
        let DocumentRead::Whole { bytes, .. } = holder
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
            .expect("the layout reads")
        else {
            panic!("a document this device holds reads whole");
        };
        assert_eq!(front(&bytes).as_deref(), Some("Checkpoint"));
        assert!(!holder.durable().expect("the replica encodes").is_empty());
    }
}
