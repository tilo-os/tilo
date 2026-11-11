//! Every feature of a device's session, over each way a device reaches its
//! host: directly, as a native device does, and only through the host's
//! packet relay, as a page does (it holds no UDP socket). Each scenario runs
//! once per [`Carrier`] against a real host running its own packet relay, so a
//! feature that works over one carrier and not the other fails here.
//!
//! The device under test reaches the host over the carrier, and so does
//! whatever approves it (a root-holding laptop, or a page holding the
//! person's root); the person's other devices that answer it (the one that
//! writes back) dial directly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until the test ends"
)]

#[path = "support/call.rs"]
mod call;
#[path = "support/participant.rs"]
mod participant;
mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::presence::Presence;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::VoiceGrant;
use tilo_session_host::{
    Carrier, Host, MemoryPeerStore, PeerEvent, PeerHandle, TransportPhase, VoiceEvent,
    answer_pairing, certify, pending_pairings,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, UserIdentity,
    sign_revocation_entry,
};

use support::{
    Device, HostSettings, TestHost, WORKSPACE, approve_one_over, enrol, holds, in_workspace, place,
    ready, route, say, start_host_serving, start_page, start_peer, wait_for,
};

/// Each scenario, once per carrier.
macro_rules! over_each_carrier {
    ($($scenario:ident),* $(,)?) => {
        mod direct {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $scenario() {
                    super::$scenario(tilo_session_host::Carrier::Direct).await;
                }
            )*
        }
        mod relay {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $scenario() {
                    super::$scenario(tilo_session_host::Carrier::Relay).await;
                }
            )*
        }
    };
}

over_each_carrier!(
    a_root_holding_device_certifies_a_new_one,
    the_persons_root_pairs_a_new_device,
    documents_sync_both_ways_until_the_device_is_revoked,
    the_workspace_admits_the_device_and_its_live_plane_carries_it,
    the_device_takes_part_in_a_call,
);

/// Ada's host, running its own packet relay, with her laptop (a root holder)
/// enrolled.
struct Ada {
    root: SigningKey,
    host_device: Device,
    laptop: Device,
    host: TestHost,
    state: tempfile::TempDir,
}

impl Ada {
    async fn start() -> Self {
        let (root, _) = UserIdentity::generate("Ada", 1);
        let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
        let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
        let state = support::scratch_dir();
        let host = start_host_serving(
            &root,
            &host_device,
            state.path(),
            "127.0.0.1:0".parse().unwrap(),
            HostSettings {
                packet_relay: true,
                ..HostSettings::default()
            },
        )
        .await;
        Self {
            root,
            host_device,
            laptop,
            host,
            state,
        }
    }

    /// A new device of Ada's, reaching the host as `host` says, certified by
    /// her root through the host: it shows its pairing code, and the laptop
    /// compares it and approves, dialling the host the same way.
    async fn certified(&self, host: &Host) -> (SigningKey, DeviceCert, DeviceCert) {
        let user = self.root.verifying_key().to_bytes();
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
        let certifying = tokio::spawn({
            let host = host.clone();
            let key = key.clone();
            async move {
                certify(
                    &host,
                    user,
                    "Browser on Ada's desk",
                    [0; 16],
                    &key,
                    |code| {
                        let _ = code_tx.send(code.to_owned());
                    },
                )
                .await
            }
        });
        let shown = tilo_test_env::within("the pairing code", code_rx)
            .await
            .unwrap();
        let approved = approve_one_over(host, &self.root, &self.laptop).await;
        assert_eq!(approved, shown, "the person compares codes");
        let (cert, host_cert) = certifying.await.unwrap().unwrap();
        (key, cert, host_cert)
    }

    /// Ada's laptop, in the workspace over a direct session.
    async fn laptop_peer(&self, host_cert: &DeviceCert) -> PeerHandle {
        place(&self.host.relay, &self.laptop.cert).await;
        let peer = start_peer(
            self.host.over(Carrier::Direct),
            self.laptop.cert.clone(),
            host_cert.clone(),
            self.laptop.key.clone(),
        );
        peer.join_placed(WORKSPACE, "Workspace").unwrap();
        peer
    }
}

/// A page's session over `carrier`: its device key stays in custody, so it
/// dials from an endpoint its device delegated.
fn page(
    host: &TestHost,
    carrier: Carrier,
    key: SigningKey,
    cert: &DeviceCert,
    host_cert: &DeviceCert,
) -> PeerHandle {
    start_page(
        host.over(carrier),
        cert.clone(),
        host_cert.clone(),
        key,
        Arc::new(MemoryPeerStore::default()),
    )
}

