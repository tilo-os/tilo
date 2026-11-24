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
use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Cell, Query, Resource, Stamp, Store, Value};

use super::super::Workspaces;
use super::super::fixtures::{TestGroupKeys, declare_app, governor_with, test_host_holding};
use super::super::sealed_documents::fixtures::member_device;
use crate::apps::services::ResourceRequestContext;

const APP: &str = "so.tilo.tasks";
const KIND: &str = "tl.so.tilo.tasks.Task";

/// What a write of a task claims: the host's verb for a tilolang package's root, as the app's
/// own program writes a record it makes.
fn create() -> Stamp {
    Stamp {
        verb: "$create".to_owned(),
        ..Stamp::default()
    }
}

/// The entry's surface: the address a visitor leaves and the verbs the workspace's entry opens
/// to any key. The entry is the workspace's own setting rather than a package this build carries,
/// so a test declares it as an installed app's surface is declared.
const ENTRY_SURFACE: &str = r#"format = 2

[[resources]]
kind = "entry.request"
label = "Request"
version = 1
fields = [
  { path = "title", type = "text" },
]
verbs = [
  { id = "join", label = "Ask to join", kind = "undoable" },
  { id = "confirm", label = "Confirm the address", kind = "undoable" },
  { id = "leave", label = "Take the address off", kind = "undoable" },
  { id = "remove", label = "Remove", kind = "undoable" },
]
"#;

fn context(workspace: &str, cert: &tilo_user_identity::DeviceCert) -> ResourceRequestContext {
    ResourceRequestContext::authenticated(
        APP,
        workspace,
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace),
    )
}

/// A member device writing a workspace's resources through a store of its own, sealing each
/// document's change under the workspace's keys as its engine does. Its changes name `of`, the
/// member whose device it is, as who made them: every change says who made it, and one that
/// names no one is refused.
struct Writer {
    key: SigningKey,
    keys: ContentKeys,
    store: Store,
    seq: u64,
    of: Option<tilo_action_policy::PersonId>,
}

impl Writer {
    /// A writer whose device is not a member names no one.
    async fn new(workspaces: &Workspaces, workspace: &str, key: SigningKey) -> Self {
        Self::naming(workspaces, workspace, key, None).await
    }

    /// A writer whose device is `member`'s, its changes that person's.
    async fn for_member(
        workspaces: &Workspaces,
        workspace: &str,
        key: SigningKey,
        member: &tilo_user_identity::DeviceCert,
    ) -> Self {
        let of = Some(tilo_action_policy::PersonId::from_root(
            member.user_public_key,
        ));
        Self::naming(workspaces, workspace, key, of).await
    }

    async fn naming(
        workspaces: &Workspaces,
        workspace: &str,
        key: SigningKey,
        of: Option<tilo_action_policy::PersonId>,
    ) -> Self {
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
            of,
        }
    }

    /// Writes the issue `id` titled `title` and hands each document's change to the workspace,
    /// as the server hands it every member write.
    async fn put(&mut self, workspaces: &Workspaces, workspace: &str, id: &str, title: &str) {
        let fields = BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]);
        self.put_fields(workspaces, workspace, id, fields).await;
    }

    /// Writes the record `id` whose title is `reference`: what an app keeps when its record
    /// names a file's bytes by address, and none of them.
    async fn put_content(
        &mut self,
        workspaces: &Workspaces,
        workspace: &str,
        id: &str,
        reference: &str,
    ) -> String {
        let fields = BTreeMap::from([(
            "title".to_owned(),
            Cell::One(Value::Text(reference.to_owned())),
        )]);
        self.put_fields(workspaces, workspace, id, fields).await
    }

    /// Writes `fields` as the resource `id` of [`KIND`] and hands each document's change to
    /// `workspace`; the document the store placed it in.
    async fn put_fields(
        &mut self,
        workspaces: &Workspaces,
        workspace: &str,
        id: &str,
        fields: BTreeMap<String, Cell>,
    ) -> String {
        let (placed, bodies) = self.draft(workspace, id, fields);
        for body in bodies {
            workspaces
                .absorb(workspace, self.key.verifying_key().to_bytes(), &body)
                .await
                .unwrap();
        }
        placed
    }

    fn draft(
        &mut self,
        workspace: &str,
        id: &str,
        fields: BTreeMap<String, Cell>,
    ) -> (String, Vec<Vec<u8>>) {
        let resource = Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields,
        };
        let mut stamp = create();
        if let Some(person) = &self.of {
            stamp.invoker = Stamp::by(&tilo_action_policy::Actor::Member(
                tilo_action_policy::MemberId::person(person.clone()),
            ));
        }
        let written = self.store.put(KIND, id, resource, &stamp).unwrap();
        let mut placed = String::new();
        let mut bodies = Vec::new();
        for write in written {
            let founding = self.seq == 0;
            let mutation = if founding {
                let whole = self.store.document(&write.doc).unwrap().save();
                collab_types::DocumentMutation::Snapshot(whole)
            } else {
                collab_types::DocumentMutation::Incremental(write.change.raw_bytes().to_vec())
            };
            bodies.push(self.seal(workspace, &write.doc, &mutation.encode().unwrap(), founding));
            placed = write.doc;
        }
        (placed, bodies)
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

