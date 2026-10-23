//! Cold native-editor records over the actual certified Iroh record route.
//! This proves profile-backed body transfer, not committed-owner construction.
//! The receiving view's read-only active resource is opened from its admitted body.

use super::*;
use crate::shell::record_hydration::{self, RecordAddress};
use automerge::ReadDoc as _;
use tilo_actions::Action as _;
use tilo_actions::remote::{ActionHostError, Caller, PendingPoll, RecordQuery};
use tilo_resource::{Demand, Resource as _};
use ui::application::NativeProfile;
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners};
use ui::widget::{Continuable as _, Widget as _};

const APP: &str = "so.tilo.code-editor";
const FILE: &str = "7164e2a8-9cf4-470b-9941-ecfe990e3d36";

type FetchObservation = (Result<Vec<u8>, String>, usize);

struct ColdPair {
    home: tilo_test_env::TestHome,
    receiving: tempfile::TempDir,
    identity: ed25519_dalek::SigningKey,
    left: Session,
    right: Session,
    address: RecordAddress,
    body: String,
}

fn room_tree(room: RoomId, identity: &[u8; 32]) -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces[0] = crate::shell::state::rehomed(&tree.desktop.workspaces[0], room);
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.set_owner(crate::shell::collab_broker_capnp::principal_id_for_root(
        *identity,
    ));
    tree
}

fn write_record(
    data: &Path,
    address: &RecordAddress,
    edit: impl FnOnce(&mut automerge::AutoCommit),
) {
    let profile_dir = record_hydration::profile_root(data, address.room, APP);
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile = NativeProfile::open(&profile_dir, Some(&address.room.to_string())).unwrap();
    let records = RecordOwners::new(profile_dir)
        .open(APP, profile.workspace_id())
        .unwrap();
    let record = RecordCheckout::new(
        address.record.clone(),
        RecordKind::new(code_editor::Project::KIND),
    );
    if record_hydration::kept_here(address, data)
        .flatten()
        .is_none()
    {
        records.create_record(&record).unwrap();
    }
    let opened = records.try_open_record(&record);
    assert!(
        matches!(
            opened,
            Ok(ui::record::RecordOpenOutcome::Loaded | ui::record::RecordOpenOutcome::AlreadyOpen)
        ),
        "existing-only admission must open the selected durable record: {opened:?}"
    );
    records.with(&record.id, edit).unwrap();
    records.try_flush().unwrap();
}

fn read_record(data: &Path, address: &RecordAddress) -> automerge::AutoCommit {
    let profile = NativeProfile::open(
        &record_hydration::profile_root(data, address.room, APP),
        Some(&address.room.to_string()),
    )
    .unwrap();
    ui::record::transfer::read_by_id(
        profile.store(),
        APP,
        profile.workspace_id(),
        &address.record,
    )
    .unwrap()
}

fn body_of(document: &automerge::AutoCommit) -> String {
    let mut project = code_editor::Project::read(document).unwrap();
    assert_eq!(project.files.len(), 1);
    let (id, file) = project.files.iter().next().unwrap();
    assert_eq!(id.as_str(), FILE);
    assert_eq!(file.name, "continuity.rs");
    assert_eq!(project.path_of(id).as_deref(), Some("continuity.rs"));
    assert_eq!(file.language, "Rust");
    assert!(file.parent.is_none());
    project
        .load_texts(document, &Demand::none().with_keys([FILE]))
        .unwrap();
    project.text(FILE).unwrap().to_owned()
}

