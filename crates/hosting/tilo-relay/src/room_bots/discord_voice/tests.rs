#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use ed25519_dalek::SigningKey;
use tilo_connector::OutsidePerson;
use tilo_moq::client::ReceivedOpus;
use tilo_relay_protocol::AuthenticatedPeer;
use tilo_resource::Resource as _;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

use super::*;
use crate::room_bots::{membership, tests as bots};
use crate::rooms::fixtures::{self, door_join, governor};

/// Discord as the worker shows it: who is in its voice channels, what they
/// say, and what the bridge asks of it and hands it. Asked into a channel,
/// it is in it at once.
struct FakeDiscord {
    calls: watch::Sender<CallsReport>,
    audio: broadcast::Sender<OutsideAudio>,
    asked: watch::Sender<Option<(Option<String>, Option<String>)>>,
    sent: Mutex<Vec<(String, Vec<u8>)>>,
    /// The link it answers the next ask with instead of joining.
    refuse: Mutex<Option<String>>,
}

impl FakeDiscord {
    fn new() -> Self {
        Self {
            calls: watch::channel(CallsReport::default()).0,
            audio: broadcast::channel(64).0,
            asked: watch::channel(None).0,
            sent: Mutex::new(Vec::new()),
            refuse: Mutex::new(None),
        }
    }

    /// `people` in voice channel `channel`, and nobody elsewhere.
    fn in_channel(&self, channel: &str, people: &[(&str, &str)]) {
        self.calls.send_modify(|report| {
            report.calls = vec![OutsideCall {
                call: channel.to_owned(),
                people: people
                    .iter()
                    .map(|(account, name)| OutsidePerson {
                        account: (*account).to_owned(),
                        name: (*name).to_owned(),
                        avatar_url: Some(format!(
                            "https://cdn.discordapp.com/avatars/{account}/f00.png"
                        )),
                        muted: false,
                    })
                    .collect(),
            }];
        });
    }

    /// `account` mutes or unmutes in Discord.
    fn mute(&self, account: &str, muted: bool) {
        self.calls.send_modify(|report| {
            for person in report.calls.iter_mut().flat_map(|call| &mut call.people) {
                if person.account == account {
                    person.muted = muted;
                }
            }
        });
    }

    fn sent(&self) -> Vec<(String, Vec<u8>)> {
        self.sent.lock().unwrap().clone()
    }
}

impl CallLane for FakeDiscord {
    async fn set_call(&self, call: Option<String>, status: Option<String>) -> std::io::Result<()> {
        self.asked.send_replace(Some((call.clone(), status)));
        let refused = self.refuse.lock().unwrap().clone();
        self.calls.send_modify(|report| {
            report.link = match (call, refused) {
                (None, _) => CallLink::Idle,
                (Some(call), Some(reason)) => CallLink::Refused { call, reason },
                (Some(call), None) => CallLink::Joined { call },
            };
        });
        Ok(())
    }

    fn calls(&self) -> watch::Receiver<CallsReport> {
        self.calls.subscribe()
    }

    fn audio(&self) -> broadcast::Receiver<OutsideAudio> {
        self.audio.subscribe()
    }

    async fn send_audio(&self, speaker: String, opus: Vec<u8>) -> std::io::Result<()> {
        self.sent.lock().unwrap().push((speaker, opus));
        Ok(())
    }

    async fn fetch_avatar(&self, url: String) -> Result<String, String> {
        Ok(blake3::hash(url.as_bytes()).to_hex().to_string())
    }
}