/// Whether the store of this device's copy of `workspace` holds the issue `id`, whoever reads it.
async fn holds(workspaces: &Workspaces, workspace: &str, id: &str) -> bool {
    let governed = workspaces.workspace(workspace).await.unwrap();
    let governed = governed.lock().await;
    governed
        .resources
        .store()
        .get(KIND, id, &|_| true)
        .is_some()
}

/// The title of the issue `id` of `workspace`, as an app reads it through the service.
async fn title_through_service(
    workspaces: &Workspaces,
    context: &ResourceRequestContext,
    id: &str,
) -> Option<Value> {
    let get = Command::Get {
        kind: KIND.to_owned(),
        id: id.to_owned(),
    };
    match workspaces.resource_command(context, get).await.unwrap() {
        Reply::Resource(held) => held.map(|held| held.scalar("title")),
        _ => None,
    }
}

async fn assert_delegated_write_refused(
    workspaces: &Workspaces,
    context: &ResourceRequestContext,
    cert: &tilo_user_identity::DeviceCert,
    resource: Resource,
    stamp: Stamp,
) {
    let delegated = ResourceRequestContext::authenticated(
        APP,
        context.workspace_id(),
        tilo_actions::remote::Caller::delegated_agent(
            "scribe",
            cert.user_public_key,
            cert.device_public_key,
        )
        .in_workspace(context.workspace_id()),
    );
    let denied = workspaces
        .resource_command(
            &delegated,
            Command::Put {
                kind: KIND.into(),
                id: "denied".into(),
                resource,
                stamp,
            },
        )
        .await
        .unwrap_err();
    assert!(denied.contains("no caller-scoped signer"));
    assert!(
        title_through_service(workspaces, context, "denied")
            .await
            .is_none()
    );
}

