//! Page actions on a room's records, answered by the host.
//!
//! The host runs `tilo_relay::agents::start`, whose app-serving plane also
//! answers the room's records ([`tilo_agent_participant::RoomActions`]). The
//! host holds none of the room's channel keys and a page never writes in the
//! clear, so the host neither receives a page's record nor commits a change
//! of its own: an action that would write through it is refused.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::InvocationId;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_agent_participant::{AgentHost, HostedApp, ModelChoice, Seed, record_provider};
use tilo_browser_peer::document::{Ciphers, open_frame};
use tilo_browser_peer::{DocumentRoute, Host, PeerEvent, PeerEvents, PeerHandle};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use support::{Device, ROOM, enrol, ready, start_peer, workspace};

const SHEETS: &str = "so.tilo.sheets";

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

/// A browser session speaking the remote-action vocabulary to the host.
struct Browser {
    peer: PeerHandle,
    events: PeerEvents,
    host_key: [u8; 32],
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
            answers: Vec::new(),
            correlation: 0,
        }
    }

    fn pump(&mut self) {
        for event in self.events.drain() {
            match event {
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

    async fn invoke(&mut self, request: InvokeRequest) -> InvocationReceipt {
        let invocation = request.invocation;
        self.send(RemoteActionMessage::Invoke(request));
        self.answer("receipt", |answer| match answer {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(receipt.clone())
            }
            _ => None,
        })
        .await
    }
}

/// How many retained bodies of the room are documents, readable or not.
async fn retained_documents(hosted: &Hosted) -> usize {
    let mut ciphers = Ciphers::new();
    hosted
        .relay
        .room_records()
        .retained(workspace())
        .await
        .unwrap()
        .iter()
        .filter(|retained| {
            !matches!(
                open_frame(&retained.body, retained.author, &mut ciphers),
                Ok(None)
            )
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_neither_receives_nor_writes_a_record_in_the_clear() {
    common::isolate_device();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    // 1. A page's new record waits for the room's channel keys.
    let mut page =
        HostedApp::open(&common::catalogue(), SHEETS, ROOM, "ada", false, Seed::New).unwrap();
    let sheets = DocumentRoute {
        app_id: SHEETS.into(),
        cluster_id: ROOM.into(),
        record: page.record().to_owned(),
    };
    browser
        .peer
        .publish(sheets.clone(), &page.take_mutation().unwrap())
        .unwrap();
    page.close();

    // 2. The session handles commands in order: once the catalogue answers,
    // the publish ran, and the host has listed nothing of the record because
    // nothing of it reached the host.
    let listed = browser.catalogue(CatalogueQuery::default()).await.unwrap();
    let sheets_provider = record_provider(&sheets);
    assert!(
        listed
            .providers
            .iter()
            .all(|provider| provider.provider != sheets_provider),
        "{:?}",
        listed.providers
    );
    assert_eq!(retained_documents(&hosted).await, 0);

    // 3. The room's "open an app" action would write a new record through
    // the host, which cannot seal it: refused, and nothing committed.
    let opened = browser
        .invoke(InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: format!("workspace:{ROOM}"),
                address: ActionAddress::new(format!("app.{SHEETS}"), "open"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
        })
        .await;
    let ReceiptStatus::Rejected {
        error: ActionHostError::Refused { reason },
    } = opened.status
    else {
        panic!("{opened:?}");
    };
    assert!(reason.contains("end-to-end encrypted"), "{reason}");
    assert_eq!(retained_documents(&hosted).await, 0);

    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
