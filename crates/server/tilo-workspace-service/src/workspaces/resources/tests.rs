//! A member's resource writes reach the governor's store over the path every member write
//! takes, are queryable there, and are read back from the store's snapshots after a restart.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "a test failing is its assertion failing"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use collab_types::{ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use key_group::ContentKeys;
use neighbour_wire::NeighbourMsg;
use tilo_resource_store::{Cell, Query, Resource, Stamp, Store, Value};

use super::super::Workspaces;
use super::super::fixtures::{TestGroupKeys, governor_with, test_host_holding};
use super::super::sealed_documents::fixtures::member_device;

const APP: &str = "so.tilo.tasks";
const KIND: &str = "tasks.issue";

/// A member device writing a workspace's resources through a store of its own, sealing each
/// document's change under the workspace's keys as its engine does.
struct Writer {
    key: SigningKey,
    keys: ContentKeys,
    store: Store,
    seq: u64,
}

impl Writer {
    async fn new(workspaces: &Workspaces, workspace: &str, key: SigningKey) -> Self {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: APP.into(),
            cluster_id: workspace.into(),
            document: String::new(),
        };
        let keys = workspaces
            .own_route_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        Self {
            key,
            keys,
            store: Store::new(),
            seq: 0,
        }
    }

    /// Writes the issue `id` titled `title` and hands each document's change to the workspace,
    /// as the server hands it every member write.
    async fn put(&mut self, workspaces: &Workspaces, workspace: &str, id: &str, title: &str) {
        let fields = BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]);
        let resource = Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields,
        };
        let written = self
            .store
            .put(KIND, id, resource, &Stamp::default())
            .unwrap();
        for write in written {
            let founding = self.seq == 0;
            let mutation = if founding {
                let whole = self.store.document(&write.doc).unwrap().save();
                collab_types::DocumentMutation::Snapshot(whole)
            } else {
                collab_types::DocumentMutation::Incremental(write.change.raw_bytes().to_vec())
            };
            let body = self.seal(workspace, &write.doc, &mutation.encode().unwrap(), founding);
            workspaces
                .absorb(workspace, self.key.verifying_key().to_bytes(), &body)
                .await
                .unwrap();
        }
    }

    fn seal(&mut self, workspace: &str, doc: &str, plaintext: &[u8], founding: bool) -> Vec<u8> {
        let retention = if founding {
            ContentRetention::Checkpoint
        } else {
            ContentRetention::Delta
        };
        let artifact = collab_types::document_artifact_id(APP, workspace, doc);
        let (metadata, payload) = self
            .keys
            .seal_content(artifact, retention, plaintext)
            .unwrap();
        let author =
            collab_types::Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes());
        self.seq = self.seq.saturating_add(1);
        let mut envelope = SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            collab_types::document_attestation_id(APP, workspace, doc),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
            self.seq,
            metadata,
        );
        collab_types::sign_op(&mut envelope, &self.key).unwrap();
        postcard::to_stdvec(&NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: APP.into(),
            cluster_id: workspace.into(),
            document_id: doc.into(),
            envelope: Box::new(envelope),
            own: None,
            readers: None,
        })
        .unwrap()
    }
}

/// The title of issue `id` as the governor's store holds it, read as anyone may read it.
async fn title(workspaces: &Workspaces, workspace: &str, id: &str) -> Option<Value> {
    let governed = workspaces.workspace(workspace).await.unwrap();
    let governed = governed.lock().await;
    governed
        .resources
        .store()
        .get(KIND, id, &|readers| readers == "*")
        .map(|held| held.scalar("title"))
}