/// The versions of issue `i-1`, oldest first, are by `authors`: whoever the service named on each
/// write, whatever its stamp claimed.
async fn assert_certified_history(
    workspaces: &Workspaces,
    context: &ResourceRequestContext,
    authors: &[tilo_actions::Actor],
) {
    let Reply::Versions(versions) = workspaces
        .resource_command(
            context,
            Command::Versions {
                kind: KIND.into(),
                id: "i-1".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("the resource has a history");
    };
    let by: Vec<_> = versions
        .iter()
        .map(|version| version.stamp.as_ref().and_then(Stamp::invoked_by))
        .collect();
    let expected: Vec<_> = authors.iter().cloned().map(Some).collect();
    assert_eq!(by, expected, "each version names who made it");
}

/// The replica follows the document placed by the store so other members hear its changes.
async fn assert_replica_holds_issue(workspaces: &Workspaces, workspace: &str) {
    let governed = workspaces.workspace(workspace).await.unwrap();
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

/// The sealed writes the replica keeps of the document the store placed name the kinds of the
/// records they write, read from the store: what a member without the document's key decides the
/// edit after the first write by.
async fn assert_sealed_claims_the_issue(workspaces: &Workspaces, workspace: &str) {
    let governed = workspaces.workspace(workspace).await.unwrap();
    let governed = governed.lock().await;
    let doc = {
        let store = governed.resources.store();
        store
            .index()
            .documents()
            .next()
            .map(str::to_owned)
            .expect("the store placed a document")
    };
    let claims: Vec<std::collections::BTreeSet<String>> = governed
        .peer()
        .publication()
        .sealed_history(APP, &doc)
        .iter()
        .flat_map(|op| op.claims.iter())
        .filter_map(|claim| collab_types::claim::whole_edit_kinds(&claim.action))
        .collect();
    assert!(
        claims
            .iter()
            .any(|kinds| kinds == &std::collections::BTreeSet::from([KIND.to_owned()])),
        "the sealed writes claim the issue's kind"
    );
}

/// A write the app makes while serving an invocation the workspace service delivered to it is
/// that invocation's invoker's: an agent acting for the person is named as the agent. Answers the
/// agent the write names.
async fn assert_scribed_as_agent(
    workspaces: &Workspaces,
    context: &ResourceRequestContext,
    cert: &tilo_user_identity::DeviceCert,
    resource: Resource,
    stamp: &Stamp,
) -> tilo_actions::Actor {
    let agent = tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe"));
    let delivered = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    // The agent works for the person whose device carried the write: what every receiver
    // decides it as, never as a bare name.
    let acted_for = hex::encode(
        context
            .caller()
            .user_public_key()
            .expect("the device's person"),
    );
    // An agent writes what its person handed it: the delegation this workspace's member list
    // holds for `scribe`, as the person gives it from their own device.
    let grant = tilo_action_policy::Grant::new(
        "scribe-writes-the-issues",
        tilo_action_policy::Grantee::agent("scribe"),
        tilo_action_policy::TreePath::workspace(context.workspace_id()),
        tilo_action_policy::KindSet::UNDOABLE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(acted_for.clone()),
    );
    workspaces
        .put_workspace_grants(context.workspace_id(), cert, [23; 32], vec![grant])
        .await
        .unwrap();
    workspaces
        .resource_command(
            &context.clone().serving(vec![(delivered, agent.clone())]),
            Command::Put {
                kind: KIND.to_owned(),
                id: "i-1".to_owned(),
                resource,
                stamp: Stamp {
                    invocation: delivered.0.to_string(),
                    acted_for: Some(acted_for),
                    ..stamp.clone()
                },
            },
        )
        .await
        .unwrap();
    agent
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
    let context = context(&workspace, &cert);
    // The workspace's content keys are here before an app writes in it.
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();

    let stamp = Stamp {
        verb: "$create".to_owned(),
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
    assert_delegated_write_refused(&workspaces, &context, &cert, issue("Denied"), stamp.clone())
        .await;
    for title in ["First", "Renamed"] {
        workspaces
            .resource_command(
                &context,
                Command::Put {
                    kind: KIND.to_owned(),
                    id: "i-1".to_owned(),
                    resource: issue(title),
                    stamp: stamp.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            title_through_service(&workspaces, &context, "i-1").await,
            Some(Value::Text(title.into())),
            "the write is read back at once"
        );
    }
    assert_sealed_claims_the_issue(&workspaces, &workspace).await;
    let agent =
        assert_scribed_as_agent(&workspaces, &context, &cert, issue("Scribed"), &stamp).await;
    assert_replica_holds_issue(&workspaces, &workspace).await;
    // A reader the issue's set leaves out reads nothing of it.
    assert!(
        title_through_service(&workspaces, &context, "i-2")
            .await
            .is_none(),
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
    let person = tilo_actions::Actor::Member(tilo_actions::MemberId::person(hex::encode(
        cert.user_public_key,
    )));
    assert_certified_history(&workspaces, &context, &[person.clone(), person, agent]).await;
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("Scribed".into())),
        "a restart reads the store back"
    );
    assert!(
        title_through_service(&workspaces, &context, "denied")
            .await
            .is_none()
    );
    drop(workspaces);
    server.shutdown().await;
}

/// A write of issue `id` titled `title` in `app`'s resource type `kind`, as the verb `verb`, made
/// by the app outside any invocation: what a view's own press writes.
fn viewed_put(kind: &str, id: &str, title: &str, verb: &str) -> Command {
    Command::Put {
        kind: kind.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
        },
        stamp: Stamp {
            verb: verb.to_owned(),
            // What the app claims is replaced by who the view was opened for.
            invoker: "the app's own claim".to_owned(),
            invocation: uuid::Uuid::new_v4().to_string(),
            ..Stamp::default()
        },
    }
}

/// Who made each version of `id` of `kind` in `app`, oldest first, as the device reads them.
async fn made_by(
    workspaces: &Workspaces,
    device: &ResourceRequestContext,
    (app, kind, id): (&str, &str, &str),
) -> Vec<Option<tilo_actions::Actor>> {
    let device =
        ResourceRequestContext::authenticated(app, device.workspace_id(), device.caller().clone());
    let Reply::Versions(versions) = workspaces
        .resource_command(
            &device,
            Command::Versions {
                kind: kind.into(),
                id: id.into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("a resource's versions");
    };
    versions
        .iter()
        .map(|version| version.stamp.as_ref().and_then(Stamp::invoked_by))
        .collect()
}

/// A view the device opened for a caller of its dispatch writes as that caller and reads as
/// them, whatever the app claims and although the connection is the device's own: an editor's
/// write is the editor's, a visitor's is the visitor's where the workspace's entry opens that verb
/// to any key, and a write the caller may not make (a viewer's, a visitor's other verb) is
/// refused before the store keeps anything of it. A visitor reads none of the members'
/// resources through the view.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "one flow through an editor, a viewer and a visitor at the same view, which is what it proves"
)]
async fn a_views_writes_are_its_callers_and_limited_to_what_they_may_do() {
    use tilo_actions::remote::Caller;
    use tilo_server_protocol::workspaces::Role;
    const ENTRY: &str = tilo_action_policy::VISITOR_ENTRY_APP;
    const REQUEST: &str = "entry.request";
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    declare_app(&server, ENTRY, ENTRY_SURFACE.as_bytes());
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    for app in [APP, ENTRY] {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.into(),
            cluster_id: workspace.clone(),
            document: String::new(),
        };
        let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    }
    let (editor_key, editor) = super::super::tests::guest_device();
    let (viewer_key, viewer) = super::super::tests::guest_device();
    for (seed, (device_key, device), role) in [
        (61, (&editor_key, &editor), Role::Editor),
        (62, (&viewer_key, &viewer), Role::Viewer),
    ] {
        super::super::fixtures::entry_join_as(
            &workspaces,
            &cert,
            &workspace,
            (device_key, device),
            seed,
            role,
        )
        .await;
    }
    // The entry keeps requests: it opens leaving one to any key.
    workspaces
        .list_invitation_only(&workspace, &cert, true)
        .await
        .unwrap();
    workspaces
        .list_wait_list_max(&workspace, &cert, 8)
        .await
        .unwrap();
    let device = context(&workspace, &cert);
    let view = |app: &str, caller: Caller| {
        ResourceRequestContext::authenticated(app, &workspace, device.caller().clone())
            .opened_for(Some(caller.in_workspace(&workspace)))
    };
    let member = |cert: &tilo_user_identity::DeviceCert| {
        Caller::authenticated(cert.user_public_key, cert.device_public_key)
    };

    // An editor's press in the view is the editor's.
    workspaces
        .resource_command(
            &view(APP, member(&editor)),
            viewed_put(KIND, "i-1", "Edited", "$create"),
        )
        .await
        .unwrap();
    let editor_is = tilo_actions::Actor::Member(tilo_actions::MemberId::person(hex::encode(
        editor.user_public_key,
    )));
    assert_eq!(
        made_by(&workspaces, &device, (APP, KIND, "i-1")).await,
        vec![Some(editor_is)],
        "the editor made it, not the device whose connection carried it"
    );

    // A viewer may change nothing: the write is refused and nothing of it is kept.
    let refused = workspaces
        .resource_command(
            &view(APP, member(&viewer)),
            viewed_put(KIND, "i-2", "Viewed", "$create"),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("may not $create"), "{refused}");
    assert!(
        title_through_service(&workspaces, &device, "i-2")
            .await
            .is_none()
    );

    // A visitor leaves a request at the entry: theirs, and only what the entry opens to them.
    let visitor = [9; 32];
    workspaces
        .resource_command(
            &view(ENTRY, Caller::visitor(visitor)),
            viewed_put(REQUEST, "r-1", "ada@example.com", "join"),
        )
        .await
        .unwrap();
    assert_eq!(
        made_by(&workspaces, &device, (ENTRY, REQUEST, "r-1")).await,
        vec![Some(tilo_actions::Actor::Visitor(hex::encode(visitor)))],
        "the visitor left it"
    );
    let refused = workspaces
        .resource_command(
            &view(ENTRY, Caller::visitor(visitor)),
            viewed_put(REQUEST, "r-2", "everyone", "remove"),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("may not remove"), "{refused}");
    // Nor does a visitor read the members' resources through the view.
    assert!(
        title_through_service(&workspaces, &view(APP, Caller::visitor(visitor)), "i-1")
            .await
            .is_none(),
        "a visitor reads no member's issue"
    );
    assert_eq!(
        title_through_service(&workspaces, &view(APP, member(&editor)), "i-1").await,
        Some(Value::Text("Edited".into())),
        "the editor reads what every member reads"
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
    let (member_key, member) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let mut writer = Writer::for_member(&workspaces, &workspace, member_key, &member).await;

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

/// A workspace keeps its chain as parts of its store: 26 documents, a thousand
/// resources each. The process is killed rather than stopped, so nothing is kept as it
/// goes: a start reads each document's snapshot of the index, not the document, and a
/// read is answered from that index.
#[test]
fn a_store_a_hard_stop_left_answers_from_its_index() {
    /// Parts of the chain.
    const PARTS: usize = 26;
    /// Resources each part holds.
    const EACH: usize = 1_000;
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let mut resources = super::Resources::open(&file, id);
    let stamp = Stamp::default();
    for part in 0..PARTS {
        let kind = format!("issue{part}");
        let before = resources.heads(&kind);
        {
            let mut store = resources.store();
            for index in 0..EACH {
                let resource = Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([(
                        "title".to_owned(),
                        Cell::One(Value::Text(format!("Issue {part}-{index}"))),
                    )]),
                };
                store
                    .put(&kind, &format!("i{part}-{index}"), resource, &stamp)
                    .unwrap();
            }
        }
        let doc = format!("*/{kind}#0");
        resources.kept(&doc, &before).unwrap();
    }
    // A hard stop: the process is killed, so nothing writes the store out.
    std::mem::forget(resources);

    let started = std::time::Instant::now();
    let reopened = super::Resources::open(&file, id);
    let opened = started.elapsed();
    let store = reopened.store();
    assert_eq!(
        store.in_memory(),
        0,
        "a start reads the index's snapshots, not the store's documents"
    );
    assert_eq!(
        store.count("issue13", &[], &|_| true),
        EACH,
        "the index answers for a part of the chain"
    );
    assert!(
        store
            .get("issue13", "i13-7", &|_| true)
            .is_some_and(|held| held.scalar("title") == Value::Text("Issue 13-7".into())),
        "a resource of a part reads from the index"
    );
    assert_eq!(store.in_memory(), 0, "reading it opened no document");
    drop(store);
    assert!(
        opened < std::time::Duration::from_secs(2),
        "the start took {opened:?}"
    );
}

/// A query an app holds open is sent its page again when a write changes what it shows, nothing
/// when a write leaves that as it was, and is let go once the app drops it.
#[tokio::test]
async fn an_open_query_is_sent_each_page_a_write_makes_different() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    let context = context(&workspace, &cert);
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    let put = |title: &str, invocation: &str| Command::Put {
        kind: KIND.to_owned(),
        id: "i-1".to_owned(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
        },
        stamp: Stamp {
            verb: "$create".to_owned(),
            invocation: invocation.to_owned(),
            ..Stamp::default()
        },
    };
    let titles = |page: tilo_resource_store::Page| -> Vec<Value> {
        page.resources
            .iter()
            .map(|(_, held)| held.scalar("title"))
            .collect()
    };

    let mut pages = workspaces
        .open_resources(&context, Query::of(KIND, 10))
        .await
        .unwrap();
    assert!(titles(pages.try_recv().expect("the first page")).is_empty());
    workspaces
        .resource_command(&context, put("First", "i1"))
        .await
        .unwrap();
    assert_eq!(
        titles(
            pages
                .try_recv()
                .expect("a write sends the page it changed at once")
        ),
        [Value::Text("First".into())]
    );
    workspaces
        .resource_command(&context, put("First", "i2"))
        .await
        .unwrap();
    assert!(
        pages.try_recv().is_err(),
        "a write that changes nothing shown sends nothing"
    );
    workspaces
        .resource_command(&context, put("Renamed", "i3"))
        .await
        .unwrap();
    assert_eq!(
        titles(pages.try_recv().expect("the next page")),
        [Value::Text("Renamed".into())]
    );

    drop(pages);
    workspaces
        .resource_command(&context, put("Again", "i4"))
        .await
        .unwrap();
    let governed = workspaces.workspace(&workspace).await.unwrap();
    assert!(
        governed.lock().await.resources.opened.is_empty(),
        "a query its app let go of is no longer read"
    );
}

