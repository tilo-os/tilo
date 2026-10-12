//! A call between the owner's native device and an invited person's
//! browser in a room the relay governs. The native side is the shell's own
//! path: the joined-room owner holds the room, its relay session carries the
//! call, and a [`Participation`] starts the call, keeps the governor's grant
//! and authorizes its voice carrier. Both get their grants from the relay
//! and hear each other over its `moq-lite` carrier.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/call.rs"]
mod call;
mod support;

tilo_automerge_isolate::test_worker!();

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::HttpsLink;
use tilo_desktop_model::RoomId;
use tilo_identity_host::custody::HostIdentity;
use tilo_joined_room::NativeWorkspaceOwner;
use tilo_joined_room::session::{Connect, Scope};
use tilo_joined_room::voice::{Channel, Participation};
use tilo_moq::client::{Client, Status};
use tilo_peer_state::join::{self, MemberRole};
use tilo_peer_state::{
    NeighbourMsg,
    owner_commit::{CommitOperation, OwnerCommitRequest},
};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::rooms::RoomAdminClient;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay_protocol::Envelope;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use call::{
    CHAT, browser, carrier, create_voice_channel, endpoint, enter, granted, hears, invite, until,
};
use support::{TestHost, enrol, start_room_host};

/// A person's native device holding a room through the joined-room owner,
/// as the shell does, with its call participations.
struct Native {
    identity: HostIdentity,
    endpoint: iroh::Endpoint,
    owner: NativeWorkspaceOwner,
    room: RoomId,
    workspace_id: u64,
    scope: Arc<Scope>,
    session: RelayCoeditSession,
    calls: Vec<Arc<Participation>>,
}

