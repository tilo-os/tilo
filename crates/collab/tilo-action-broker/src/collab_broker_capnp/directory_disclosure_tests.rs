#![expect(
    clippy::future_not_send,
    reason = "real Cap'n Proto connections and the broker registry run on the test LocalSet"
)]
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use tilo_actions::{ActionContract, OperationKind};

struct Person {
    root: SigningKey,
    device: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
}
impl Person {
    fn new(root_seed: u8, device_seed: u8) -> Self {
        let root = SigningKey::from_bytes(&[root_seed; 32]);
        let device = SigningKey::from_bytes(&[device_seed; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Directory regression device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        Self {
            root,
            device,
            certificate,
        }
    }
    fn id(&self) -> String {
        principal_id_for_root(self.root.verifying_key().to_bytes())
    }
    fn presentation(&self, room: &str) -> Vec<u8> {
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.into(),
            device_certificate: serde_json::to_vec(&self.certificate).unwrap(),
            proof: self
                .device
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

struct Connection {
    channel: channel_handle::Client,
    driver: tokio::task::JoinHandle<Result<(), capnp::Error>>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl Connection {
    async fn join(
        state: &Arc<CapnpBrokerState>,
        directories: &Rc<LocalActionDirectoryRegistry>,
        route: &CrdtRoute,
        person: &Person,
        claimed_participant: &str,
    ) -> Self {
        let peer = state.next_peer_id();
        let service: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
            worker: None,
            app_worker: None,
            state: state.clone(),
            directories: directories.clone(),
            peer_id: peer,
            lease: Rc::new(ConnectionLease {
                local_media: None,
                state: state.clone(),
                directories: directories.clone(),
                peer_id: peer,
            }),
            holds: route_holds::ConnectionHolds::default(),
        });
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        let driver =
            tokio::task::spawn_local(runtime_portal::serve_connected_io(server, service.client));
        let client: collab_service::Client = runtime_portal::CapnpClient::from_connected_io(
            client,
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap();
        let mut join = client.join_channel_request();
        join.get().set_channel_id(route.channel_key());
        join.get()
            .set_identity(&person.presentation(&route.cluster_id));
        join.get().set_participant_kind("person");
        join.get().set_participant_handle(claimed_participant);
        let response = join.send().promise.await.unwrap();
        Self {
            channel: response.get().unwrap().get_handle().unwrap(),
            driver,
        }
    }
    async fn publish(&self, app: &str, rows: Vec<runtime_collab::ActionDirectoryRow>) {
        let mut publish = self.channel.publish_action_directory_request();
        publish.get().set_app_id(app);
        publish
            .get()
            .set_directory(capnp_rpc::new_client(CachedDirectory(rows)));
        publish.send().promise.await.unwrap();
    }
    async fn query(&self, peer: u64, needle: &str) -> runtime_collab::ActionDirectoryPage {
        self.query_page(peer, needle, "", &[]).await.unwrap()
    }
    async fn query_page(
        &self,
        peer: u64,
        needle: &str,
        revision: &str,
        cursor: &[u8],
    ) -> Result<runtime_collab::ActionDirectoryPage, capnp::Error> {
        let mut request = self.channel.query_action_directory_request();
        let mut query = request.get().init_query();
        query.set_peer_id(peer);
        query.set_needle(needle);
        query.set_revision(revision);
        query.set_cursor(cursor);
        query.set_max_results(32);
        query.set_max_bytes(CapnpBrokerState::DIRECTORY_MAX_BYTES);
        query.set_max_work(128);
        query.set_max_depth(4);
        let reply = request.send().promise.await?;
        runtime_collab::decode_action_directory_page(reply.get()?.get_page()?)
    }
}

struct CachedDirectory(Vec<runtime_collab::ActionDirectoryRow>);
impl action_directory::Server for CachedDirectory {
    fn query(
        self: Rc<Self>,
        params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let result = (|| {
            let request = params.get()?;
            let query = request.get_query()?;
            let needle = query.get_needle()?.to_str()?;
            // This models a publisher's owner-scoped cached SDK snapshot. It
            // deliberately does not know which person the broker is serving.
            let paged = needle == "paged";
            let tail = !query.get_cursor()?.is_empty();
            let rows = self
                .0
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    if paged {
                        if tail { *index >= 4 } else { *index < 4 }
                    } else {
                        row.resource_label.contains(needle)
                    }
                })
                .map(|(_, row)| row.clone())
                .collect();
            runtime_collab::write_action_directory_page(
                results.get().init_page(),
                &runtime_collab::ActionDirectoryPage {
                    revision: "cached-r1".into(),
                    rows,
                    next_cursor: if paged && !tail {
                        b"Secret account label:offset:4".to_vec()
                    } else {
                        Vec::new()
                    },
                    exhausted: !paged || tail,
                    stale_cursor: false,
                    budget_exhausted: false,
                },
            )
        })();
        Promise::from(result)
    }
    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        results.get().init_resolution().set_not_found(());
        Promise::ok(())
    }
}

fn rows() -> Vec<runtime_collab::ActionDirectoryRow> {
    let mut rows = Vec::new();
    for (id, label, kind, participant) in [
        (
            "account:secret",
            "Secret account label",
            OperationKind::View,
            false,
        ),
        (
            "document:public",
            "Public document",
            OperationKind::Document,
            false,
        ),
        (
            "navigation:public",
            "Public navigation",
            OperationKind::View,
            true,
        ),
    ] {
        let mut contract = ActionContract::view();
        if participant {
            contract
                .tags
                .push(tilo_actions::PARTICIPANT_VIEW_TAG.into());
        }
        let descriptor = runtime_collab::ExportedAction {
            semantic_identity: None,
            resource: id.into(),
            action: "read".into(),
            resource_kind: "test.record".into(),
            resource_label: label.into(),
            action_label: format!("Read {label}"),
            description: Some(label.into()),
            operation_kind: kind,
            params: Vec::new(),
            contract: contract.clone(),
            input: runtime_collab::ExportedInput::None,
            view: None,
        };
        let row = runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource: id.into(),
            resource_kind: "test.record".into(),
            resource_label: label.into(),
            presentation: Some(tilo_actions::ResourcePresentation {
                detail: Some(label.into()),
                badge: None,
                preview: None,
            }),
            action: Some("read".into()),
            descriptor: Some(descriptor),
            has_children: false,
            visibility: Some(runtime_collab::RowVisibility {
                action: "read".into(),
                contract,
                governing_kind: Some(kind),
            }),
        };
        rows.push(row.clone());
        rows.push(runtime_collab::ActionDirectoryRow {
            action: None,
            descriptor: None,
            has_children: true,
            ..row
        });
    }
    rows
}