/// An app keeps a record whose field names a file's bytes as a resource of the workspace's store:
/// the document names the bytes by reference, and the server reads the document back from the
/// store — the replica does not hold a document the store placed.
#[tokio::test]
async fn a_members_store_document_reads_back_the_content_its_cells_name() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Photos").await.unwrap();
    let workspace = created.workspace;
    let (member_key, member) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let mut writer = Writer::for_member(&workspaces, &workspace, member_key, &member).await;
    let hash = "ab".repeat(32);
    let reference = format!("blake3:{hash}:4096");
    let document = writer
        .put_content(&workspaces, &workspace, "p-1", &reference)
        .await;
    assert!(
        collab_types::is_store_document(&document),
        "the store placed the record: {document}"
    );
    let read = workspaces
        .resource_document(&workspace, APP, &document)
        .await
        .unwrap()
        .expect("the record a member wrote reads back from the store");
    assert!(
        String::from_utf8_lossy(&read).contains(&hash),
        "the document names the picture's bytes, which the workspace keeps for it"
    );
    server.shutdown().await;
}

/// Simulate shutdown after indexing a signed checkpoint but before the governor
/// followed it.
async fn index_retained_checkpoint(
    workspaces: &Workspaces,
    workspace: &str,
    workspace_id: tilo_server_protocol::WorkspaceId,
    member_key: SigningKey,
    member: &tilo_user_identity::DeviceCert,
) -> String {
    let author = member_key.verifying_key().to_bytes();
    let mut writer = Writer::for_member(workspaces, workspace, member_key, member).await;
    let (document, bodies) = writer.draft(
        workspace,
        "i-1",
        BTreeMap::from([("title".into(), Cell::One(Value::Text("First".into())))]),
    );
    let [body]: [Vec<u8>; 1] = bodies.try_into().unwrap();
    let message = NeighbourMsg::decode(&body).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &message else {
        panic!("a store document checkpoint");
    };
    let (_, retention) = neighbour_wire::retention(&message, false).unwrap();
    assert_eq!(retention, neighbour_wire::Retention::Checkpoint);
    {
        let state = workspaces.workspace(workspace).await.unwrap();
        let mut governed = state.lock().await;
        assert!(
            governed
                .take_resources(author, (APP, &document), envelope, None)
                .await
                .unwrap()
        );
        assert!(!governed.peer().holds_document(APP, &document));
    }

    let device = workspaces.member_identity().unwrap();
    let context = context(workspace, &device.cert);
    let records = &workspaces.inner.records;
    let before = records.retained(workspace_id).await.unwrap();
    let mut watches = workspaces.document_writes();
    let refused = workspaces
        .resource_command(&context, put_signed_issue("No signed base"))
        .await
        .unwrap_err();
    assert!(refused.contains("has not opened this resource's signed history"));
    let refused = workspaces
        .resource_command(&context, Command::Batch(vec![put_signed_issue("Unsigned")]))
        .await
        .unwrap_err();
    assert!(refused.contains("has not opened this resource's signed history"));
    assert_eq!(records.retained(workspace_id).await.unwrap(), before);
    assert!(watches.try_recv().is_err());
    assert_eq!(
        title_through_service(workspaces, &context, "i-1").await,
        Some(Value::Text("First".into()))
    );

    workspaces
        .inner
        .records
        .carry(
            workspace_id,
            author,
            &crate::workspace_documents::HostCommit { body },
        )
        .unwrap();
    document
}

