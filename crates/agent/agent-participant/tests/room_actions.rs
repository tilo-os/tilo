//! Page actions on records nobody has open, answered by the host.
//!
//! The host runs `tilo_relay::agents::start`, whose app-serving plane now
//! also answers the room's records ([`tilo_agent_participant::RoomActions`]).
//! A browser session creates a Sheets and a Slides record and closes them;
//! no agent exists. The page then lists the room's catalogue, runs a record's
//! action through the host, retries it under the same id, reuses the id for
//! another request, is asked to confirm a destructive action, and cancels an
//! invocation queued behind another. Every change the host makes arrives as
//! an ordinary durable change authored by the host device.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard; one scenario in order"
)]

#[path = "../../../browser/tilo-browser-peer/tests/support/steward.rs"]
mod steward;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
pub(crate) mod support;

mod common;
#[cfg(test)]
#[path = "room_actions/encrypted_resource.rs"]
mod encrypted_resource;
#[cfg(test)]
#[path = "room_actions/feedback_journey.rs"]
mod feedback_journey;
#[cfg(test)]
#[path = "room_actions/governed_sealed.rs"]
mod governed_sealed;
#[path = "room_actions/resource_support.rs"]
mod resource_support;

#[cfg(not(test))]
pub mod mounted_resource;

tilo_automerge_isolate::test_worker!();
// Room actions hold each record in a confined app worker: this binary,
// started again.
tilo_agent_participant::app_worker!();

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
#[cfg(test)]
use tilo_actions::{
    ConfirmationPolicy,
    remote::{Confirmation, ReceiptStatus},
};
use tilo_agent_participant::{AgentHost, ModelChoice, Replica, Seed, record_provider};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerEvents, PeerHandle};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use support::{Device, ROOM, enrol, ready, start_peer, workspace};