fn assert_disclosure(rows: &[runtime_collab::ActionDirectoryRow], private: bool, needle: &str) {
    assert_eq!(
        rows.iter()
            .filter(|row| row.resource == "account:secret")
            .count(),
        if private { 2 } else { 0 }
    );
    if needle.is_empty() {
        assert_eq!(
            rows.iter()
                .filter(|row| row.resource == "document:public")
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.resource == "navigation:public")
                .count(),
            2
        );
    }
    for row in rows {
        assert!(row.visibility.as_ref().unwrap().governing_kind.is_some());
    }
    if !private {
        assert!(!format!("{rows:?}").contains("Secret"));
    }
}

async fn bridge_query(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    route: &CrdtRoute,
    person: &Person,
    peer: u64,
    needle: &str,
) -> tilo_actions::remote::CataloguePage {
    use tilo_actions::remote::{Caller, CatalogueQuery, PendingPoll};
    let caller = Caller::local_person(person.id())
        .in_room(&route.cluster_id)
        .in_new_execution();
    let mut pending = state.action_query(
        &caller,
        &CatalogueQuery {
            provider: Some(peer.to_string()),
            needle: needle.into(),
            max_results: 32,
            ..CatalogueQuery::default()
        },
    );
    tilo_test_env::within("the directory disclosure", async {
        loop {
            state.drain_action_directory_bridge(directories);
            match pending.poll_ready() {
                PendingPoll::Ready(page) => break page.unwrap(),
                PendingPoll::Abandoned => panic!("bridge query abandoned"),
                PendingPoll::Waiting => tokio::task::yield_now().await,
            }
        }
    })
    .await
}