impl ColdPair {
    fn start() -> OwnerFixtureFuture<'static, Self> {
        Box::pin(async {
            let home = tilo_test_env::TestHome::new().unwrap();
            let receiving = tempfile::tempdir().unwrap();
            let identity = ed25519_dalek::SigningKey::from_bytes(&[0xB3; 32]);
            let room = RoomId::new();
            let address = RecordAddress {
                room,
                app_id: APP.into(),
                record: RecordId::minted(),
            };
            let file = home.data().join("source-only.rs");
            code_editor::native_fixture::seed(&file).unwrap();
            let body = std::fs::read_to_string(file).unwrap();
            write_record(home.data(), &address, |doc| {
                code_editor::actions::record_entry_action(
                    FILE,
                    code_editor::project::EntryKind::File,
                    "continuity.rs",
                    None,
                    "Rust",
                )
                .apply(doc)
                .unwrap();
                code_editor::actions::record_text_action(FILE, body.clone())
                    .apply(doc)
                    .unwrap();
            });
            let left = Box::pin(Session::new(
                &identity,
                0xB4,
                room_tree(room, &identity.verifying_key().to_bytes()),
            ))
            .await;
            let right = Box::pin(Session::new(
                &identity,
                0xB5,
                room_tree(room, &identity.verifying_key().to_bytes()),
            ))
            .await;
            pair_on_topic(&left, &right, room_topic_id(room)).await;
            Self {
                home,
                receiving,
                identity,
                left,
                right,
                address,
                body,
            }
        })
    }

    fn fetch(&mut self, from_left: bool) -> OwnerFixtureFuture<'_, FetchObservation> {
        Box::pin(async move {
            eprintln!("COLD_EDITOR_FETCH begin from_left={from_left}");
            let home = if from_left {
                self.home.data()
            } else {
                self.receiving.path()
            }
            .to_path_buf();
            self.home.env().set("XDG_DATA_HOME", home);
            let (server, client) = if from_left {
                (&self.left, &self.right)
            } else {
                (&self.right, &self.left)
            };
            let mut answer = record_hydration::fetch_from(
                Arc::clone(&client.network),
                vec![(room_topic_id(self.address.room), server.endpoint.id())],
                &self.address,
            );
            let mut answered = 0_usize;
            let result = tilo_test_env::within("certified cold editor body fetch", async {
                loop {
                    server.network.tick();
                    client.network.tick();
                    let requests = std::mem::take(
                        &mut *server
                            .network
                            .inbound_remote_actions_handle()
                            .lock()
                            .unwrap(),
                    );
                    for request in &requests {
                        assert_eq!(
                            request.caller,
                            Caller::authenticated(
                                client.cert.user_public_key,
                                client.cert.device_public_key,
                            )
                            .in_room(self.address.room.to_string())
                        );
                    }
                    answered = answered.saturating_add(requests.len());
                    crate::shell::collab_broker::answer_remote_actions(
                        &server.network,
                        None,
                        requests,
                    );
                    match answer.poll_ready() {
                        PendingPoll::Ready(result) => break result,
                        PendingPoll::Abandoned => {
                            panic!("the authenticated record request was abandoned")
                        }
                        PendingPoll::Waiting => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                }
            })
            .await;
            eprintln!(
                "COLD_EDITOR_FETCH end from_left={from_left} requests={answered} ok={}",
                result.is_ok()
            );
            (result, answered)
        })
    }

    fn fetch_admitted(&mut self, from_left: bool) -> OwnerFixtureFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let (result, answered) = self.fetch(from_left).await;
            assert!(
                answered > 0,
                "the holder's production broker must observe every admitted transfer"
            );
            result.expect("the admitted holder must supply the complete document")
        })
    }

    fn assert_read_denied(&self) {
        let tree = self.left.network.replica.lock().unwrap().tree.clone();
        let query = RecordQuery {
            room: self.address.room.to_string(),
            app: APP.into(),
            record: self.address.record.as_str().into(),
            offset: 0,
        };
        let caller = Caller::authenticated(
            self.right.cert.user_public_key,
            self.right.cert.device_public_key,
        )
        .in_room(query.room.clone());
        assert!(matches!(
            record_hydration::serve(
                Some(&tree.read().desktop.workspaces[0]),
                Some(self.home.data()),
                &caller,
                &query,
            ),
            Err(ActionHostError::NotPermitted)
        ));
    }

    fn disconnect_and_edit(&mut self) -> OwnerFixtureFuture<'_, ()> {
        Box::pin(async move {
            self.left.close().await;
            self.right.close().await;
            let source_edit = format!("// source offline\n{}", self.body);
            let receiving_edit = format!("{}// receiving offline\n", self.body);
            write_record(self.home.data(), &self.address, |doc| {
                doc.set_actor(automerge::ActorId::from(
                    self.left.cert.device_public_key.to_vec(),
                ));
                code_editor::actions::text_edit_action(FILE, &self.body, &source_edit)
                    .apply(doc)
                    .unwrap();
            });
            write_record(self.receiving.path(), &self.address, |doc| {
                doc.set_actor(automerge::ActorId::from(
                    self.right.cert.device_public_key.to_vec(),
                ));
                code_editor::actions::text_edit_action(FILE, &self.body, &receiving_edit)
                    .apply(doc)
                    .unwrap();
            });
            assert_eq!(
                body_of(&read_record(self.home.data(), &self.address)),
                source_edit
            );
            assert_eq!(
                body_of(&read_record(self.receiving.path(), &self.address)),
                receiving_edit
            );
            self.left = Box::pin(Session::new(
                &self.identity,
                0xB4,
                room_tree(self.address.room, &self.identity.verifying_key().to_bytes()),
            ))
            .await;
            self.right = Box::pin(Session::new(
                &self.identity,
                0xB5,
                room_tree(self.address.room, &self.identity.verifying_key().to_bytes()),
            ))
            .await;
            pair_on_topic(&self.left, &self.right, room_topic_id(self.address.room)).await;
        })
    }

    fn assert_active_resource_open(&self) {
        let mut document = read_record(self.receiving.path(), &self.address);
        let buffers = document.get(automerge::ROOT, "buffers").unwrap().unwrap().1;
        let text = document.get(&buffers, FILE).unwrap().unwrap().1;
        let position = ui::text_position::TextPosition::capture(
            &document,
            &text,
            FILE,
            &ui::text_position::ResolvedTextPosition {
                carets: vec![(20, None)],
                top_character: 0,
                vertical_fraction: 0.25,
                horizontal_columns: 0.0,
            },
        )
        .unwrap();
        let heads = document.get_heads();
        let mut editor = code_editor::CodeApp::new();
        assert!(editor.active_file_node().is_none());
        let focus = editor
            .native_input_snapshot(&document, &automerge::ROOT)
            .unwrap()["editor_focused"]
            .clone();
        assert!(
            editor.restore_live_continuation_metadata(
                &ui::collaboration::ContinuationMetadata {
                    schema_version: ui::collaboration::semantic_schema::TEXT_POSITION_V1,
                    bytes: serde_json::to_vec(&position).unwrap(),
                },
                &document,
                &automerge::ROOT,
            ),
            "the cold editor must open the exact resource from the admitted body"
        );
        assert!(editor.active_file_node().is_some());
        let snapshot = editor
            .native_input_snapshot(&document, &automerge::ROOT)
            .unwrap();
        assert_eq!(snapshot["buffer_id"], FILE);
        assert_eq!(
            snapshot["editor_focused"], focus,
            "continuation must preserve local focus"
        );
        assert_eq!(snapshot["body_bytes"], body_of(&document).len());
        assert_eq!(
            document.get_heads(),
            heads,
            "opening a view must not mutate shared state"
        );
    }

    fn close(&mut self) -> OwnerFixtureFuture<'_, ()> {
        Box::pin(async move {
            self.left.close().await;
            self.right.close().await;
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn certified_cold_editor_body_fetch_reconnect_and_membership_revocation() {
    let mut pair = ColdPair::start().await;
    let receiving_profile =
        record_hydration::profile_root(pair.receiving.path(), pair.address.room, APP);
    assert!(
        !receiving_profile.exists(),
        "the receiving store starts genuinely cold"
    );
    let bytes = pair.fetch_admitted(true).await;
    assert!(
        !receiving_profile.exists(),
        "transport alone must not prehydrate or initialize the receiver"
    );
    let adopted = record_hydration::keep(&pair.address, pair.receiving.path(), &bytes)
        .unwrap()
        .expect("a thread that may wait keeps it at once");
    assert_eq!(adopted.id, pair.address.record);
    assert_eq!(
        body_of(&read_record(pair.receiving.path(), &pair.address)),
        pair.body
    );
    pair.disconnect_and_edit().await;
    let bytes = pair.fetch_admitted(true).await;
    record_hydration::keep(&pair.address, pair.receiving.path(), &bytes).unwrap();
    let bytes = pair.fetch_admitted(false).await;
    record_hydration::keep(&pair.address, pair.home.data(), &bytes).unwrap();
    let expected = format!("// source offline\n{}// receiving offline\n", pair.body);
    assert_eq!(
        body_of(&read_record(pair.home.data(), &pair.address)),
        expected
    );
    assert_eq!(
        body_of(&read_record(pair.receiving.path(), &pair.address)),
        expected
    );
    let mut source_heads = read_record(pair.home.data(), &pair.address).get_heads();
    let mut receiving_heads = read_record(pair.receiving.path(), &pair.address).get_heads();
    source_heads.sort_unstable();
    receiving_heads.sort_unstable();
    assert_eq!(
        source_heads, receiving_heads,
        "the whole document converges, not only visible text"
    );
    pair.assert_active_resource_open();
    let tree = pair.left.network.replica.lock().unwrap().tree.clone();
    tree.write().desktop.workspaces[0].set_owner("01".repeat(32));
    assert!(
        pair.left
            .authenticated_on(room_topic_id(pair.address.room), pair.right.endpoint.id())
    );
    pair.assert_read_denied();
    let (denied, observed) = pair.fetch(true).await;
    assert_eq!(
        observed, 0,
        "revoked room membership must reject the request before the body broker"
    );
    assert_eq!(
        denied.unwrap_err(),
        "no device of the room could give that record",
        "the expired request must resolve through production holder failover"
    );
    assert!(
        pair.left
            .authenticated_on(room_topic_id(pair.address.room), pair.right.endpoint.id())
    );
    assert_eq!(
        body_of(&read_record(pair.receiving.path(), &pair.address)),
        expected,
        "revocation must not delete the already durable user resource"
    );
    pair.close().await;
    drop(pair);
}
