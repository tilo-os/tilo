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

use std::sync::{Arc, Mutex};

use collab_types::signer::DeviceSigner;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::owner::{AppWrite, WriteRefusal};
use tilo_peer_state::owner_commit::CommitOperation;
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentWrite,
};
use tilo_peer_state::{ReceiveOutcome, StatePeer, TransportOrigin};

use super::device_keys::DeviceKeys;
use super::on_its_own;

/// The most bytes of one document a read hands out.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// One workspace's documents, held by a replica and written as the device whose key signs it.
pub struct HeldDocuments {
    workspace: String,
    held: Arc<Mutex<StatePeer>>,
    device: [u8; 32],
    signer: Arc<dyn DeviceSigner>,
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
        Self {
            workspace: workspace.to_owned(),
            device: signer.device_public_key(),
            held: Arc::new(Mutex::new(peer)),
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
        self.held.lock_recover().durable()
    }

    /// The replica, for an embedder that reads and changes it itself.
    #[must_use]
    pub fn replica(&self) -> Arc<Mutex<StatePeer>> {
        Arc::clone(&self.held)
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
        self.held
            .lock_recover()
            .receive_from(TransportOrigin::RetainedServerDelta(from), message, now)
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
        let peer = self.held.lock_recover().clone();
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
        let peer = self.held.lock_recover().clone();
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
        Ok(super::listing(&self.held.lock_recover()))
    }

    fn own_records(
        &self,
        workspace: &str,
    ) -> std::collections::BTreeMap<(String, String), [u8; 32]> {
        if !self.holds_workspace(workspace) {
            return std::collections::BTreeMap::new();
        }
        super::own_records(&self.held.lock_recover())
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.holds_workspace(workspace) && super::holds(&self.held.lock_recover(), app, resource)
    }

    fn workspaces(&self) -> Vec<String> {
        vec![self.workspace.clone()]
    }
}

impl DeviceWrites for HeldDocuments {
    fn write(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> DocumentWrite {
        let held = Arc::clone(&self.held);
        let signer = Arc::clone(&self.signer);
        let device = self.device;
        let mine = self.holds_workspace(workspace);
        let (workspace, app, document) =
            (workspace.to_owned(), app.to_owned(), document.to_owned());
        let changes = changes.to_vec();
        let sealed = sealed.cloned();
        Box::pin(async move {
            if !mine {
                return Err(WriteRefusal::Refused(
                    "this device holds no such workspace".to_owned(),
                ));
            }
            let now = tilo_clock::wall();
            // The write is drafted against the replica as it stands, signed, and admitted as this
            // device's: the guard is never held across the signer's answer.
            let snapshot = held.lock_recover().clone();
            let (mut request, author) = super::draft(
                &snapshot,
                &workspace,
                &device,
                super::Write {
                    app: &app,
                    document: &document,
                    readers: None,
                    changes: &changes,
                    founding,
                    sealed: sealed.as_ref(),
                    now,
                },
            )?;
            let CommitOperation::Document { envelope, own, .. } = &mut request.operation else {
                return Err(WriteRefusal::Refused(
                    "a local write is a document write".to_owned(),
                ));
            };
            let bytes = envelope
                .signing_bytes()
                .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
            let signature = signer
                .sign(bytes)
                .await
                .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
            envelope.signature = collab_types::Signature(signature);
            let (envelope, own) = ((**envelope).clone(), own.clone());
            drop(snapshot);
            held.lock_recover()
                .admit_new_local_document(&app, &document, envelope.clone(), now)
                .map_err(|refused| WriteRefusal::Refused(refused.to_string()))?;
            Ok(AppWrite {
                peer: neighbour_wire::peer_handle(&device),
                app_id: app,
                cluster_id: workspace,
                document_id: document,
                author,
                replayed: false,
                envelope,
                own,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    //! A holder without a service: it holds a workspace, takes in an admitted change, reads the
    //! record and its listing, and commits its own write.

    use std::sync::Arc;

    use automerge::transaction::{CommitOptions, Transactable as _};
    use automerge::{AutoCommit, ReadDoc as _, ROOT, Value};
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};
    use tilo_lock::MutexRecover as _;
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
    fn write_front(bytes: &[u8], key: &SigningKey, front: &str) -> (Vec<u8>, Vec<u8>) {
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
                .with_message(tilo_desktop_model::shared_resource::change_message(None)),
        );
        (document.save_after(&heads), document.save())
    }

    /// `change` as device `key`'s signed write of the shared desktop.
    fn write_message(key: &SigningKey, change: &[u8], sequence: u64, now: SystemTime) -> (neighbour_wire::NeighbourMsg, [u8; 32]) {
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
                own: None,
                readers: None,
            },
            *author.as_bytes(),
        )
    }

    #[test]
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
                .with_message(tilo_desktop_model::shared_resource::change_message(None)),
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
            .lock_recover()
            .document(DESKTOP_APP, DESKTOP_DOCUMENT)
            .expect("the holder keeps the record");
        let (change, _) = write_front(&current, &other_key, "Elsewhere");
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
            .lock_recover()
            .document(DESKTOP_APP, DESKTOP_DOCUMENT)
            .expect("the holder keeps the record");
        let (change, _) = write_front(&current, &SigningKey::from_bytes(&[2; 32]), "Mine");
        let written = futures::executor::block_on(holder.write(
            WORKSPACE,
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            &change,
            false,
            None,
        ));
        assert!(
            written.is_ok(),
            "the device's own write is taken: {}",
            written.err().map(|refused| refused.to_string()).unwrap_or_default()
        );

        let DocumentRead::Whole { bytes, .. } = holder
            .read(WORKSPACE, DESKTOP_APP, DESKTOP_DOCUMENT, None)
            .expect("the layout reads")
        else {
            panic!("a document this device holds reads whole");
        };
        assert_eq!(front(&bytes).as_deref(), Some("Mine"));
        assert!(!holder.durable().expect("the replica encodes").is_empty());
    }
}