struct Fixture {
    owner: Person,
    editor: Person,
    viewer: Person,
    other_device: Person,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    route: CrdtRoute,
    publisher: Connection,
    peer: u64,
    _fanout: Arc<FanoutState>,
}
impl Fixture {
    async fn new() -> Self {
        let owner = Person::new(101, 102);
        let editor = Person::new(103, 104);
        let viewer = Person::new(105, 106);
        let other_device = Person::new(101, 107);
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(71),
            "Directory disclosure room",
        );
        workspace.set_owner(owner.id());
        workspace.admit(editor.id(), CapRole::Editor);
        workspace.admit(viewer.id(), CapRole::Viewer);
        let route = CrdtRoute::record(
            "so.tilo.directory-test",
            workspace.room().to_string(),
            "record",
        );
        let mut tree = crate::test_support::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        tree.set_active(crate::test_support::WorkspaceId(71));
        let fanout = Arc::new(FanoutState::new());
        let devices = [&owner, &editor, &viewer, &other_device]
            .map(|p| p.certificate.device_public_key)
            .to_vec();
        let (state, rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            Some(Arc::new(devices)),
        );
        let directories = Rc::new(LocalActionDirectoryRegistry::new(rx));
        let publisher =
            Connection::join(&state, &directories, &route, &owner, "display-owner").await;
        publisher.publish(&route.app_id, rows()).await;
        let peer = *directories.directories.borrow().keys().next().unwrap();
        assert_eq!(
            directories
                .registered(peer, &route.cluster_id)
                .unwrap()
                .publishing_person,
            Some(owner.id())
        );
        Self {
            owner,
            editor,
            viewer,
            other_device,
            state,
            directories,
            route,
            publisher,
            peer,
            _fanout: fanout,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cached_private_metadata_is_filtered_on_wire_and_bridge_for_authenticated_people() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let Fixture {
                owner,
                editor,
                viewer,
                other_device,
                state,
                directories,
                route,
                publisher,
                peer,
                _fanout,
            } = Fixture::new().await;
            for (person, private) in [
                (&owner, true),
                (&editor, false),
                (&viewer, false),
                (&other_device, true),
            ] {
                // Every foreign person claims the owner's participant name. The
                // certified root, not this wire label, must decide disclosure.
                let client =
                    Connection::join(&state, &directories, &route, person, &owner.id()).await;
                for needle in ["", "Secret"] {
                    let page = client.query(peer, needle).await;
                    assert_disclosure(&page.rows, private, needle);
                    let page =
                        bridge_query(&state, &directories, &route, person, peer, needle).await;
                    assert_eq!(
                        page.rows.len(),
                        if private {
                            if needle.is_empty() { 6 } else { 2 }
                        } else if needle.is_empty() {
                            4
                        } else {
                            0
                        }
                    );
                    if !private {
                        assert!(!serde_json::to_string(&page).unwrap().contains("Secret"));
                    }
                }
            }
            // Even the room's Owner cannot inspect another person's private view.
            let foreign_publisher =
                Connection::join(&state, &directories, &route, &editor, &owner.id()).await;
            foreign_publisher.publish(&route.app_id, rows()).await;
            let foreign_peer = *directories
                .directories
                .borrow()
                .iter()
                .find(|(_, entry)| entry.publishing_person == Some(editor.id()))
                .unwrap()
                .0;
            assert_disclosure(&publisher.query(foreign_peer, "").await.rows, false, "");
            // Legacy registrations do not acquire the room owner's identity.
            directories
                .directories
                .borrow_mut()
                .get_mut(&peer)
                .unwrap()
                .publishing_person = None;
            assert_disclosure(&publisher.query(peer, "").await.rows, false, "");
        })
        .await;
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct FrameworkReadAction;
impl tilo_actions::Action for FrameworkReadAction {
    fn apply(&self, _: &mut automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
        Ok(())
    }
}
struct FrameworkProjection;
#[cfg(test)]
impl tilo_resource::Resource for FrameworkProjection {
    const KIND: &'static str = "test.framework-projection";
    const LABEL: &'static str = "Public document";
    const READABLE: bool = true;
    type State = ();
    type Action = FrameworkReadAction;
    fn read(_: &tilo_resource::Doc) -> Result<(), tilo_resource::ModelError> {
        Ok(())
    }
    fn children((): &()) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new("public-child", "text", "Public child"),
            tilo_actions::Resource::new("private-child", "account", "Secret account").with_action(
                tilo_actions::ActionSpec::operation(
                    "render",
                    "Private view",
                    OperationKind::View,
                    "test.private-view",
                    &FrameworkReadAction,
                )
                .unwrap(),
            ),
        ]
    }
    fn document_child((): &(), id: &str) -> bool {
        id == "public-child"
    }
    fn read_content(
        (): &(),
        _: &tilo_resource::Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if item.is_some_and(|id| id != "public-child") {
            return Err("not part of the document".into());
        }
        Ok(serde_json::json!({"text": "Public content"}))
    }
}
struct FrameworkProjectionApp;
impl app_framework::framework::ResourceApp for FrameworkProjectionApp {
    type Resource = FrameworkProjection;
    const ID: &'static str = "so.tilo.test-framework-projection";
    const TITLE: &'static str = "Public document";
    fn view(_: &mut app_framework::framework::ViewCx<FrameworkProjection>) -> ui::widget::Element {
        Box::new(ui::widget::Text::new("Public document"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn framework_projection_keeps_private_child_metadata_off_signed_wire_and_bridge() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let fixture = Fixture::new().await;
            let mut app =
                app_framework::framework::testing::TestApp::<FrameworkProjectionApp>::new()
                    .unwrap();
            app.settle();
            let resources = app.harness().resources();
            let descriptors = tilo_actions::door::survey(&resources);
            let mut published = Vec::new();
            // Export the real framework's governing actions, not a hand-written
            // assertion about what a readable parent ought to publish.
            for id in ["test.framework-projection", "public-child", "private-child"] {
                let descriptor = descriptors
                    .iter()
                    .find(|d| d.address.resource == id && d.contract.reads_document())
                    .or_else(|| {
                        descriptors
                            .iter()
                            .find(|d| d.address.resource == id && d.address.action == "render")
                    })
                    .expect("framework governing action");
                let descriptor = runtime_collab::ExportedAction::from(descriptor);
                let visibility = runtime_collab::RowVisibility {
                    action: descriptor.action.clone(),
                    contract: descriptor.contract.clone(),
                    governing_kind: Some(descriptor.operation_kind),
                };
                let row = runtime_collab::ActionDirectoryRow {
                    parent_resource: if id == "test.framework-projection" {
                        String::new()
                    } else {
                        "test.framework-projection".into()
                    },
                    resource: descriptor.resource.clone(),
                    resource_kind: descriptor.resource_kind.clone(),
                    resource_label: descriptor.resource_label.clone(),
                    presentation: None,
                    action: Some(descriptor.action.clone()),
                    descriptor: Some(descriptor),
                    has_children: false,
                    visibility: Some(visibility),
                };
                published.push(runtime_collab::ActionDirectoryRow {
                    action: None,
                    descriptor: None,
                    ..row.clone()
                });
                published.push(row);
            }
            fixture
                .publisher
                .publish(&fixture.route.app_id, published)
                .await;
            let reader = Connection::join(
                &fixture.state,
                &fixture.directories,
                &fixture.route,
                &fixture.viewer,
                &fixture.owner.id(),
            )
            .await;
            let wire = reader.query(fixture.peer, "").await;
            assert_eq!(
                wire.rows.len(),
                4,
                "public root and child, with their read actions"
            );
            assert!(wire.rows.iter().any(|r| r.resource == "public-child"));
            assert!(!format!("{:?}", wire.rows).contains("Secret"));
            assert!(!wire.rows.iter().any(|r| r.resource == "private-child"));
            let bridge = bridge_query(
                &fixture.state,
                &fixture.directories,
                &fixture.route,
                &fixture.viewer,
                fixture.peer,
                "",
            )
            .await;
            assert_eq!(bridge.rows.len(), 4);
            assert!(!serde_json::to_string(&bridge).unwrap().contains("Secret"));
            drop(fixture);
        }))
        .await;
}