/// Everything is allowed: the relay is the authority under test.
struct Open;
impl ClientAuthority for Open {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

/// A Tilo member in the call: their device, its grants and its carrier.
struct Member {
    peer: AuthenticatedPeer,
    grants: mpsc::Receiver<Envelope>,
    outbound: mpsc::Sender<Envelope>,
    grant: VoiceGrant,
    client: Client,
    timestamp: u64,
}

impl Member {
    /// Person `seed`'s device, admitted to `room` through its door.
    async fn admitted(rooms: &Rooms, host: &DeviceCert, room: &str, seed: u8) -> AuthenticatedPeer {
        let person = SigningKey::from_bytes(&[seed; 32]);
        let device = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &person).unwrap();
        door_join(rooms, host, room, (&device, &cert), seed).await;
        AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
            role: tilo_relay_protocol::PeerRole::Device,
        }
    }

    /// `peer` joins `call` and opens its carrier.
    async fn joins(rooms: &Rooms, room: &str, peer: AuthenticatedPeer, call: [u8; 32]) -> Self {
        let (outbound, grants) = mpsc::channel(64);
        let grant = fixtures::voice_join(rooms, room, &peer, call, None, outbound.clone())
            .await
            .unwrap();
        let client = Client::start(
            Connection {
                scope: ConnectionScope {
                    endpoint: grant.endpoint.clone(),
                    fingerprints: grant.fingerprints.clone(),
                    owner_epoch: grant.participation,
                    carrier_generation: 1,
                    publication: grant.publication.clone(),
                    subscription_prefix: Some(hex::encode(grant.call)),
                },
                authorization: grant.token.clone(),
            },
            Arc::new(Open),
        )
        .unwrap();
        Self {
            peer,
            grants,
            outbound,
            grant,
            client,
            timestamp: 0,
        }
    }

    /// The latest grant the relay sent, after a heartbeat.
    async fn current(&mut self, rooms: &Rooms, room: &str) -> &VoiceGrant {
        if let Ok(grant) = fixtures::voice_join(
            rooms,
            room,
            &self.peer,
            self.grant.call,
            Some(self.grant.participation),
            self.outbound.clone(),
        )
        .await
        {
            self.grant = grant;
        }
        while let Ok(envelope) = self.grants.try_recv() {
            if let Envelope::VoiceGranted { grant, .. } = envelope {
                self.grant = *grant;
            }
        }
        &self.grant
    }

    /// Say `payload` once, when the carrier takes it.
    fn says(&mut self, payload: &[u8]) {
        if self.client.status() == Status::Live {
            let _ = self.client.enable_publication();
        }
        let _ = self.client.try_send(CapturedOpus {
            timestamp_48k: self.timestamp,
            duration_samples: tilo_moq::voice::FRAME_SAMPLES,
            payload: payload.to_vec(),
            captured_at: Instant::now(),
        });
        self.timestamp = self
            .timestamp
            .saturating_add(u64::from(tilo_moq::voice::FRAME_SAMPLES));
    }

    fn heard(&self) -> Vec<ReceivedOpus> {
        std::iter::from_fn(|| self.client.try_recv()).collect()
    }
}