const SHEETS: &str = "so.tilo.sheets";
const SLIDES: &str = "so.tilo.slides";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_existing_native_chat_reference_discovers_local_views_over_the_governed_https_host() {
    use chat::doc::{ChatDoc, NewMessage};
    use tilo_actions::remote::ViewResourceQuery;
    let (identity_key, _) = tilo_user_identity::UserIdentity::generate("Resource fixture", 1);
    let host = enrol(
        &identity_key,
        "temporary-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let browser_device = enrol(
        &identity_key,
        "temporary-browser",
        DeviceCapabilities::member(),
    );
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&identity_key, &host, state.path()).await;
    let room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Native resource fixture")
        .await
        .unwrap();
    let mut browser = Browser::open_room(&hosted, &browser_device, &host, &room.room).await;
    let mut source = Replica::open(
        &common::catalogue(),
        "so.tilo.chat",
        &room.room,
        "ada",
        false,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    ChatDoc::create_workspace(&mut doc, "Native shared space");
    let channel = ChatDoc::list_channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .unwrap();
    let message = ChatDoc::push_message(
        &mut doc,
        &channel.id,
        NewMessage {
            author: "ada".into(),
            text: "Hello é, shared natively".into(),
            ..NewMessage::default()
        },
    );
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    let id = format!("chat.message:{message}");
    let reference = serde_json::json!({"id": id, "kind": "chat.message", "label": "Native selection",
        "span": {"start": 6, "end": 7, "unit": "Chars"}});
    source
        .invoke(
            &ActionAddress::new(format!("chat.channel:{}", channel.id), "share-resource"),
            &serde_json::json!({"reference": reference, "text": "Discuss this resource"})
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .unwrap();
    let shared = source
        .read(|doc| ChatDoc::list_messages(doc, &channel.id))
        .unwrap()
        .into_iter()
        .find(|message| message.text == "Discuss this resource")
        .unwrap();
    assert_eq!(serde_json::to_value(shared.reference).unwrap(), reference);
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: source.record().into(),
    };
    browser
        .peer
        .publish(route.clone(), &source.take_mutation().unwrap())
        .unwrap();
    let provider = record_provider(&route);
    let policy = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(collab_types::Role::Owner), "ada").in_room(&room.room),
        Vec::new(),
    );
    let source_views = source
        .read(|doc| {
            common::catalogue().resource_views(
                "so.tilo.chat",
                doc,
                &ViewResourceQuery {
                    id: id.clone(),
                    kind: "chat.message".into(),
                },
                &policy,
            )
        })
        .unwrap();
    assert!(
        !source_views.is_empty(),
        "the native source publishes its exact local view"
    );
    let query = CatalogueQuery {
        provider: Some(provider.clone()),
        view_resource: Some(ViewResourceQuery {
            id: id.clone(),
            kind: "chat.message".into(),
        }),
        max_results: 256,
        ..CatalogueQuery::default()
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let page = loop {
        let result = browser.catalogue(query.clone()).await;
        if let Ok(page) = &result
            && !page.rows.is_empty()
        {
            break page.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the governed host never discovered the existing native reference: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(page.rows.iter().all(|row| {
        row.resource.id == id
            && row.resource.kind == "chat.message"
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.kind == tilo_actions::OperationKind::View)
    }));
    let locator = page.locator.unwrap();
    assert_eq!(locator.origin, hosted.web.origin);
    assert_eq!(locator.room, room.room);
    assert_eq!(
        locator.host_device,
        hex::encode(host.cert.device_public_key)
    );
    assert_eq!(locator.provider, provider);
    for wanted in [
        ViewResourceQuery {
            id: "chat.message:missing".into(),
            kind: "chat.message".into(),
        },
        ViewResourceQuery {
            id: id.clone(),
            kind: "notes.block".into(),
        },
    ] {
        let empty = browser
            .catalogue(CatalogueQuery {
                view_resource: Some(wanted),
                ..query.clone()
            })
            .await
            .unwrap();
        assert!(empty.rows.is_empty());
        assert!(empty.locator.is_none());
    }
    assert!(
        browser
            .catalogue(CatalogueQuery {
                provider: Some("record:so.tilo.chat:missing-source".into()),
                ..query.clone()
            })
            .await
            .is_err(),
        "a missing exact source must not create a record, keys or a locator"
    );
    let other_room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Isolated resource fixture")
        .await
        .unwrap();
    let mut other = Browser::open_room(&hosted, &browser_device, &host, &other_room.room).await;
    assert!(
        other.catalogue(query.clone()).await.is_err(),
        "an exact reference from another room must not produce views or a locator"
    );
    other.peer.close();
    let ordinary = browser
        .catalogue(CatalogueQuery {
            provider: Some(provider),
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    assert!(
        ordinary
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .all(|action| action.kind == tilo_actions::OperationKind::Document)
    );
    // The production catalogue worker must discard already-read metadata when
    // the authority changes before delivery, even though the source was valid.
    let bytes = source.read(|doc| doc.clone().save()).unwrap();
    let caller = tilo_actions::remote::Caller::authenticated(
        browser_device.cert.user_public_key,
        browser_device.cert.device_public_key,
    );
    for revoke_on_read in [false, true] {
        let link = Arc::new(DeliveryReadFixture {
            member: tilo_agent_participant::room_actions::RoomMember {
                endpoint: tilo_agent_participant::HostEndpoint {
                    host: Host::Https(support::https::DevHttps::new(
                        hosted.web.addr,
                        hosted.web.certificate_sha256.as_ref().unwrap(),
                    )),
                    host_cert: host.cert.clone(),
                },
                key: host.key.clone(),
                runtime: tokio::runtime::Handle::current(),
            },
            caller: caller.clone(),
            route: route.clone(),
            bytes: bytes.clone(),
            revoke_on_read,
            revoked: std::sync::atomic::AtomicBool::new(false),
            locator_reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let actions = tilo_agent_participant::RoomActions::new(Arc::new(common::catalogue));
        actions.attach(link.clone()).unwrap();
        let answer = actions
            .serve(
                room.workspace_id,
                &caller,
                RemoteActionMessage::CatalogueRequest {
                    correlation: 1,
                    query: query.clone(),
                },
            )
            .unwrap();
        let response = answer
            .wait_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .unwrap();
        let RemoteActionMessage::CatalogueResponse { result, .. } = response else {
            panic!("expected catalogue response")
        };
        if revoke_on_read {
            assert_eq!(result, Err(ActionHostError::UnknownAction));
            assert_eq!(
                link.locator_reads.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "revoked metadata must never reach locator issuance"
            );
        } else {
            let page = result.unwrap();
            assert!(
                !page.rows.is_empty(),
                "the same production worker accepts current Read"
            );
            assert!(page.locator.is_some());
        }
        assert_eq!(link.bytes, bytes, "view discovery cannot mutate its source");
        actions.shutdown();
    }
    browser.peer.close();
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

/// Deterministically changes authority after a real Chat snapshot is read.
struct DeliveryReadFixture {
    member: tilo_agent_participant::room_actions::RoomMember,
    caller: tilo_actions::remote::Caller,
    route: DocumentRoute,
    bytes: Vec<u8>,
    revoke_on_read: bool,
    revoked: std::sync::atomic::AtomicBool,
    locator_reads: std::sync::atomic::AtomicUsize,
}

impl tilo_agent_participant::room_actions::RoomRecordsLink for DeliveryReadFixture {
    fn user_public_key(&self) -> [u8; 32] {
        self.member.endpoint.host_cert.user_public_key
    }
    fn member(&self) -> tilo_agent_participant::room_actions::RoomMember {
        self.member.clone()
    }
    fn view_read_policy(
        &self,
        workspace: u64,
        caller: &tilo_actions::remote::Caller,
        app: &str,
        record: &str,
    ) -> Option<tilo_agent_participant::room_actions::ViewReadPolicy> {
        if self.revoked.load(std::sync::atomic::Ordering::SeqCst)
            || caller != &self.caller
            || self.route.workspace_id().ok() != Some(workspace)
            || app != self.route.app_id
            || record != self.route.record
        {
            return None;
        }
        Some(tilo_agent_participant::room_actions::ViewReadPolicy {
            room: self.route.cluster_id.clone(),
            role: collab_types::Role::Owner,
            caps: Vec::new(),
            person: hex::encode(self.user_public_key()),
            unix_time: 1,
        })
    }
    fn view_document(
        &self,
        workspace: u64,
        caller: &tilo_actions::remote::Caller,
        source: &DocumentRoute,
    ) -> Result<tilo_agent_participant::room_actions::ViewDocument, String> {
        if source != &self.route
            || self
                .view_read_policy(workspace, caller, &source.app_id, &source.record)
                .is_none()
        {
            return Err("read refused".into());
        }
        self.revoked
            .store(self.revoke_on_read, std::sync::atomic::Ordering::SeqCst);
        Ok(tilo_agent_participant::room_actions::ViewDocument {
            bytes: self.bytes.clone(),
            revision: "fixture-source".into(),
        })
    }
    fn view_locator(
        &self,
        room: &str,
        provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        self.locator_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(tilo_actions::remote::ResourceViewLocator {
            origin: "https://fixture.tilo.invalid".into(),
            browser_page: Some("https://fixture.tilo.invalid/qos-runtime/resource.html".into()),
            room: room.into(),
            host_device: hex::encode(self.member.endpoint.host_cert.device_public_key),
            provider: provider.into(),
        })
    }
}

struct Hosted {
    relay: Relay,
    web: WebHandle,
    agents: AgentHost,
}

async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    start_browser(root, host, state, None).await
}

async fn start_browser(
    root: &SigningKey,
    host: &Device,
    state: &Path,
    page: Option<&str>,
) -> Hosted {
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.join("relay"),
        resource_browser_page: page.map(|origin| format!("{origin}/resource.html")),
        ..Default::default()
    };
    cfg.workspaces.push(tilo_relay::WorkspaceBinding {
        id: workspace(),
        pin: true,
        replication: tilo_relay::ReplicationIntent::FullWorkspace,
        admitted_users: vec![hex::encode(user)],
        shell_authority: None,
        checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
    });
    let (relay, agents) = tilo_relay::agents::start(
        cfg,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host.key.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        tilo_relay::agents::AgentOptions {
            state_dir: state.join("agents"),
            model: ModelChoice::Scripted,
            catalogue: Some(Arc::new(common::catalogue)),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: Some(state.join("web-dev-identity")),
            },
            allowed_origins: page.into_iter().map(str::to_owned).collect(),
            plain_listen: None,
        })
        .await
        .unwrap();
    Hosted { relay, web, agents }
}

/// A document checkpoint: its hash and its bytes.
type Checkpoint = ([u8; 32], Vec<u8>);

/// A browser session speaking the remote-action vocabulary to the host.
struct Browser {
    peer: PeerHandle,
    events: PeerEvents,
    host_key: [u8; 32],
    documents: BTreeMap<DocumentRoute, Vec<Checkpoint>>,
    answers: Vec<RemoteActionMessage>,
    correlation: u64,
    workspace: u64,
    room: String,
}

impl Browser {
    async fn open(hosted: &Hosted, device: &Device, host: &Device) -> Self {
        Self::open_room(hosted, device, host, ROOM).await
    }

    async fn open_room(hosted: &Hosted, device: &Device, host: &Device, room: &str) -> Self {
        let workspace = tilo_relay_protocol::room_workspace_id(room).unwrap();
        let https = support::https::DevHttps::new(
            hosted.web.addr,
            hosted.web.certificate_sha256.as_ref().unwrap(),
        );
        let peer = start_peer(
            Host::Https(https),
            device.cert.clone(),
            host.cert.clone(),
            device.key.clone(),
        );
        let events = peer.events();
        if room != ROOM {
            ready(&[&peer]).await;
            let (reply, answer) = tokio::sync::oneshot::channel();
            peer.rooms_then(
                tilo_relay_protocol::rooms::RoomsRequest::Enter { room: room.into() },
                Box::new(move |result| {
                    let _ = reply.send(result);
                }),
            )
            .unwrap();
            assert!(matches!(
                tilo_test_env::within("the node enters the governed fixture", answer)
                    .await
                    .unwrap()
                    .unwrap(),
                tilo_relay_protocol::rooms::RoomsReply::Room(_)
            ));
        }
        peer.join(workspace).unwrap();
        ready(&[&peer]).await;
        Self {
            peer,
            events,
            host_key: host.cert.device_public_key,
            documents: BTreeMap::new(),
            answers: Vec::new(),
            correlation: 0,
            workspace,
            room: room.to_owned(),
        }
    }

    fn pump(&mut self) {
        for event in self.events.drain() {
            match event {
                PeerEvent::Document(document) => {
                    if let Some(mutation) = document.mutation() {
                        let bytes = match mutation {
                            collab_types::DocumentMutation::Snapshot(bytes)
                            | collab_types::DocumentMutation::Incremental(bytes) => bytes,
                        };
                        self.documents
                            .entry(document.route.clone())
                            .or_default()
                            .push((document.author, bytes));
                    }
                }
                PeerEvent::RemoteAction { from, frame, .. } if from == self.host_key => {
                    self.answers
                        .push(RemoteActionFrame::decode(&frame).unwrap().message);
                }
                PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                    panic!("undeliverable: {reason}")
                }
                _ => {}
            }
        }
    }

    fn send(&self, message: RemoteActionMessage) {
        let frame = RemoteActionFrame::new(message).encode().unwrap();
        self.peer
            .send_remote_action(self.workspace, self.host_key, frame)
            .unwrap();
    }

    /// Wait for the first answer `pick` accepts, removing it.
    async fn answer<T>(
        &mut self,
        what: &str,
        pick: impl Fn(&RemoteActionMessage) -> Option<T>,
    ) -> T {
        let deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
        loop {
            self.pump();
            if let Some(index) = self
                .answers
                .iter()
                .position(|answer| pick(answer).is_some())
            {
                let answer = self.answers.remove(index);
                return pick(&answer).unwrap();
            }
            assert!(Instant::now() < deadline, "no answer: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn catalogue(&mut self, query: CatalogueQuery) -> Result<CataloguePage, ActionHostError> {
        self.correlation = self.correlation.checked_add(1).unwrap();
        let correlation = self.correlation;
        self.send(RemoteActionMessage::CatalogueRequest { correlation, query });
        self.answer("catalogue", |answer| match answer {
            RemoteActionMessage::CatalogueResponse {
                correlation: got,
                result,
            } if *got == correlation => Some(result.clone()),
            _ => None,
        })
        .await
    }

    async fn receipt(&mut self, invocation: InvocationId) -> InvocationReceipt {
        self.answer("receipt", |answer| match answer {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(receipt.clone())
            }
            _ => None,
        })
        .await
    }

    async fn invoke(&mut self, request: InvokeRequest) -> InvocationReceipt {
        let invocation = request.invocation;
        self.send(RemoteActionMessage::Invoke(request));
        self.receipt(invocation).await
    }

    /// Create `app`'s record the way a page does, then close it: nobody has
    /// it open afterwards.
    fn create(&mut self, app: &str) -> DocumentRoute {
        self.create_before_publish(app, |_| {})
    }

    fn create_before_publish(
        &mut self,
        app: &str,
        before_publish: impl FnOnce(&DocumentRoute),
    ) -> DocumentRoute {
        let mut page = Replica::open(
            &common::catalogue(),
            app,
            &self.room,
            "ada",
            false,
            Seed::New,
        )
        .unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: self.room.clone(),
            record: page.wire_record().to_owned(),
        };
        let mutation = page.take_mutation().unwrap();
        before_publish(&route);
        self.peer.publish(route.clone(), &mutation).unwrap();
        // The host never echoes a device's own change back to it.
        let bytes = match mutation {
            collab_types::DocumentMutation::Snapshot(bytes)
            | collab_types::DocumentMutation::Incremental(bytes) => bytes,
        };
        self.documents
            .entry(route.clone())
            .or_default()
            .push(([0; 32], bytes));
        route
    }

    /// The record at `route` as this page has received it.
    fn mount(&mut self, route: &DocumentRoute) -> Replica {
        self.pump();
        let changes = self.documents.get(route).cloned().unwrap_or_default();
        Replica::open(
            &common::catalogue(),
            &route.app_id,
            &self.room,
            "ada",
            false,
            Seed::Existing {
                record: if route.record.is_empty() {
                    ui::record::RecordId::package_primary()
                } else {
                    ui::record::RecordId::parse(&route.record).unwrap()
                },
                changes: changes.into_iter().map(|(_, bytes)| bytes).collect(),
            },
        )
        .unwrap()
    }
}

fn sheet_names(app: &Replica) -> Vec<String> {
    let body: serde_json::Value = app
        .read(|doc| serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap())
        .unwrap();
    body["wb"]["sheets"]
        .as_array()
        .map(|sheets| {
            sheets
                .iter()
                .map(|sheet| sheet["name"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The argument of Sheets' New sheet: the sheet's name.
fn sheet_named(name: &str) -> InvokeArgument {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), name.into());
    InvokeArgument::Json(arguments)
}

fn request(provider: &str, address: ActionAddress, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address,
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        fresh_binding: None,
        wait_ms: None,
    }
}

/// The record `create_before_publish` makes, with the host device's
/// admission to its channel held: the host sees it sealed and cannot open it
/// until the hold is dropped.
async fn create_withheld(
    browser: &mut Browser,
    steward: &steward::Steward,
    host_key: [u8; 32],
) -> (DocumentRoute, steward::HeldAdmission) {
    let mut held = None;
    let route = browser.create_before_publish(SHEETS, |route| {
        held = Some(steward.hold_admission(route, host_key));
    });
    let held = held.unwrap();
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    held.requested().await;
    (route, held)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_whose_keys_are_withheld_holds_back_no_other() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let steward = steward::Steward::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let host_key = host.cert.device_public_key;

    // A record the host holds keys for. The room's first listing joins it
    // and waits, once, for every record's keys.
    let open = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let open_provider = record_provider(&open);
    let started = Instant::now();
    let listing = browser.catalogue(CatalogueQuery::default()).await.unwrap();
    let joined = started.elapsed();
    assert!(
        listing
            .providers
            .iter()
            .any(|p| p.provider == open_provider),
        "{:?}",
        listing.providers
    );
    let by_name = CatalogueQuery {
        provider: Some(open_provider.clone()),
        max_results: 200,
        ..CatalogueQuery::default()
    };
    let new_sheet = browser
        .catalogue(by_name.clone())
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "new-sheet")
        .expect("Sheets offers New sheet")
        .address
        .clone();
    let started = Instant::now();
    let warm = browser
        .invoke(request(
            &open_provider,
            new_sheet.clone(),
            sheet_named("Keys held"),
        ))
        .await;
    let acted_with_keys = started.elapsed();
    eprintln!(
        "room actions latency: join and first listing {joined:?}, invoke with keys {acted_with_keys:?}"
    );
    assert!(
        matches!(warm.status, ReceiptStatus::Completed { .. }),
        "{warm:?}"
    );

    // A second record arrives sealed, and the host's admission to it is held.
    let (withheld, held) = create_withheld(&mut browser, &steward, host_key).await;

    // Nothing about the first record waits for the second's keys.
    let started = Instant::now();
    browser.catalogue(by_name).await.unwrap();
    let named_while_held = started.elapsed();
    let started = Instant::now();
    let listing = browser.catalogue(CatalogueQuery::default()).await.unwrap();
    let listed_while_held = started.elapsed();
    let started = Instant::now();
    let acted = browser
        .invoke(request(
            &open_provider,
            new_sheet.clone(),
            sheet_named("Keys withheld elsewhere"),
        ))
        .await;
    let acted_while_held = started.elapsed();
    eprintln!(
        "room actions latency, another record's keys withheld: named catalogue \
         {named_while_held:?}, listing {listed_while_held:?}, invoke {acted_while_held:?}"
    );
    assert!(
        matches!(acted.status, ReceiptStatus::Completed { .. }),
        "{acted:?}"
    );
    assert!(
        !listing
            .providers
            .iter()
            .any(|p| p.provider == record_provider(&withheld)),
        "a record the host cannot open is not listed"
    );

    // An invoke on the withheld record waits for its keys, then runs as
    // soon as they arrive: the key's arrival wakes the wait.
    let pending = request(
        &record_provider(&withheld),
        new_sheet.clone(),
        sheet_named("After the keys"),
    );
    browser.send(RemoteActionMessage::Invoke(pending.clone()));
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    browser.pump();
    assert!(
        !browser.answers.iter().any(|answer| matches!(
            answer,
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == pending.invocation
        )),
        "nothing runs on a record before its keys: {:?}",
        browser.answers
    );
    let released = Instant::now();
    drop(held);
    let after_keys = browser.receipt(pending.invocation).await;
    let ran_after_keys = released.elapsed();
    eprintln!("room actions latency: the withheld record ran {ran_after_keys:?} after its keys");
    assert!(
        matches!(after_keys.status, ReceiptStatus::Completed { .. }),
        "{after_keys:?}"
    );

    // Keys that never arrive refuse the invocation clearly; nothing runs.
    let (refused, held) = create_withheld(&mut browser, &steward, host_key).await;
    let started = Instant::now();
    let never = browser
        .invoke(request(
            &record_provider(&refused),
            new_sheet,
            sheet_named("Never"),
        ))
        .await;
    let refused_after = started.elapsed();
    eprintln!("room actions latency: a record without keys refused after {refused_after:?}");
    match &never.status {
        ReceiptStatus::Rejected {
            error: ActionHostError::Unavailable { reason },
        } => assert!(
            reason.contains(&refused.record),
            "the refusal names the record: {reason}"
        ),
        other => panic!("an invocation without keys is refused: {other:?}"),
    }
    browser.pump();
    assert!(
        !browser
            .documents
            .get(&refused)
            .is_some_and(|changes| changes.iter().any(|(author, _)| *author == host_key)),
        "the host wrote nothing to a record it could not open"
    );
    // The steward stops before the hold is released, so it never admits
    // the host into a relay that is shutting down.
    drop(steward);
    drop(held);

    let prompt = Duration::from_secs(5);
    assert!(
        named_while_held < prompt && listed_while_held < prompt && acted_while_held < prompt,
        "another record's missing keys held a request back: catalogue {named_while_held:?}, \
         listing {listed_while_held:?}, invoke {acted_while_held:?}"
    );
    assert!(
        ran_after_keys < prompt,
        "the wait did not end when the keys arrived: {ran_after_keys:?}"
    );
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_listed_themes_action_is_one_the_host_can_run() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let _steward = steward::Steward::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let themes = browser.create("so.tilo.themes");
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let provider = record_provider(&themes);
    let page = browser
        .catalogue(CatalogueQuery {
            provider: Some(provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    // The catalogue offers only what the record's door runs: the record's
    // document operations — the host runs no app's code, so effects and
    // views stay with the app where it is open. A listed operation may fail,
    // need confirmation or other arguments, but is never unknown.
    let listed: Vec<_> = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .collect();
    assert!(!listed.is_empty(), "a themes record offers its operations");
    assert!(
        listed
            .iter()
            .all(|action| action.kind == tilo_actions::OperationKind::Document),
        "the record catalogue must offer only document operations: {:?}",
        listed
            .iter()
            .map(|action| &action.address)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        listed
            .iter()
            .filter(|action| action.address.action == "new-theme")
            .count(),
        1,
        "creating a theme within the open Studio is one canonical document action"
    );
    for action in page.rows.iter().filter_map(|row| row.action.as_ref()) {
        let mut arguments = serde_json::Map::new();
        for param in action.params.iter().filter(|param| param.required) {
            arguments.insert(param.name.to_string(), "Dusk".into());
        }
        let receipt = browser
            .invoke(InvokeRequest {
                on_behalf_of: None,
                confirmation: Some(Confirmation::Confirmed),
                ..request(
                    &provider,
                    action.address.clone(),
                    InvokeArgument::Json(arguments),
                )
            })
            .await;
        assert_ne!(
            receipt.status,
            ReceiptStatus::Rejected {
                error: ActionHostError::UnknownAction
            },
            "{:?} is listed but unknown",
            action.address
        );
    }
    let retained = hosted
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap();
    let (readable, _) = common::retained_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_acts_on_records_nobody_has_open_through_the_host() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let steward = steward::Steward::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let host_key = host.cert.device_public_key;

    let sheets = browser.create(SHEETS);
    let slides = browser.create(SLIDES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let sheets_provider = record_provider(&sheets);
    let slides_provider = record_provider(&slides);

    // 1. The catalogue: every record of the room is a provider, and with no
    // provider named the recent records' actions are listed with the room's
    // "open an app" actions.
    let page = browser.catalogue(CatalogueQuery::default()).await.unwrap();
    let providers: Vec<&str> = page.providers.iter().map(|p| p.provider.as_str()).collect();
    assert!(
        providers.contains(&sheets_provider.as_str()),
        "{providers:?}"
    );
    assert!(
        providers.contains(&slides_provider.as_str()),
        "{providers:?}"
    );
    assert!(
        providers.contains(&format!("workspace:{ROOM}").as_str()),
        "{providers:?}"
    );
    let sheets_page = browser
        .catalogue(CatalogueQuery {
            provider: Some(sheets_provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    let new_sheet = sheets_page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "new-sheet")
        .expect("Sheets offers New sheet")
        .address
        .clone();

    // 2. Invoke on a record nobody has open: the host mounts it, runs the
    // action and commits the change as its own.
    let add = request(&sheets_provider, new_sheet.clone(), sheet_named("Budget"));
    let receipt = browser.invoke(add.clone()).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        browser.pump();
        let by_host = browser
            .documents
            .get(&sheets)
            .is_some_and(|changes| changes.iter().any(|(author, _)| *author == host_key));
        if by_host {
            let app = browser.mount(&sheets);
            let names = sheet_names(&app);
            if names.iter().filter(|name| *name == "Budget").count() == 1 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "the host's change never arrived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 3. Idempotency: the same request under the same id reports the
    // original and runs nothing; another request under it is a conflict.
    let retry = browser.invoke(add.clone()).await;
    assert_eq!(retry, receipt);
    let mut other = add.clone();
    other.argument = sheet_named("Other");
    let conflict = browser.invoke(other).await;
    assert_eq!(
        conflict.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::InvocationConflict
        }
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let app = browser.mount(&sheets);
    assert_eq!(
        sheet_names(&app)
            .iter()
            .filter(|name| name.as_str() == "Budget")
            .count(),
        1,
        "a retry never runs the action twice: {:?}",
        sheet_names(&app)
    );
    assert!(!sheet_names(&app).contains(&"Other".to_owned()));

    // 4. Confirmation: a destructive action is refused without the caller's
    // explicit confirmation, and nothing ran; confirmed, it runs.
    let slides_page = browser
        .catalogue(CatalogueQuery {
            provider: Some(slides_provider.clone()),
            max_results: 200,
            ..CatalogueQuery::default()
        })
        .await
        .unwrap();
    let destructive = slides_page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| {
            action.contract.confirmation == ConfirmationPolicy::Confirm
                && action.params.is_empty()
                && matches!(action.input, tilo_actions::InputSpec::None)
        })
        .expect("Slides offers a destructive action")
        .address
        .clone();
    let unconfirmed = request(&slides_provider, destructive.clone(), InvokeArgument::None);
    let refused = browser.invoke(unconfirmed.clone()).await;
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::ConfirmationRequired
        }
    );
    let confirmed = InvokeRequest {
        on_behalf_of: None,
        confirmation: Some(Confirmation::Confirmed),
        ..unconfirmed
    };
    let ran = browser.invoke(confirmed).await;
    assert!(
        !matches!(
            ran.status,
            ReceiptStatus::Rejected {
                error: ActionHostError::ConfirmationRequired
            }
        ),
        "a refused invocation is forgotten, so the confirmed retry under its id is admitted: {ran:?}"
    );

    // 5. Cancel: an invocation queued behind another is cancelled before it
    // starts and never runs; a finished one cannot be cancelled.
    // Hold the first record's real host admission so its mount cannot finish
    // before the queued invocation's cancel arrives. A cold mount alone is
    // not a synchronization boundary on a fast or differently scheduled host.
    let mut held = None;
    let busy_route = browser.create_before_publish(SHEETS, |route| {
        held = Some(steward.hold_admission(route, host_key));
    });
    let held = held.unwrap();
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let slow = request(
        &record_provider(&busy_route),
        new_sheet.clone(),
        sheet_named("Busy"),
    );
    let queued = request(&sheets_provider, new_sheet, sheet_named("Never"));
    held.requested().await;
    browser.send(RemoteActionMessage::Invoke(slow.clone()));
    browser.send(RemoteActionMessage::Invoke(queued.clone()));
    browser.send(RemoteActionMessage::Cancel {
        invocation: queued.invocation,
    });
    let cancel = browser
        .answer("cancel", |answer| match answer {
            RemoteActionMessage::CancelResponse { invocation, result }
                if *invocation == queued.invocation =>
            {
                Some(result.clone())
            }
            _ => None,
        })
        .await;
    drop(held);
    let queued_receipt = browser.receipt(queued.invocation).await;
    let slow_receipt = browser.receipt(slow.invocation).await;
    assert!(
        matches!(slow_receipt.status, ReceiptStatus::Completed { .. }),
        "{slow_receipt:?}"
    );
    match cancel {
        Ok(()) => {
            assert!(
                matches!(queued_receipt.status, ReceiptStatus::Cancelled { .. }),
                "{queued_receipt:?}"
            );
            browser.send(RemoteActionMessage::observe(queued.invocation));
            let observed = browser
                .answer("observe", |answer| match answer {
                    RemoteActionMessage::Update { invocation, state }
                        if *invocation == queued.invocation =>
                    {
                        Some(state.clone())
                    }
                    _ => None,
                })
                .await
                .expect("the page owns its invocation");
            assert!(matches!(
                observed.receipt().status,
                ReceiptStatus::Cancelled { .. }
            ));
            tokio::time::sleep(Duration::from_millis(500)).await;
            let app = browser.mount(&sheets);
            assert!(!sheet_names(&app).contains(&"Never".to_owned()));
        }
        Err(error) => panic!("the queued invocation was not cancellable: {error:?}"),
    }
    browser.send(RemoteActionMessage::Cancel {
        invocation: add.invocation,
    });
    let finished = browser
        .answer("cancel finished", |answer| match answer {
            RemoteActionMessage::CancelResponse { invocation, result }
                if *invocation == add.invocation =>
            {
                Some(result.clone())
            }
            _ => None,
        })
        .await;
    assert!(matches!(
        finished,
        Err(ActionHostError::NotCancellable { .. })
    ));

    // 6. The room's "open an app" action creates a record through the host.
    let opened = browser
        .invoke(request(
            &format!("workspace:{ROOM}"),
            ActionAddress::new(format!("app.{SHEETS}"), "open"),
            InvokeArgument::None,
        ))
        .await;
    let ReceiptStatus::Completed { output } = opened.status else {
        panic!("{opened:?}");
    };
    assert!(
        output.value.unwrap()["provider"]
            .as_str()
            .unwrap()
            .starts_with("record:so.tilo.sheets:")
    );

    let retained = hosted
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap();
    let (readable, _) = common::retained_documents(
        retained
            .into_iter()
            .map(|retained| (retained.author, retained.body)),
    );
    assert_eq!(readable, 0, "nothing reaches the host in the clear");
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