fn signed_issue(title: &str) -> Resource {
    Resource {
        readers: "*".into(),
        parent: None,
        order: None,
        fields: BTreeMap::from([("title".into(), Cell::One(Value::Text(title.into())))]),
    }
}

fn put_signed_issue(title: &str) -> Command {
    Command::Put {
        kind: KIND.into(),
        id: "i-1".into(),
        resource: signed_issue(title),
        stamp: create(),
    }
}

/// A write cancelled before the workspace admits it releases the workspace and
/// keeps nothing of it: no retained frame and no row.
async fn a_cancelled_write_keeps_nothing(
    workspaces: &Workspaces,
    workspace: &str,
    workspace_id: collab_types::WorkspaceId,
    context: &ResourceRequestContext,
) {
    let records = &workspaces.inner.records;
    let before = records.retained(workspace_id).await.unwrap();
    let state = workspaces.workspace(workspace).await.unwrap();
    // The workspace held, the write waits for it before admission: cancelling
    // it there keeps nothing.
    let held = state.lock().await;
    let cancelled = tokio::spawn({
        let workspaces = workspaces.clone();
        let context = context.clone();
        async move {
            workspaces
                .resource_command(&context, put_signed_issue("Cancelled"))
                .await
        }
    });
    // Let the write run until it waits for the workspace this test holds.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    drop(held);
    assert_eq!(records.retained(workspace_id).await.unwrap(), before);
    assert_eq!(
        title_through_service(workspaces, context, "i-1").await,
        None
    );
    drop(state);
}