/// Until `done` answers something, looked at every 20 ms.
async fn until<T, F: Future<Output = Option<T>>>(what: &str, mut done: impl FnMut() -> F) -> T {
    tilo_test_env::within(what, async {
        loop {
            if let Some(value) = done().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// The room's calls as a member's device lists them.
async fn listed(rooms: &Rooms, room: &str, peer: &AuthenticatedPeer) -> Vec<CallSummary> {
    let (outbound, _) = mpsc::channel(4);
    fixtures::voice_calls(rooms, room, peer, outbound)
        .await
        .unwrap()
}

/// Who is in the call on Tilo voice channel `channel`, as members see it.
async fn in_call(
    rooms: &Rooms,
    room: &str,
    peer: &AuthenticatedPeer,
    channel: &str,
) -> Vec<VoiceParticipant> {
    listed(rooms, room, peer)
        .await
        .into_iter()
        .filter(|call| call.channel_id == channel && call.state.active)
        .flat_map(|call| call.participants)
        .collect()
}

/// A room bridged to a Discord server whose voice channel `1102` is
/// mapped to the room's Chat voice channel, which is answered.
async fn bridged_room(rooms: &Rooms, cert: &DeviceCert) -> (String, String) {
    use tilo_resource::Fields as _;
    let room = rooms.create("Lounge").await.unwrap().room;
    let link = discord::DiscordLink {
        guild_id: "1000".into(),
        only_channel: None,
        name: "Fake".into(),
        mirror_channels: false,
        map_roles: false,
    };
    discord::connect(rooms, &room, cert, &link).await.unwrap();
    let catalogue = AppCatalogue::native();
    let owner = hex::encode(cert.user_public_key);
    let voice = rooms
        .author_application(
            &room,
            cert,
            &catalogue,
            CHAT,
            &room_record(&catalogue, CHAT),
            |doc| {
                chat::demo::apply_as(
                    doc,
                    &owner,
                    chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                        name: "hangout".into(),
                        category: None,
                        kind: Some(chat::model::ChannelKind::Voice),
                        topic: None,
                    }),
                )
                .map_err(|error| error.to_string())?;
                Ok(chat::projection::read(doc)
                    .channel_rows()
                    .into_iter()
                    .find(|channel| {
                        channel.kind == chat::model::ChannelKind::Voice && channel.name == "hangout"
                    })
                    .expect("the voice channel")
                    .id)
            },
        )
        .await
        .unwrap();
    let tilo = voice.clone();
    rooms
        .author_application(
            &room,
            cert,
            &catalogue,
            discord::APP,
            &room_record(&catalogue, discord::APP),
            move |doc| {
                let current = ::discord::Guild::read_fields(doc, &automerge::ROOT)
                    .map_err(|error| error.to_string())?;
                let mut seen = current.clone();
                seen.observe(
                    "2000".into(),
                    serde_json::from_value(serde_json::json!([
                        {"id": "1101", "kind": "text", "name": "general", "parent": null, "position": 0},
                        {"id": "1102", "kind": "voice", "name": "hangout", "parent": null, "position": 1},
                    ]))
                    .map_err(|error| error.to_string())?,
                    Vec::new(),
                );
                let key = ::discord::map_key(::discord::MappedKind::Channel, &tilo);
                seen.map.push(::discord::Mapping {
                    key,
                    kind: ::discord::MappedKind::Channel,
                    tilo,
                    discord: "1102".into(),
                    origin: ::discord::Origin::Discord,
                    persona: None,
                    root: None,
                });
                seen.write_changed_fields(
                    &current,
                    doc,
                    &automerge::ROOT,
                    &tilo_resource::Delta::default(),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    (room, voice)
}

/// A relay serving its rooms' voice carrier on a local listener, deciding
/// what members and bots change against the surfaces of the apps it runs.
async fn voice_host(
    state: &std::path::Path,
    key: &SigningKey,
    cert: &DeviceCert,
) -> (crate::Relay, crate::web::WebHandle) {
    // The native apps name their surfaces as the host first runs them.
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt_from_registry("http://127.0.0.1:9");
    let relay = crate::Relay::start(
        crate::RelayConfig {
            state_root: state.join("relay"),
            identity_root: state.join("identity"),
            ..Default::default()
        },
        crate::daemon::RelayIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(crate::web::WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: crate::web::TlsSource::Development {
                hosts: vec!["localhost".into(), "127.0.0.1".into()],
                store: None,
            },
            allowed_origins: Vec::new(),
            plain_listen: None,
            public_origin: None,
            live_origin: None,
            limits: crate::web::WebLimits::default(),
            country: None,
            fetch_proxy: None,
        })
        .await
        .unwrap();
    (relay, web)
}

/// The bridge carries the room's call to Discord and back: it joins when
/// someone is in either side and leaves when both are empty, each Discord
/// speaker is heard in Tilo as their own participant, every Tilo speaker
/// reaches Discord, nothing Discord said goes back to it, and a person
/// removed from the room is carried no further.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn the_bridge_carries_a_voice_channel_both_ways_while_anyone_is_in_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let (relay, web) = voice_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, voice) = bridged_room(&rooms, &cert).await;
    let certifier = membership::HostRootCertifier::new(bots::owner_root());
    let bot = bots::discord_bridge();
    membership::keep_member(&rooms, &room, &cert, &certifier, &bot)
        .await
        .unwrap();
    let bot_cert = certifier
        .device(&room, &bot, tilo_clock::unix_now_ns())
        .unwrap()
        .cert;
    let ada = Member::admitted(&rooms, &cert, &room, 40).await;
    let bo = Member::admitted(&rooms, &cert, &room, 50).await;
    // Ada shows in the room's Chat as Ada: Discord is told that name.
    let catalogue = AppCatalogue::native();
    let ada_person = hex::encode(ada.user_public_key);
    rooms
        .author_application(
            &room,
            &cert,
            &catalogue,
            CHAT,
            &room_record(&catalogue, CHAT),
            |doc| {
                chat::demo::apply_as(
                    doc,
                    &ada_person,
                    chat::Chat::INTRODUCE.with(chat::record::chat::Introduce {
                        name: "Ada".into(),
                        initials: "A".into(),
                        color: 0,
                    }),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();

    let discord = Arc::new(FakeDiscord::new());
    let bridge = tokio::spawn(
        VoiceBridge::new(
            rooms.clone(),
            room.clone(),
            cert.clone(),
            bot_cert.clone(),
            Arc::clone(&discord),
        )
        .run(),
    );

    // Grace comes into the Discord channel: the bridge goes in on both
    // sides, and she is in the Tilo call as herself.
    discord.in_channel("1102", &[("200", "Grace")]);
    let grace = until("Grace is in the Tilo call", || {
        let (rooms, room, ada, voice) = (&rooms, &room, &ada, &voice);
        async move {
            in_call(rooms, room, ada, voice)
                .await
                .into_iter()
                .find(|participant| {
                    participant
                        .bridged
                        .as_ref()
                        .is_some_and(|bridged| bridged.account.as_deref() == Some("200"))
                })
        }
    })
    .await;
    let shown = grace.bridged.clone().unwrap();
    assert_eq!(
        (shown.service.as_str(), shown.name.as_str()),
        ("discord", "Grace")
    );
    assert!(shown.avatar.is_some(), "her picture is in the room");
    assert!(grace.publishes, "she speaks");
    assert_eq!(grace.device_public_key, bot_cert.device_public_key);
    let asked = discord.asked.borrow().clone();
    assert_eq!(
        asked,
        Some((Some("1102".into()), Some("Nobody on Tilo yet".into())))
    );

    // Ada joins the Tilo call: Discord hears who is there, and Ada hears
    // Grace as Grace.
    let call = listed(&rooms, &room, &ada)
        .await
        .into_iter()
        .find(|call| call.channel_id == voice)
        .unwrap()
        .state
        .call;
    let mut ada_in = Member::joins(&rooms, &room, ada.clone(), call).await;
    until("Discord shows Ada on Tilo", || {
        let asked = discord.asked.borrow().clone();
        async move {
            asked
                .and_then(|(_, status)| status)
                .filter(|status| status == "On Tilo: Ada")
        }
    })
    .await;
    let grace_says = [0x6a, 0x47, 0x01];
    until("Ada hears Grace", || {
        let _ = discord.audio.send(OutsideAudio {
            speaker: "200".into(),
            opus: grace_says.to_vec(),
        });
        let heard = ada_in.heard().into_iter().any(|frame| {
            frame.publisher.namespace == grace.namespace && frame.object.payload == grace_says
        });
        async move { heard.then_some(()) }
    })
    .await;

    // What Ada says reaches Discord.
    let ada_says = [0xad, 0xa0];
    until("Discord hears Ada", || {
        ada_in.says(&ada_says);
        let heard = discord.sent().iter().any(|(_, opus)| *opus == ada_says);
        async move { heard.then_some(()) }
    })
    .await;

    // Bo joins and speaks; removed from the room, he is carried no more.
    let mut bo_in = Member::joins(&rooms, &room, bo.clone(), call).await;
    let bo_says = [0xb0, 0x0b];
    until("Discord hears Bo", || {
        bo_in.says(&bo_says);
        let heard = discord.sent().iter().any(|(_, opus)| *opus == bo_says);
        async move { heard.then_some(()) }
    })
    .await;
    rooms
        .remove_member(
            &room,
            &cert,
            tilo_relay_session::rooms::RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bo.user_public_key,
            },
        )
        .await
        .unwrap();
    let bo_namespace = bo_in.grant.publication.clone().unwrap();
    tilo_test_env::within("Bo is out of the call", async {
        while ada_in
            .current(&rooms, &room)
            .await
            .participants
            .iter()
            .any(|participant| participant.namespace == bo_namespace)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    discord.sent.lock().unwrap().clear();
    for _ in 0..50 {
        bo_in.says(&bo_says);
        ada_in.says(&ada_says);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sent = discord.sent();
    assert!(
        !sent.iter().any(|(_, opus)| *opus == bo_says),
        "nothing of Bo reaches Discord after his removal"
    );
    assert!(
        sent.iter().any(|(_, opus)| *opus == ada_says),
        "Ada still does"
    );
    assert!(
        !discord.sent().iter().any(|(_, opus)| *opus == grace_says),
        "nothing Discord said goes back to it"
    );

    // Grace mutes herself in Discord: everyone in the call sees her muted,
    // and unmuted again when she unmutes.
    let grace_shows_muted = |muted: bool| {
        let (rooms, room, ada, voice) = (&rooms, &room, &ada, &voice);
        async move {
            in_call(rooms, room, ada, voice)
                .await
                .iter()
                .any(|participant| {
                    participant
                        .bridged
                        .as_ref()
                        .is_some_and(|bridged| bridged.account.as_deref() == Some("200"))
                        && participant.showing.muted == muted
                })
                .then_some(())
        }
    };
    discord.mute("200", true);
    until("Grace shows muted", || grace_shows_muted(true)).await;
    discord.mute("200", false);
    until("Grace shows unmuted", || grace_shows_muted(false)).await;

    // Grace leaves Discord: Ada keeps the bridge in; once she leaves too,
    // the bridge leaves both sides.
    discord.in_channel("1102", &[]);
    until("Grace's participation ends", || {
        let (rooms, room, ada, voice) = (&rooms, &room, &ada, &voice);
        async move {
            let participants = in_call(rooms, room, ada, voice).await;
            (participants.iter().any(|participant| {
                participant
                    .bridged
                    .as_ref()
                    .is_some_and(|bridged| bridged.account.is_none())
            }) && !participants.iter().any(|participant| {
                participant
                    .bridged
                    .as_ref()
                    .is_some_and(|bridged| bridged.account.is_some())
            }))
            .then_some(())
        }
    })
    .await;
    fixtures::voice_leave(&rooms, &ada, call, ada_in.grant.participation);
    until("the bridge leaves both sides", || {
        let asked = discord.asked.borrow().clone();
        let (rooms, room, ada, voice) = (&rooms, &room, &ada, &voice);
        async move {
            (asked == Some((None, None)) && in_call(rooms, room, ada, voice).await.is_empty())
                .then_some(())
        }
    })
    .await;

    bridge.abort();
    drop(web);
    relay.shutdown().await;
}

/// Discord refusing the bridge its voice channel is said where the app
/// shows the bridge's state, and cleared once the bridge is let in.
#[tokio::test(flavor = "multi_thread")]
async fn a_voice_channel_discord_refuses_is_said_in_the_guild_record() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let (relay, web) = voice_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, _) = bridged_room(&rooms, &cert).await;
    let certifier = membership::HostRootCertifier::new(bots::owner_root());
    let bot = bots::discord_bridge();
    membership::keep_member(&rooms, &room, &cert, &certifier, &bot)
        .await
        .unwrap();
    let bot_cert = certifier
        .device(&room, &bot, tilo_clock::unix_now_ns())
        .unwrap()
        .cert;
    let discord = Arc::new(FakeDiscord::new());
    *discord.refuse.lock().unwrap() = Some("missing the Connect permission".into());
    let bridge = tokio::spawn(
        VoiceBridge::new(
            rooms.clone(),
            room.clone(),
            cert.clone(),
            bot_cert,
            Arc::clone(&discord),
        )
        .run(),
    );
    let problem = || {
        let (rooms, room) = (&rooms, &room);
        async move {
            let record = room_record(&AppCatalogue::native(), discord::APP);
            let saved = rooms
                .record_document(room, discord::APP, &record)
                .await
                .unwrap()
                .unwrap();
            ::discord::Guild::read(&automerge::AutoCommit::load(&saved).unwrap())
                .unwrap()
                .voice_problem
        }
    };
    discord.in_channel("1102", &[("200", "Grace")]);
    until("the refusal is in the record", || {
        let problem = problem();
        async move { problem.await.filter(|problem| problem.contains("Connect")) }
    })
    .await;

    // Let in: the problem is gone.
    *discord.refuse.lock().unwrap() = None;
    discord.calls.send_modify(|report| {
        report.link = CallLink::Joined {
            call: "1102".into(),
        };
    });
    until("the refusal is cleared", || {
        let problem = problem();
        async move { problem.await.is_none().then_some(()) }
    })
    .await;

    bridge.abort();
    drop(web);
    relay.shutdown().await;
}

#[test]
fn the_bridge_keeps_the_pair_it_carries_and_otherwise_takes_the_first_occupied() {
    let pair = |discord: &str| VoicePair {
        discord: discord.into(),
        tilo: format!("t{discord}"),
    };
    let pairs = [pair("1"), pair("2"), pair("3")];
    assert_eq!(choose(&pairs, None, |_| false), None);
    assert_eq!(choose(&pairs, None, |p| p.discord != "1"), Some(pair("2")));
    assert_eq!(
        choose(&pairs, Some(&pair("3")), |_| true),
        Some(pair("3")),
        "kept while occupied"
    );
    assert_eq!(
        choose(&pairs, Some(&pair("3")), |p| p.discord == "1"),
        Some(pair("1"))
    );
}

#[test]
fn blocked_people_and_bridges_do_not_occupy_a_channel() {
    let mut guild = ::discord::Guild::default();
    guild.blocked_personas.insert("300".into());
    let report = CallsReport {
        calls: vec![OutsideCall {
            call: "1102".into(),
            people: ["200", "300"]
                .iter()
                .map(|account| OutsidePerson {
                    account: (*account).into(),
                    name: "x".into(),
                    avatar_url: None,
                    muted: false,
                })
                .collect(),
        }],
        link: CallLink::Idle,
    };
    let people: Vec<&str> = discord_people(&report, "1102", &guild)
        .iter()
        .map(|person| person.account.as_str())
        .collect();
    assert_eq!(people, ["200"]);
    assert!(discord_people(&report, "9", &guild).is_empty());
    assert_eq!(shown_name("  Grace ", "200"), "Grace");
    assert_eq!(shown_name("Gr\u{202E}ace", "200"), "Discord user 200");
}