fn document_read_rows() -> Vec<runtime_collab::ActionDirectoryRow> {
    let mut published = rows();
    // Like a framework resource's `read`: a View that reads the
    // document, not the publishing person's private UI state.
    let mut document_reads = published[2..4].to_vec();
    for row in &mut document_reads {
        row.resource = "document:readable".into();
        let visibility = row.visibility.as_mut().unwrap();
        visibility.governing_kind = Some(OperationKind::View);
        visibility
            .contract
            .tags
            .push(tilo_actions::DOCUMENT_READ_TAG.into());
        if let Some(descriptor) = &mut row.descriptor {
            descriptor.resource = row.resource.clone();
            descriptor.operation_kind = OperationKind::View;
            descriptor.contract = visibility.contract.clone();
        }
    }
    // A legacy resource-only row must not turn public merely by
    // claiming the document-read tag without a known governing kind.
    let mut unknown = document_reads[1].clone();
    unknown.resource = "document:unknown-kind".into();
    unknown.resource_label = "Secret unknown-kind document".into();
    unknown.visibility.as_mut().unwrap().governing_kind = None;
    published.push(unknown.clone());
    unknown.parent_resource = unknown.resource.clone();
    unknown.resource = "document:unknown-kind/child".into();
    unknown.resource_label = "Secret unknown-kind child".into();
    unknown.has_children = false;
    published.push(unknown);
    // Private view metadata stays private at both the root and child
    // levels, even beside a public document-read view.
    let mut private_child = published[1].clone();
    private_child.parent_resource = "account:secret".into();
    private_child.resource = "account:secret/child".into();
    private_child.resource_label = "Secret private child".into();
    private_child.has_children = false;
    published.push(private_child);
    published.extend(document_reads);
    published
}