/// The service writes its device's resources through its own store, as the device's signed and
/// sealed document writes the workspace's other members follow, and reads them back at once.
#[tokio::test]
async fn a_device_writes_its_resources_through_the_service_and_reads_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    // The service acts as this device, as a desktop's does: what it writes is the device's.
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    // The workspace's content keys are here before an app writes in it.
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();

    let stamp = Stamp {
        verb: "add".to_owned(),
        invoker: "ada".to_owned(),
        invocation: "i1".to_owned(),
        ..Stamp::default()
    };
    let issue = |title: &str| Resource {
        readers: "*".into(),
        parent: None,
        order: None,
        fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
    };
    let read = |workspaces: &Workspaces, workspace: &str, id: &str| {
        let workspaces = workspaces.clone();
        let workspace = workspace.to_owned();
        let id = id.to_owned();
        async move {
            workspaces
                .resource_get(&workspace, KIND, &id)
                .await
                .unwrap()
                .map(|held| held.scalar("title"))
        }
    };
    for title in ["First", "Renamed"] {
        workspaces
            .write_resources(&workspace, APP, |store| {
                store.put(KIND, "i-1", issue(title), &stamp)
            })
            .await
            .unwrap();
        assert_eq!(
            read(&workspaces, &workspace, "i-1").await,
            Some(Value::Text(title.into())),
            "the write is read back at once"
        );
    }
    // The workspace's replica follows the document its store holds: its next change is drafted
    // after it, and the workspace's other members hear the write as they hear any.
    {
        let governed = workspaces.workspace(&workspace).await.unwrap();
        let governed = governed.lock().await;
        let held = {
            let store = governed.resources.store();
            let doc = store
                .index()
                .documents()
                .next()
                .map(str::to_owned)
                .expect("the store placed a document");
            (
                doc,
                store
                    .index()
                    .len(&Query::of(KIND, 10), &|readers| readers == "*"),
            )
        };
        assert_eq!(held.1, 1, "the store indexes the issue");
        assert!(
            governed.peer().holds_document(APP, &held.0),
            "the workspace's replica holds the document the store placed"
        );
    }
    // A reader the issue's set leaves out reads nothing of it.
    assert!(
        read(&workspaces, &workspace, "i-2").await.is_none(),
        "an issue nobody wrote is not there"
    );
    drop(workspaces);
    server.shutdown().await;
    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key),
    }));
    assert_eq!(
        read(&workspaces, &workspace, "i-1").await,
        Some(Value::Text("Renamed".into())),
        "a restart reads the store back"
    );
    drop(workspaces);
    server.shutdown().await;
}

#[tokio::test]
async fn a_members_resource_writes_are_queryable_on_the_governor_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let (member_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let mut writer = Writer::new(&workspaces, &workspace, member_key).await;

    let mut woken = workspaces.document_writes();
    writer.put(&workspaces, &workspace, "i-1", "First").await;
    let write = woken.try_recv().expect("the workspace's bots are woken");
    assert_eq!(
        (write.app.as_str(), write.workspace.as_str()),
        (APP, workspace.as_str())
    );
    assert!(collab_types::is_store_document(&write.record));
    assert_eq!(
        title(&workspaces, &workspace, "i-1").await,
        Some(Value::Text("First".into())),
        "the write that founds a document is indexed"
    );
    writer.put(&workspaces, &workspace, "i-1", "Renamed").await;
    writer.put(&workspaces, &workspace, "i-2", "Second").await;
    assert_eq!(
        title(&workspaces, &workspace, "i-1").await,
        Some(Value::Text("Renamed".into())),
        "a later change of the document is indexed"
    );
    {
        let governed = workspaces.workspace(&workspace).await.unwrap();
        let governed = governed.lock().await;
        let store = governed.resources.store();
        assert_eq!(store.count(KIND, &[], &|readers| readers == "*"), 2);
        assert_eq!(
            store.count(KIND, &[], &|_| false),
            0,
            "a reader the reader set leaves out is shown nothing"
        );
    }

    drop(workspaces);
    server.shutdown().await;
    let index = dir
        .path()
        .join("workspaces")
        .join(created.workspace_id.to_string())
        .join("store")
        .join("index");
    assert_eq!(
        std::fs::read_dir(&index).map_or(0, Iterator::count),
        1,
        "the store kept its document's snapshot as it stopped"
    );

    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    assert_eq!(
        title(&workspaces, &workspace, "i-1").await,
        Some(Value::Text("Renamed".into())),
        "a restart reads the store back"
    );
    {
        let governed = workspaces.workspace(&workspace).await.unwrap();
        let governed = governed.lock().await;
        assert_eq!(
            governed.resources.store().in_memory(),
            0,
            "the start read the snapshot, not the document"
        );
    }
    drop(workspaces);
    server.shutdown().await;
}

#[tokio::test]
async fn a_write_by_a_device_the_workspace_never_admitted_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::default()).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    // A device holding the workspace's keys, as a removed member's may, that no member is.
    let stranger = SigningKey::from_bytes(&[9; 32]);
    let mut stranger = Writer::new(&workspaces, &workspace, stranger).await;

    stranger
        .put(&workspaces, &workspace, "i-1", "Planted")
        .await;
    assert_eq!(title(&workspaces, &workspace, "i-1").await, None);
    drop(workspaces);
    server.shutdown().await;
}