#[tokio::test]
async fn a_first_resource_write_refused_by_retention_leaves_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let context = context(&workspace, &cert);
    let records = &workspaces.inner.records;
    let before = records.retained(created.workspace_id).await.unwrap();
    // Peer-state's admission refuses the first write: this device holds no keys
    // of the record's channel yet, so nothing is retained, nothing reads, and
    // whoever follows the workspace's records is not woken.
    let mut watches = workspaces.document_writes();
    let refused = workspaces
        .resource_command(&context, put_signed_issue("Refused"))
        .await;
    assert_eq!(refused.unwrap_err(), "this channel has no keys yet");
    assert_eq!(
        records.retained(created.workspace_id).await.unwrap(),
        before
    );
    let title = title_through_service(&workspaces, &context, "i-1").await;
    assert_eq!(
        title, None,
        "a first write without a durable frame is invisible"
    );
    assert!(
        watches.try_recv().is_err(),
        "a refused write wakes no follower"
    );
    // The channel's keys make the next writes admission's to take.
    workspaces
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();

    a_cancelled_write_keeps_nothing(&workspaces, &workspace, created.workspace_id, &context).await;

    let batch = Command::Batch(vec![
        put_signed_issue("First"),
        put_signed_issue("Second"),
        Command::Remove {
            kind: KIND.into(),
            id: "missing".into(),
            stamp: create(),
        },
    ]);
    let refused = workspaces
        .resource_command(&context, batch)
        .await
        .unwrap_err();
    assert!(refused.contains("missing"));
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("Second".into())),
        "the admitted prefix advances the same document twice"
    );
    let root = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let governed = state.lock().await;
        governed.resources.root.clone()
    };
    drop(workspaces);
    server.shutdown().await;

    // Model interruption after retention but before the resource projection reached disk.
    std::fs::remove_dir_all(root).unwrap();
    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert,
        signer: Arc::new(key),
    }));
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("Second".into())),
        "the signed checkpoint and delta rebuild the projection"
    );
    drop(workspaces);
    server.shutdown().await;
}

