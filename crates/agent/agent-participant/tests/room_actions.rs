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

#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, Confirmation, InvocationReceipt,
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ConfirmationPolicy, InvocationId};
use tilo_agent_participant::{AgentHost, HostedApp, ModelChoice, Seed, record_provider};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerEvents, PeerHandle};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use support::{Device, ROOM, enrol, ready, start_peer, workspace};

const SHEETS: &str = "so.tilo.sheets";
const SLIDES: &str = "so.tilo.slides";

struct Hosted {
    relay: Relay,
    web: WebHandle,
    agents: AgentHost,
}

async fn start(root: &SigningKey, host: &Device, state: &Path) -> Hosted {
    let user = root.verifying_key().to_bytes();
    let mut cfg = tilo_relay::RelayConfig {
        state_root: state.join("relay"),
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
            host_profile: false,
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
            allowed_origins: Vec::new(),
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
}

impl Browser {
    async fn open(hosted: &Hosted, device: &Device, host: &Device) -> Self {
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
        peer.join(workspace()).unwrap();
        ready(&[&peer]).await;
        Self {
            peer,
            events,
            host_key: host.cert.device_public_key,
            documents: BTreeMap::new(),
            answers: Vec::new(),
            correlation: 0,
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
            .send_remote_action(workspace(), self.host_key, frame)
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
        let mut page =
            HostedApp::open(&common::catalogue(), app, ROOM, "ada", false, Seed::New).unwrap();
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: ROOM.into(),
            record: page.record().to_owned(),
        };
        let mutation = page.take_mutation().unwrap();
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
        page.close();
        route
    }

    /// The record at `route` as this page has received it.
    fn mount(&mut self, route: &DocumentRoute) -> HostedApp {
        self.pump();
        let changes = self.documents.get(route).cloned().unwrap_or_default();
        HostedApp::open(
            &common::catalogue(),
            &route.app_id,
            ROOM,
            "ada",
            false,
            Seed::Existing {
                record: ui::record::RecordId::parse(&route.record).unwrap(),
                changes: changes.into_iter().map(|(_, bytes)| bytes).collect(),
            },
        )
        .unwrap()
    }
}

fn sheet_names(app: &HostedApp) -> Vec<String> {
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

fn request(provider: &str, address: ActionAddress, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address,
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_listed_themes_action_is_one_the_host_can_run() {
    common::isolate_device();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
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
    // The catalogue offers only what the record's door runs: a listed action
    // may fail, need confirmation or other arguments, but is never unknown.
    let new_theme = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .filter(|action| action.action_label == "New theme");
    assert_eq!(
        new_theme.count(),
        1,
        "a launch-only action is not offered on an open record"
    );
    for action in page.rows.iter().filter_map(|row| row.action.as_ref()) {
        let mut arguments = serde_json::Map::new();
        for param in action.params.iter().filter(|param| param.required) {
            arguments.insert(param.name.to_string(), "Dusk".into());
        }
        let receipt = browser
            .invoke(InvokeRequest {
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
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_acts_on_records_nobody_has_open_through_the_host() {
    common::isolate_device();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
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
    let add = request(
        &sheets_provider,
        new_sheet.clone(),
        InvokeArgument::Text("Budget".into()),
    );
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
            app.close();
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
    other.argument = InvokeArgument::Text("Other".into());
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
    app.close();

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
    // The first job mounts a record the host has never mounted, which keeps
    // the service busy while the cancel for the second arrives.
    let busy_route = browser.create(SHEETS);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let slow = request(
        &record_provider(&busy_route),
        new_sheet.clone(),
        InvokeArgument::Text("Busy".into()),
    );
    let queued = request(
        &sheets_provider,
        new_sheet,
        InvokeArgument::Text("Never".into()),
    );
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
    let queued_receipt = browser.receipt(queued.invocation).await;
    let slow_receipt = browser.receipt(slow.invocation).await;
    assert!(
        !matches!(slow_receipt.status, ReceiptStatus::Accepted),
        "{slow_receipt:?}"
    );
    match cancel {
        Ok(()) => {
            assert!(
                matches!(queued_receipt.status, ReceiptStatus::Cancelled { .. }),
                "{queued_receipt:?}"
            );
            browser.send(RemoteActionMessage::Observe {
                invocation: queued.invocation,
            });
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
            app.close();
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

    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