impl Native {
    async fn pending_channel_start(&mut self) {
        use automerge::AutoCommit;
        const LATE: &str = "late-governor-channel";
        let now = SystemTime::now();
        let (message, late) = self
            .owner
            .with_current(self.room, |view| {
                let mut doc = AutoCommit::load(&view.peer().document(CHAT, "").unwrap()).unwrap();
                let heads = doc.get_heads();
                let workspace = chat::projection::read(&doc)
                    .workspace_rows()
                    .into_iter()
                    .next()
                    .expect("the governed chat has a workspace")
                    .id;
                chat::demo::apply_as(
                    &mut doc,
                    "fixture",
                    chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                        workspace: workspace.into(),
                        name: LATE.into(),
                        category: None,
                        kind: Some(chat::model::ChannelKind::Voice),
                    }),
                )
                .unwrap();
                let late = chat::projection::read(&doc)
                    .channel_rows()
                    .into_iter()
                    .find(|channel| channel.name == LATE)
                    .expect("the action created the late voice channel")
                    .id;
                view.peer()
                    .clone()
                    .publish_incremental(
                        CHAT,
                        "",
                        doc.save_after(&heads),
                        &self.identity.device_signing_key,
                        now,
                    )
                    .map(|message| (message, late))
            })
            .unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = message else {
            panic!("signed native channel write")
        };
        self.owner
            .commit_document(
                OwnerCommitRequest {
                    room: self.room.to_string(),
                    operation: CommitOperation::Document {
                        app: CHAT.into(),
                        record: String::new(),
                        envelope,
                    },
                },
                now,
            )
            .unwrap();
        // Hold the actual signed outbox: local admission precedes the governor's.
        let call = Participation::open(
            self.owner.clone(),
            self.room,
            &Channel {
                app: CHAT.into(),
                record: String::new(),
                channel: late,
            },
            &self.session,
            |_| Ok(()),
        )
        .unwrap();
        tilo_test_env::within(
            "governor explicitly defers an unadmitted exact channel",
            async {
                loop {
                    if let Some(Inbound::Voice(event)) = self.session.recv().await
                        && let tilo_relay_session::coedit::VoiceEvent::Result {
                            result: Err(reason),
                            ..
                        } = &event
                    {
                        assert_eq!(
                            reason,
                            tilo_relay_protocol::voice::CHANNEL_ADMISSION_PENDING
                        );
                        assert!(call.apply(&event, Some(&self.session), |_| false));
                        break;
                    }
                }
            },
        )
        .await;
        assert!(call.refused().is_none());
        assert!(call.grant().is_none());
        self.scope.publish(&self.session, &self.identity).unwrap();
        self.calls = vec![call.clone()];
        tilo_test_env::within("pending Start is retried only after explicit governor deferral", async {
            while call.grant().is_none() && call.refused().is_none() {
                call.heartbeat(&self.session, |_| Ok(())).unwrap();
                tokio::select! {
                    event = self.session.recv() => match event.expect("pending Start session stays open") {
                        Inbound::Delta { author, data } => {
                            let message = postcard::from_bytes(&data).unwrap();
                            self.scope.input(author, true, message).admit(&self.identity).unwrap();
                        }
                        Inbound::Voice(event) => { call.apply(&event, Some(&self.session), |_| false); }
                        _ => {}
                    },
                    () = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
        }).await;
        assert!(call.refused().is_none());
        assert!(call.grant().is_some());
        call.leave(Some(&self.session));
        self.calls.clear();
        self.scope.close();
        (self.scope, self.session) = Self::dial(
            &self.owner,
            self.room,
            &self.identity,
            &self.endpoint,
            self.workspace_id,
        )
        .await;
    }

    async fn join(
        directory: &std::path::Path,
        cert: DeviceCert,
        key: SigningKey,
        invitation: &str,
        workspace_id: u64,
    ) -> Self {
        let owner = tilo_joined_room::open_file(&directory.join("joined-rooms.json")).unwrap();
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::default(),
            workspace_revocations: HashMap::default(),
            identity_paths: None,
        };
        let endpoint = endpoint(&key).await;
        let enrollment = tilo_joined_room::enroll::prepare_and_complete(
            WorkspaceJoinClient::new(endpoint.clone()),
            identity.clone(),
            workspace_id,
            None,
            invitation.to_owned(),
            owner.clone(),
        )
        .await
        .unwrap();
        owner.install(&enrollment, SystemTime::now()).unwrap();
        let room = RoomId::parse(&enrollment.bootstrap().room).unwrap();
        let (scope, session) = Self::dial(&owner, room, &identity, &endpoint, workspace_id).await;
        Self {
            identity,
            endpoint,
            owner,
            room,
            workspace_id,
            scope,
            session,
            calls: Vec::new(),
        }
    }

    /// Connect to the relay the room's current pin names, as the shell's
    /// actor does whenever that pin changes.
    async fn dial(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        identity: &HostIdentity,
        endpoint: &iroh::Endpoint,
        workspace_id: u64,
    ) -> (Arc<Scope>, RelayCoeditSession) {
        let connect = Connect::new(owner.clone(), room).unwrap();
        let (hint, device, revocations, continuity) = connect.material(identity).unwrap();
        let session = tilo_joined_room::session::dial(
            endpoint.clone(),
            workspace_id,
            &hint,
            device.clone(),
            identity.device_signing_key.clone(),
            revocations,
        )
        .await
        .unwrap();
        (
            Arc::new(Scope::new(connect, hint, device, continuity)),
            session,
        )
    }

    /// Admit the room's deltas and hand the governor's call answers to the
    /// participations, as the shell's actor does, until `done`.
    async fn receive_until(&mut self, what: &str, done: impl Fn(&Self) -> bool + Send + Sync) {
        tilo_test_env::within(what, async {
            while !done(self) {
                let inbound = tokio::select! {
                    inbound = self.session.recv() => inbound.unwrap_or_else(|| panic!("{what}: native relay session closed before the expected call state")),
                    () = tokio::time::sleep(Duration::from_millis(50)) => continue,
                };
                match inbound {
                    Inbound::Delta { author, data } => {
                        let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                        let governor = self
                            .owner
                            .with_current(self.room, |view| Ok(view.governor().device_public_key))
                            .unwrap();
                        match self
                            .scope
                            .input(author, author == governor, message)
                            .admit(&self.identity)
                        {
                            Ok(_) => {}
                            Err(error) if error.contains("pin changed") => {
                                (self.scope, self.session) = Self::dial(
                                    &self.owner,
                                    self.room,
                                    &self.identity,
                                    &self.endpoint,
                                    self.workspace_id,
                                )
                                .await;
                                for call in &self.calls {
                                    let _ = call.rejoin(&self.session);
                                }
                            }
                            Err(error) => panic!("{what}: admission refused: {error}"),
                        }
                    }
                    Inbound::Voice(event) => {
                        for call in &self.calls {
                            if call.apply(&event, Some(&self.session), |_| false) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .await;
    }

    /// Join the call on the voice channel, starting it, as the shell does.
    async fn call(&mut self, channel: &str) -> Arc<Participation> {
        let channel = Channel {
            app: CHAT.into(),
            record: String::new(),
            channel: channel.into(),
        };
        loop {
            // Until the governor admits the channel, it refuses to start.
            let call = Participation::open(
                self.owner.clone(),
                self.room,
                &channel,
                &self.session,
                |_| Ok(()),
            )
            .unwrap();
            self.calls = vec![call.clone()];
            self.receive_until("the governor answers the call", |_| {
                call.grant().is_some() || call.refused().is_some()
            })
            .await;
            if call.grant().is_some() {
                return call;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Retire the Start transport after admission but without applying its
    /// result: policy activation itself advances the shell's continuity pin.
    async fn call_after_retired_start(
        &mut self,
        admin: &RoomAdminClient,
        address: iroh::EndpointAddr,
        channel: &str,
    ) -> Arc<Participation> {
        let call = Participation::open(
            self.owner.clone(),
            self.room,
            &Channel {
                app: CHAT.into(),
                record: String::new(),
                channel: channel.into(),
            },
            &self.session,
            |_| Ok(()),
        )
        .unwrap();
        let admitted = tilo_test_env::within(
            "the exact started call is admitted before its result",
            async {
                loop {
                    let event = self
                        .session
                        .recv()
                        .await
                        .expect("Start transport stays open until retirement");
                    if let Inbound::Delta { author, data } = event {
                        let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                        let governor = self
                            .owner
                            .with_current(self.room, |view| Ok(view.governor().device_public_key))
                            .unwrap();
                        self.scope
                            .input(author, author == governor, message)
                            .admit(&self.identity)
                            .unwrap();
                    }
                    // Voice results are deliberately not delivered to Participation.
                    if let Some(id) = self
                        .owner
                        .with_current(self.room, |view| {
                            Ok(view.peer().validated_media_policy()?.and_then(|policy| {
                                policy
                                    .calls
                                    .into_iter()
                                    .find(|(_, entry)| entry.binding.channel_id == channel)
                                    .map(|(id, _)| id)
                            }))
                        })
                        .unwrap()
                    {
                        break id;
                    }
                }
            },
        )
        .await;
        assert!(call.grant().is_none());
        assert!(call.refused().is_none());
        let other = admin
            .create(address.clone(), "Other private call room")
            .await
            .unwrap();
        enter(admin, address.clone(), &other.room).await;
        let invited = admin
            .invite(
                address,
                &other.room,
                MemberRole::Editor,
                Some(self.identity.device_cert.user_public_key),
                Duration::from_secs(600),
            )
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let foreign = Self::join(
            directory.path(),
            self.identity.device_cert.clone(),
            self.identity.device_signing_key.clone(),
            &join::encode_invitation_text(&invited.signed_invitation),
            other.workspace_id,
        )
        .await;
        assert_eq!(
            call.rejoin(&foreign.session).unwrap_err(),
            "the session belongs to another room"
        );
        assert!(
            call.grant().is_none(),
            "a foreign session cannot grant a Starting participation"
        );
        assert!(
            call.refused().is_none(),
            "foreign input cannot replace the pending exact binding"
        );
        self.scope.close();
        (self.scope, self.session) = Self::dial(
            &self.owner,
            self.room,
            &self.identity,
            &self.endpoint,
            self.workspace_id,
        )
        .await;
        call.rejoin(&self.session)
            .expect("fresh admitted binding recovers the lost Start result");
        self.calls = vec![call.clone()];
        self.receive_until(
            "the recovered Start receives its genuine governor grant",
            |_| call.grant().is_some() || call.refused().is_some(),
        )
        .await;
        assert!(call.refused().is_none());
        assert_eq!(
            call.grant().unwrap().call,
            admitted,
            "recovery joins the original call rather than starting another"
        );
        call
    }
}

async fn voice_journey(recover_start: bool) {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let desk = enrol(&alice, "alice-desk", DeviceCapabilities::primary());
    let page = enrol(&bob, "bob-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_room_host(&alice, &host, state.path(), false).await;
    let address = relay.node_addr();

    // Alice's room on her relay; her native device holds it as its owner,
    // and Bob's person is invited.
    let admin = RoomAdminClient::new(endpoint(&laptop.key).await, laptop.cert.clone());
    let room = admin.create(address.clone(), "Standup").await.unwrap();
    enter(&admin, address.clone(), &room.room).await;
    enter(
        &RoomAdminClient::new(endpoint(&desk.key).await, desk.cert.clone()),
        address.clone(),
        &room.room,
    )
    .await;
    let own = admin
        .invite(
            address.clone(),
            &room.room,
            MemberRole::Editor,
            Some(alice.verifying_key().to_bytes()),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut native = Native::join(
        directory.path(),
        laptop.cert.clone(),
        laptop.key.clone(),
        &join::encode_invitation_text(&own.signed_invitation),
        room.workspace_id,
    )
    .await;
    invite(&admin, address.clone(), &room.room, &bob, &page).await;

    // The chat gets a voice channel; Alice's native device starts its call
    // and the governor grants her the first participation.
    let (_replica, voice) = create_voice_channel(
        address.clone(),
        room.workspace_id,
        &room.room,
        &host.cert,
        &desk,
    )
    .await;
    native
        .receive_until("Alice's native device admits the voice channel", |native| {
            native
                .owner
                .with_current(native.room, |view| Ok(view.peer().document(CHAT, "")))
                .unwrap()
                .is_some()
        })
        .await;
    let call = if recover_start {
        native.pending_channel_start().await;
        native
            .call_after_retired_start(&admin, address.clone(), &voice)
            .await
    } else {
        native.call(&voice).await
    };
    let alone = call.grant().unwrap();
    assert_eq!(alone.participants.len(), 1);
    assert!(alone.subscriptions.is_empty());

    // Bob's browser joins the same call over HTTPS.
    let mut page_link = HttpsLink::connect(
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap()),
        &page.cert,
        host.cert.device_public_key,
        &RevocationList::new(),
        &page.key,
    )
    .await
    .unwrap();
    page_link
        .send(Envelope::Subscribe {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    browser(&mut page_link, |envelope| match envelope {
        Envelope::Ready { .. } => Some(()),
        Envelope::NotAuthorised { reason, .. } => panic!("browser not admitted: {reason}"),
        _ => None,
    })
    .await;
    page_link
        .send(Envelope::VoiceJoin {
            workspace_id: room.workspace_id,
            call: alone.call,
            participation: None,
        })
        .unwrap();
    let bob_grant = browser(&mut page_link, granted).await;
    native
        .receive_until("Alice's grant names Bob", |_| {
            call.grant()
                .is_some_and(|grant| grant.participants.len() == 2)
        })
        .await;
    let alice_grant = call.grant().unwrap();
    let alice_voice = alice_grant.publication.clone().unwrap();
    let bob_voice = bob_grant.publication.clone().unwrap();
    assert_eq!(bob_grant.subscriptions, vec![alice_voice.clone()]);
    assert_eq!(alice_grant.subscriptions, vec![bob_voice.clone()]);

    // Alice's carrier is the one her participation authorizes; Bob's is his
    // browser's. Audio flows both ways.
    let alice_carrier =
        Arc::new(Client::start(call.next_carrier().unwrap().unwrap(), call.clone()).unwrap());
    call.set_client(&alice_carrier).unwrap();
    let bob_carrier = carrier(&bob_grant, 1);
    until("Alice's carrier", &alice_carrier, |status| {
        status == Status::Live
    })
    .await;
    until("Bob's carrier", &bob_carrier, |status| {
        status == Status::Live
    })
    .await;
    alice_carrier.enable_publication().unwrap();
    bob_carrier.enable_publication().unwrap();
    hears(
        "Bob hears Alice",
        &alice_carrier,
        &bob_carrier,
        &alice_voice,
    )
    .await;
    hears("Alice hears Bob", &bob_carrier, &alice_carrier, &bob_voice).await;

    // Her heartbeat is answered with a grant for the same scope.
    let before = call.revision();
    call.heartbeat(&native.session, |_| Ok(())).unwrap();
    native
        .receive_until("the governor answers Alice's heartbeat", |_| {
            call.revision() > before
        })
        .await;
    assert!(!call.stale(), "a heartbeat keeps the carrier's scope");
    assert!(call.authorized());
    assert_eq!(call.id(), Some(alone.participation));

    // Losing local device custody fences capture and transport synchronously,
    // before a heartbeat discovers that the participation must end.
    let capture_stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let capture_ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = capture_stopped.clone();
    let ended = capture_ended.clone();
    call.set_capture_stop(Arc::new(move |terminal| {
        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        if terminal {
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    native.owner.invalidate_identity(native.room);
    assert_eq!(alice_carrier.status(), Status::Closed);
    assert!(capture_stopped.load(std::sync::atomic::Ordering::SeqCst));
    assert!(call.inspect_current(|_, _| Ok(())).is_err());
    assert!(call.rejoin(&native.session).is_err());
    assert!(alice_carrier.enable_publication().is_err());
    assert!(call.heartbeat(&native.session, |_| Ok(())).is_err());
    assert!(call.live().is_err());
    assert!(capture_ended.load(std::sync::atomic::Ordering::SeqCst));

    // Alice leaves: her carrier closes and Bob's roster loses her.
    call.leave(Some(&native.session));
    until("Alice's carrier closes", &alice_carrier, |status| {
        status != Status::Live
    })
    .await;
    browser(&mut page_link, |envelope| {
        granted(envelope).filter(|grant| grant.participants.len() == 1)
    })
    .await;
    drop(web);
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_native_shell_and_an_invited_browser_talk_on_relay_grants() {
    voice_journey(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retired_start_recovers_the_exact_call_before_native_and_browser_audio() {
    voice_journey(true).await;
}