#[tokio::test]
async fn a_member_copy_retains_and_forwards_its_first_resource_write() {
    use super::super::fixtures::{device_cert, signed_pre_admission};

    let owner_dir = tempfile::tempdir().unwrap();
    let member_dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (owner_key, owner_cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let owner = test_host_holding(owner_dir.path(), &owner_key, &owner_cert, keys.clone()).await;
    let created = owner.workspaces().create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    owner
        .workspaces()
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[37; 32]),
        "member",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let invite = owner
        .workspaces()
        .pre_admit(
            &owner_cert,
            super::super::PreAdmitRequest {
                request_id: [37; 32],
                grant: signed_pre_admission(&workspace, b"resource-writer"),
                recipient_user_public_key: Some(cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let member = test_host_holding(member_dir.path(), &key, &cert, keys).await;
    member
        .workspaces()
        .set_member_device(Arc::new(super::super::MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key),
        }));
    let invite = tilo_peer_state::join::encode_invitation_text(&invite.signed_invitation);
    assert_eq!(
        member.workspaces().join_member(&invite).await.unwrap(),
        created.workspace_id
    );
    let context = context(&workspace, &cert);
    member
        .workspaces()
        .resource_command(&context, put_signed_issue("Member"))
        .await
        .unwrap();
    let retained = member
        .workspace_documents()
        .retained(created.workspace_id)
        .await
        .unwrap();
    assert!(retained.iter().any(|frame| {
        frame.author == cert.device_public_key
            && matches!(NeighbourMsg::decode(&frame.body), Ok(NeighbourMsg::AppDocument {
                app_id, envelope, ..
            }) if app_id == APP && envelope.is_whole_resource())
    }));
    let expected = Some(Value::Text("Member".into()));
    assert_eq!(
        title_through_service(member.workspaces(), &context, "i-1").await,
        expected
    );
    tilo_test_env::within("the serving device projects the member's resource", async {
        while title(owner.workspaces(), &workspace, "i-1").await != expected {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    member.shutdown().await;
    owner.shutdown().await;
}

/// A device that joins a workspace reads the resources written there before it arrived, though
/// their writes reach its copy before the group's keys reach the device: once the keys do, the
/// copy opens what it kept, with nothing written since. A resource sealed to its author alone
/// reaches the copy too, and stays shut: no member's keys open it.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the write before the join, the join, and the keys that come after both"
)]
async fn a_member_copy_reads_resources_that_reached_it_before_its_keys() {
    use super::super::fixtures::{device_cert, signed_pre_admission};

    let owner_dir = tempfile::tempdir().unwrap();
    let member_dir = tempfile::tempdir().unwrap();
    let owner_keys = Arc::new(TestGroupKeys::default());
    let (owner_key, owner_cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let owner = test_host_holding(
        owner_dir.path(),
        &owner_key,
        &owner_cert,
        Arc::clone(&owner_keys),
    )
    .await;
    owner
        .workspaces()
        .set_member_device(Arc::new(super::super::MemberDevice {
            cert: owner_cert.clone(),
            signer: Arc::new(owner_key.clone()),
        }));
    let created = owner.workspaces().create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    owner
        .workspaces()
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    owner
        .workspaces()
        .resource_command(
            &context(&workspace, &owner_cert),
            put_signed_issue("Written before"),
        )
        .await
        .unwrap();
    // A draft of the owner's that only they read: its own document, sealed to their group.
    let author = hex::encode(owner_cert.user_public_key);
    owner
        .workspaces()
        .resource_command(
            &context(&workspace, &owner_cert),
            Command::Put {
                kind: KIND.into(),
                id: "draft-1".into(),
                resource: Resource {
                    readers: author.clone(),
                    ..signed_issue("Only mine")
                },
                stamp: create(),
            },
        )
        .await
        .unwrap();
    let private = |document_id: &str| {
        collab_types::store_document_readers(document_id)
            == Some(collab_types::reader_set_name(&author).as_str())
    };

    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[41; 32]),
        "member",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let invite = owner
        .workspaces()
        .pre_admit(
            &owner_cert,
            super::super::PreAdmitRequest {
                request_id: [41; 32],
                grant: signed_pre_admission(&workspace, b"joins-later"),
                recipient_user_public_key: Some(cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    // The joining device holds none of the group's keys until the group lets it in.
    let member_keys = Arc::new(TestGroupKeys::admitted_only());
    let member = test_host_holding(member_dir.path(), &key, &cert, Arc::clone(&member_keys)).await;
    member
        .workspaces()
        .set_member_device(Arc::new(super::super::MemberDevice {
            cert,
            signer: Arc::new(key),
        }));
    // What the device's apps are handed of it, from the start.
    let mut shown = member.workspaces().admitted_writes();
    // Whoever follows the copy's records hears of each write that changes one.
    let mut woke = member.workspaces().document_writes();
    let invite = tilo_peer_state::join::encode_invitation_text(&invite.signed_invitation);
    member.workspaces().join_member(&invite).await.unwrap();
    let kept = |shared: bool| {
        let documents = member.workspace_documents();
        async move {
            documents
                .retained(created.workspace_id)
                .await
                .unwrap()
                .iter()
                .any(|frame| {
                    matches!(NeighbourMsg::decode(&frame.body), Ok(NeighbourMsg::AppDocument {
                    app_id, document_id, ..
                }) if app_id == APP
                    && collab_types::is_store_document(&document_id)
                    && private(&document_id) != shared)
                })
        }
    };
    tilo_test_env::within(
        "the copy keeps the issue and the draft it cannot open yet",
        async {
            while !(kept(true).await && kept(false).await) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    assert_eq!(
        title(member.workspaces(), &workspace, "i-1").await,
        None,
        "the copy reads no sealed issue before the device holds the group's keys"
    );

    // The keys make the kept write readable: the copy tells its followers so again.
    while woke.try_recv().is_ok() {}
    member_keys.hand_every_member_of(&owner_keys, &workspace);
    let expected = Some(Value::Text("Written before".into()));
    tilo_test_env::within("the copy reads the issue once the keys are here", async {
        while title(member.workspaces(), &workspace, "i-1").await != expected {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    // The member's device holds the owner's draft, sealed, and opens none of it.
    assert!(kept(false).await, "the copy keeps the sealed draft");
    assert!(
        holds(owner.workspaces(), &workspace, "draft-1").await,
        "the owner reads their draft"
    );
    assert!(
        !holds(member.workspaces(), &workspace, "draft-1").await,
        "another member's device holds the draft and cannot open it"
    );
    tilo_test_env::within(
        "the copy tells its followers the record it restored",
        async { while woke.recv().await.is_ok_and(|write| write.app != APP) {} },
    )
    .await;
    // Written long before the apps here could see it, the issue reaches them as history, which
    // no clock window applies to, never as a write made just now.
    let mut handed = 0;
    while let Ok(write) = shown.try_recv() {
        if matches!(write.message.as_ref(), NeighbourMsg::AppDocument { app_id, .. } if app_id == APP)
        {
            assert!(
                write.history,
                "a write from before the device joined is history"
            );
            handed += 1;
        }
    }
    assert!(handed > 0, "the apps here are handed the issue");
    member.shutdown().await;
    owner.shutdown().await;
}

#[tokio::test]
async fn retained_store_checkpoint_restores_signed_history_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let (member_key, member) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let document = index_retained_checkpoint(
        &workspaces,
        &workspace,
        created.workspace_id,
        member_key,
        &member,
    )
    .await;
    drop(workspaces);
    server.shutdown().await;

    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key),
    }));
    let history = workspaces
        .document_history(&workspace, APP, &document)
        .await
        .unwrap()
        .expect("the writer's checkpoint restores the signed history");
    assert_eq!(history.writes.len(), 1);
    assert_eq!(history.writes[0].author, member.device_public_key);
    assert!(history.writes[0].mutation.is_ok());

    let context = context(&workspace, &cert);
    let records = &workspaces.inner.records;
    let before_cancel = records.retained(created.workspace_id).await.unwrap();
    let mut watches = workspaces.document_writes();
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut governed = state.lock().await;
        let device = workspaces.member_identity().unwrap();
        let draft = governed
            .store_with_surfaces(&workspaces.inner.host.surfaces)
            .draft_field_update(
                (KIND, "i-1"),
                &signed_issue("Cancelled"),
                &create(),
                device.device(),
                None,
                &|_: &tilo_resource_store::Proposal<'_>| true,
            )
            .unwrap()
            .unwrap();
        let refused = workspaces
            .commit_resource_update(&mut governed, &device, APP, &draft, |_| false)
            .await;
        assert_eq!(
            refused.unwrap_err(),
            "the resource writer is no longer authorized"
        );
    }
    assert!(
        watches.try_recv().is_err(),
        "a cancelled write wakes nobody"
    );
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("First".into()))
    );
    assert_eq!(
        records.retained(created.workspace_id).await.unwrap(),
        before_cancel,
        "a cancelled write leaves retained history untouched"
    );

    workspaces
        .resource_command(&context, put_signed_issue("Edited"))
        .await
        .unwrap();
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("Edited".into()))
    );
    drop(workspaces);
    server.shutdown().await;
}