async fn a_root_holding_device_certifies_a_new_one(carrier: Carrier) {
    let ada = Ada::start().await;
    let (_, cert, host_cert) = ada.certified(&ada.host.over(carrier)).await;
    assert_eq!(
        host_cert.device_public_key,
        ada.host_device.cert.device_public_key
    );
    assert_eq!(cert.user_public_key, ada.root.verifying_key().to_bytes());
    assert!(!cert.capabilities.can_admit_devices);
    ada.host.relay.shutdown().await;
}

/// What a page holding the person's root runs: over the session it already
/// holds with the host, it lists the device waiting for it and pairs it, each
/// command its own root-signed approval, binding no endpoint of its own.
async fn the_persons_root_pairs_a_new_device(carrier: Carrier) {
    let ada = Ada::start().await;
    let host = ada.host.over(carrier);
    let (page_key, page_cert, host_cert) = ada.certified(&host).await;
    let session = page(&ada.host, carrier, page_key, &page_cert, &host_cert);
    let user = ada.root.verifying_key().to_bytes();
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let asking = tokio::spawn({
        let host = host.clone();
        let key = key.clone();
        async move {
            certify(&host, user, "Phone", [0; 16], &key, |code| {
                let _ = code_tx.send(code.to_owned());
            })
            .await
        }
    });
    let shown = tilo_test_env::within("the pairing code", code_rx)
        .await
        .unwrap();
    // Until the session first reaches the host it has no endpoint to ask
    // from, and says so.
    let waiting = wait_for("the phone to wait for the root", || async {
        pending_pairings(&session.member_host(), &ada.root)
            .await
            .ok()?
            .pop()
    })
    .await;
    assert_eq!(waiting.pairing_code, shown, "the person compares codes");
    let issued = answer_pairing(
        &session.member_host(),
        &ada.root,
        waiting.request_digest,
        true,
    )
    .await
    .unwrap()
    .expect("an approval issues a certificate");
    let (cert, host_cert) = asking.await.unwrap().unwrap();
    assert_eq!(cert, issued);
    assert_eq!(cert.device_public_key, key.verifying_key().to_bytes());
    assert_eq!(
        host_cert.device_public_key,
        ada.host_device.cert.device_public_key
    );
    assert!(
        pending_pairings(&session.member_host(), &ada.root)
            .await
            .unwrap()
            .is_empty()
    );
    session.close();
    ada.host.relay.shutdown().await;
}

async fn documents_sync_both_ways_until_the_device_is_revoked(carrier: Carrier) {
    let ada = Ada::start().await;
    let _channels = participant::Participant::start(&ada.host.relay, &ada.root).await;
    let (key, cert, host_cert) = ada.certified(&ada.host.over(carrier)).await;
    let page = page(&ada.host, carrier, key, &cert, &host_cert);
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    let laptop = ada.laptop_peer(&host_cert).await;
    ready(&[&page, &laptop]).await;

    // An acknowledged commit from the page reaches the laptop, and the
    // laptop's reply reaches the page.
    let mut note = automerge::AutoCommit::new();
    page.publish(route(), &say(&mut note, "written on the page"))
        .unwrap();
    // Behind a lock: the receipt itself is not shared between threads, and
    // the wait holds it across its sleeps.
    let receipt = std::sync::Mutex::new(page.barrier().unwrap());
    wait_for("the commit to be durable", || {
        let durable = receipt.lock().unwrap().try_recv().ok();
        async move { durable }
    })
    .await
    .unwrap();
    let mut on_laptop = holds(&laptop, &route(), &note).await;
    laptop
        .publish(route(), &say(&mut on_laptop, "typed on the laptop"))
        .unwrap();
    holds(&page, &route(), &on_laptop).await;
    assert_eq!(page.snapshot().synchronization, "acknowledged");

    // Revoked, the page is turned away on every reconnect and stops.
    let mut entry = RevocationEntry {
        user_public_key: ada.root.verifying_key().to_bytes(),
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &ada.root).unwrap();
    tilo_relay::store::save_revocations(ada.state.path(), &[entry]).unwrap();
    let failed = wait_for("the page to be refused", || {
        let snapshot = page.snapshot();
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("not-admitted"));
    assert_eq!(page.status().phase, TransportPhase::Failed);
    page.close();
    laptop.close();
    ada.host.relay.shutdown().await;
}

/// The workspace scope's record: its channel keys seal the plane's tracks.
fn plane_route() -> DocumentRoute {
    DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: WORKSPACE.to_owned(),
        record: collab_types::scope::WORKSPACE_RECORD.to_owned(),
    }
}