#[tokio::test(flavor = "current_thread")]
async fn document_read_views_are_disclosed_without_exposing_the_publishers_private_view() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(async {
            let fixture = Fixture::new().await;
            let published = document_read_rows();
            let expected_resources = [
                "document:public",
                "document:public",
                "navigation:public",
                "navigation:public",
                "document:readable",
                "document:readable",
            ];
            fixture
                .publisher
                .publish(&fixture.route.app_id, published)
                .await;
            let reader = Connection::join(
                &fixture.state,
                &fixture.directories,
                &fixture.route,
                &fixture.viewer,
                &fixture.owner.id(),
            )
            .await;
            for authenticated_publisher in [true, false] {
                if !authenticated_publisher {
                    // No publisher certificate must not turn the room owner
                    // or a caller-supplied participant label into its identity.
                    fixture
                        .directories
                        .directories
                        .borrow_mut()
                        .get_mut(&fixture.peer)
                        .unwrap()
                        .publishing_person = None;
                }
                let page = reader.query(fixture.peer, "").await;
                assert_eq!(
                    page.rows
                        .iter()
                        .map(|row| row.resource.as_str())
                        .collect::<Vec<_>>(),
                    expected_resources,
                    "wire must omit unknown-kind document rows and private root/child metadata"
                );
                assert!(!format!("{:?}", page.rows).contains("Secret"));
                assert_disclosure(&page.rows, false, "");
                assert_eq!(
                    page.rows
                        .iter()
                        .filter(|row| row.resource == "document:readable")
                        .count(),
                    2
                );
                let page = bridge_query(
                    &fixture.state,
                    &fixture.directories,
                    &fixture.route,
                    &fixture.viewer,
                    fixture.peer,
                    "",
                )
                .await;
                assert_eq!(
                    page.rows
                        .iter()
                        .map(|row| row.resource.id.as_str())
                        .collect::<Vec<_>>(),
                    expected_resources,
                    "bridge must omit unknown-kind document rows and private root/child metadata"
                );
                assert!(!serde_json::to_string(&page).unwrap().contains("Secret"));
            }
            drop(fixture);
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_directory_continuations_hide_source_metadata_and_bind_the_querying_person() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = Fixture::new().await;
            let editor = Connection::join(
                &fixture.state,
                &fixture.directories,
                &fixture.route,
                &fixture.editor,
                &fixture.owner.id(),
            )
            .await;
            let page = editor.query(fixture.peer, "paged").await;
            assert_eq!(
                page.rows.len(),
                2,
                "denied rows do not contribute to the returned count"
            );
            assert!(
                page.rows
                    .iter()
                    .all(|row| row.resource == "document:public")
            );
            assert_eq!(
                page.next_cursor.len(),
                16,
                "a fixed-size broker token, not an offset or id"
            );
            assert!(!page.next_cursor.windows(6).any(|word| word == b"Secret"));
            let tail = editor
                .query_page(fixture.peer, "paged", &page.revision, &page.next_cursor)
                .await
                .unwrap();
            assert_eq!(tail.rows.len(), 2);
            assert!(
                tail.rows
                    .iter()
                    .all(|row| row.resource == "navigation:public")
            );
            assert!(tail.exhausted && tail.next_cursor.is_empty());
            assert!(
                fixture
                    .publisher
                    .query_page(fixture.peer, "paged", &page.revision, &page.next_cursor)
                    .await
                    .is_err()
            );
            assert!(
                editor
                    .query_page(fixture.peer, "Secret", &page.revision, &page.next_cursor)
                    .await
                    .is_err()
            );
            assert!(
                editor
                    .query_page(
                        fixture.peer,
                        "paged",
                        &page.revision,
                        b"Secret account label:offset:4"
                    )
                    .await
                    .is_err()
            );
            drop(editor);
            drop(fixture);
        })
        .await;
}