/// A write this device cannot seal, its readers' keys not being here yet, is refused, and
/// nothing of it stays readable: the app never shows what no member will receive. Once the
/// keys are here, the same write goes through.
#[tokio::test]
async fn a_write_this_device_cannot_send_is_not_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let created = workspaces.create("Tasks").await.unwrap();
    let workspace = created.workspace;
    let context = context(&workspace, &cert);
    let put = |title: &str, invocation: &str| Command::Put {
        kind: KIND.to_owned(),
        id: "i-1".to_owned(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
        },
        stamp: Stamp {
            verb: "$create".to_owned(),
            invoker: "ada".to_owned(),
            invocation: invocation.to_owned(),
            ..Stamp::default()
        },
    };

    let mut watches = workspaces.document_writes();
    let error = workspaces
        .resource_command(&context, put("Unsent", "i1"))
        .await
        .expect_err("no keys of the readers are here yet");
    assert_eq!(error, "this channel has no keys yet");
    assert!(watches.try_recv().is_err());
    assert!(
        title_through_service(&workspaces, &context, "i-1")
            .await
            .is_none(),
        "the refused write is not read back"
    );
    drop(workspaces);
    server.shutdown().await;

    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    workspaces.set_member_device(Arc::new(super::super::MemberDevice {
        cert,
        signer: Arc::new(key),
    }));
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        None,
        "a refused first write must not leave a readable row on disk"
    );
    let retained = workspaces
        .inner
        .records
        .retained(created.workspace_id)
        .await
        .unwrap();
    assert!(
        !retained.iter().any(|frame| matches!(
            NeighbourMsg::decode(&frame.body),
            Ok(NeighbourMsg::AppDocument { app_id, document_id, .. })
                if app_id == APP && collab_types::is_store_document(&document_id)
        )),
        "the refusal must not leave a retained frame"
    );

    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    workspaces
        .resource_command(&context, put("Sent", "i2"))
        .await
        .unwrap();
    assert_eq!(
        title_through_service(&workspaces, &context, "i-1").await,
        Some(Value::Text("Sent".into())),
        "with the keys here, the write goes through"
    );
    drop(workspaces);
    server.shutdown().await;
}