/// The scope's keys, as the workspace's members derive them.
fn plane_keys() -> ChannelContentKeys {
    ChannelGroup::create(
        collab_types::ChannelId::new(),
        MemberKey::generate("ada").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

async fn the_workspace_admits_the_device_and_its_live_plane_carries_it(carrier: Carrier) {
    let ada = Ada::start().await;
    let (key, cert, host_cert) = ada.certified(&ada.host.over(carrier)).await;
    place(&ada.host.relay, &cert).await;
    let keys = plane_keys();

    // The workspace's governance admits the page.
    let page = page(&ada.host, carrier, key, &cert, &host_cert);
    page.install_channel_keys(plane_route(), keys.clone())
        .unwrap();
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&page, support::workspace()).await;
    let laptop = ada.laptop_peer(&host_cert).await;
    laptop
        .install_channel_keys(plane_route(), keys.clone())
        .unwrap();
    in_workspace(&laptop, support::workspace()).await;

    // The page says where it is on the workspace's plane, and the laptop
    // hears it, opened with the scope's keys.
    let events = laptop.events();
    let said = Presence::pointing(WORKSPACE, 7, 0.25, 0.75);
    let (from, track, body) = wait_for("the laptop to hear the page on its plane", || {
        page.publish_presence(support::workspace(), vec![said.clone()])
            .unwrap();
        let events = events.drain();
        async move {
            events.into_iter().find_map(|event| match event {
                PeerEvent::LivePlane {
                    workspace_id,
                    from,
                    track,
                    body,
                    ..
                } if workspace_id == support::workspace() => Some((from, track, body)),
                _ => None,
            })
        }
    })
    .await;
    assert_eq!(from, cert.device_public_key);
    assert_eq!(track, tilo_live::live::Track::Presence);
    assert_eq!(tilo_live::presence::decode(&body).unwrap(), said);
    assert_eq!(page.live_note(), None, "the page reached its plane");

    page.close();
    laptop.close();
    ada.host.relay.shutdown().await;
}

/// The next call event of `peer` that `pick` accepts.
async fn voice<T>(
    peer: &PeerHandle,
    pending: &mut Vec<PeerEvent>,
    what: &str,
    mut pick: impl FnMut(&VoiceEvent) -> Option<T>,
) -> T {
    wait_for(what, || {
        pending.extend(peer.drain());
        let found = pending
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                PeerEvent::Voice(event) => pick(event).map(|value| (index, value)),
                _ => None,
            });
        let found = found.map(|(index, value)| {
            pending.drain(..=index);
            value
        });
        async move { found }
    })
    .await
}

async fn the_device_takes_part_in_a_call(carrier: Carrier) {
    // Alice's governed workspace has a call; Bob, whom she invited, takes
    // part from his page's session.
    let workspace = call::GovernedCall::start_relaying(true).await;
    let workspace_id = workspace.workspace_id;
    let (_bob, page) = workspace.invite("bob-browser").await;
    let peer = start_page(
        workspace.iroh().over(carrier),
        page.cert.clone(),
        workspace.host.cert.clone(),
        page.key.clone(),
        Arc::new(MemoryPeerStore::default()),
    );
    peer.join(workspace_id).unwrap();
    ready(&[&peer]).await;
    let mut pending = Vec::new();

    peer.voice_calls(workspace_id).unwrap();
    let calls = voice(&peer, &mut pending, "the call list", |event| match event {
        VoiceEvent::Calls { result, .. } => Some(result.clone()),
        _ => None,
    })
    .await
    .unwrap();
    let [listed] = calls.as_slice() else {
        panic!("one call is listed: {calls:?}");
    };
    assert_eq!(listed.state, workspace.call);

    workspace
        .owner
        .send(Envelope::VoiceJoin {
            workspace_id,
            call: workspace.call.call,
            participation: None,
        })
        .unwrap();
    peer.voice_join(workspace_id, workspace.call.call).unwrap();
    let bob: VoiceGrant = voice(&peer, &mut pending, "Bob's grant", |event| match event {
        VoiceEvent::Granted { grant, .. } if grant.participants.len() == 2 => {
            Some((**grant).clone())
        }
        _ => None,
    })
    .await;
    assert!(bob.directions.publish && bob.directions.subscribe);
    let alice = call::native(&workspace.owner, |envelope| match envelope {
        Envelope::VoiceGranted { grant, .. } if grant.participants.len() == 2 => Some(*grant),
        _ => None,
    })
    .await;
    assert_eq!(alice.call, workspace.call.call);
    peer.close();
    workspace.shutdown().await;
}
