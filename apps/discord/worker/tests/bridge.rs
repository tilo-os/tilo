//! The bridge bot end to end: its program installed in a room on a relay,
//! the `discord-worker` binary under the connector host, and fake-discord
//! as the server. A message crosses in both directions, each side showing
//! who wrote it, and edits, replies, reactions and deletes follow.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::AutoCommit;
use discord::record::{Guild, MappedKind, Ownership, guild};
use ed25519_dalek::SigningKey;
use fake_discord::{CHANNEL_GENERAL, FakeDiscord, GUILD, TOKEN, User};
use serde_json::Value;
use tilo_agent_participant::AppCatalogue;
use tilo_connector::{BlobAccess, ConnectorProcess, WorkerCommand};
use tilo_relay::room_bots::installed::{self, BotRunner};
use tilo_relay::room_bots::membership::{HostRootCertifier, keep_member};
use tilo_relay::rooms::Rooms;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use ui::tilo_actions::ActionContext;
use ui::tilo_resource::{Id, Invoke, Replica, Resource as _};

tilo_automerge_isolate::test_worker!();
tilo_agent_participant::app_worker!();

const APP: &str = "so.tilo.discord";
const CHAT: &str = "so.tilo.chat";
const ADA: [u8; 32] = [8; 32];

#[derive(Default)]
struct Blobs {
    stored: Mutex<HashMap<String, Vec<u8>>>,
}

impl BlobAccess for Blobs {
    fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        self.stored.lock().unwrap().get(blake3).cloned()
    }
    fn store(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        self.stored
            .lock()
            .unwrap()
            .insert(hash.clone(), bytes.to_vec());
        Ok(hash)
    }
    fn publish(&self, _blake3: &str) -> Result<(), String> {
        Ok(())
    }
}

fn governor() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "home".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// The room's records, read as the governor's copy holds them.
struct Room {
    rooms: Rooms,
    id: String,
    cert: DeviceCert,
    catalogue: AppCatalogue,
}

impl Room {
    async fn doc(&self, app: &str) -> AutoCommit {
        AutoCommit::load(
            &self
                .rooms
                .record_document(&self.id, app, "")
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }

    async fn chat(&self) -> chat::Chat {
        chat::Chat::read(&self.doc(CHAT).await).unwrap()
    }

    /// The chat's first part of messages, read.
    async fn part_messages(&self) -> chat::parts::ChatPart {
        chat::messages::read_part(&self.part().await)
    }

    /// The chat's first part of messages, as the room holds it.
    async fn part(&self) -> AutoCommit {
        self.rooms
            .record_document(&self.id, CHAT, chat::parts::part(0).as_str())
            .await
            .ok()
            .flatten()
            .map(|bytes| AutoCommit::load(&bytes).unwrap())
            .unwrap_or_default()
    }

    /// Do `invoke` to the chat's messages as `author`, as their page does: a
    /// view of the chat record and the parts of its messages the room
    /// holds, the verb going through the chain to the part it changes
    /// (starting the first), each changed part carried to the room on its
    /// own route. The ids of the messages it added.
    async fn part_as(&self, author: &str, invoke: Invoke<chat::parts::ChatPart>) -> Vec<String> {
        use tilo_agent_participant::{Replica, ReplicaActor, Seed};
        use ui::collaboration::DocumentMutation;
        let chat = self
            .rooms
            .record_document(&self.id, CHAT, "")
            .await
            .unwrap()
            .unwrap();
        let mut held = Vec::new();
        for (app, record) in self.rooms.record_routes(&self.id).await.unwrap() {
            let Some(part) = ui::record::RecordId::parse(&record)
                .filter(|part| app == CHAT && chat::parts::part_index(part).is_some())
            else {
                continue;
            };
            let bytes = self
                .rooms
                .record_document(&self.id, CHAT, &record)
                .await
                .unwrap()
                .unwrap();
            held.push((part, bytes));
        }
        let ids = |page: &Replica| -> Vec<String> {
            page.records_beside()
                .iter()
                .filter_map(|record| page.read_record(record, chat::messages::read_part))
                .flat_map(|part| {
                    part.messages
                        .ids()
                        .map(|id| id.as_str().to_owned())
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let (taken, added) = {
            let mut page = Replica::open(
                &self.catalogue,
                CHAT,
                &self.id,
                author,
                ReplicaActor::Owner,
                Seed::Existing {
                    record: ui::record::RecordId::package_primary(),
                    changes: vec![chat],
                },
            )
            .unwrap();
            for (part, bytes) in held {
                page.apply_record(&part, &DocumentMutation::Snapshot(bytes))
                    .unwrap();
            }
            let before = ids(&page);
            page.invoke(
                &invoke.address_on(ui::framework::chain_resource_id(&chat::parts::chain())),
                &invoke.arguments,
                true,
            )
            .unwrap();
            let added: Vec<String> = ids(&page)
                .into_iter()
                .filter(|id| !before.contains(id))
                .collect();
            (page.take_record_mutations(), added)
        };
        for (record, mutation) in taken {
            match mutation {
                DocumentMutation::Snapshot(bytes) => self
                    .rooms
                    .author_room_record(&self.id, &self.cert, (CHAT, record.as_str()), move |doc| {
                        tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                            .map(drop)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .unwrap(),
                DocumentMutation::Incremental(bytes) => self
                    .rooms
                    .author_application(
                        &self.id,
                        &self.cert,
                        &self.catalogue,
                        CHAT,
                        record.as_str(),
                        move |doc| {
                            tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                                .map(drop)
                                .map_err(|error| error.to_string())
                        },
                    )
                    .await
                    .unwrap(),
            }
        }
        added
    }

    async fn guild(&self) -> Guild {
        Guild::read(&self.doc(APP).await).unwrap()
    }

    /// Change the Chat record as `author`, as their own device would.
    async fn chat_as<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut AutoCommit) -> Result<T, String> + Send + 'static,
    ) -> T {
        self.rooms
            .author_application(&self.id, &self.cert, &self.catalogue, CHAT, "", change)
            .await
            .unwrap()
    }

    /// The messages posted under another name, as their text and that name:
    /// the name a post showed is in its change's entry, and the message it
    /// made is the one its change added.
    async fn bridged(&self) -> Vec<(String, String)> {
        let mut doc = self.part().await;
        let ids = |doc: &AutoCommit| -> Vec<String> {
            chat::messages::read_part(doc)
                .messages
                .ids()
                .map(|id| id.as_str().to_owned())
                .collect()
        };
        let mut out = Vec::new();
        for change in doc.get_changes(&[]) {
            let Some(name) = change
                .message()
                .and_then(ActionContext::from_change_message)
                .and_then(|context| context.invoker?.appearance)
                .map(|appearance| appearance.name)
            else {
                continue;
            };
            let (hash, parents) = (change.hash(), change.deps().to_vec());
            let before = ids(&doc.clone().fork_at(&parents).unwrap());
            let mut after = doc.clone().fork_at(&[hash]).unwrap();
            let part = chat::messages::read_part(&after);
            for id in ids(&mut after) {
                if !before.contains(&id) {
                    let text = part
                        .messages
                        .iter()
                        .find(|(m, _)| m.as_str() == id)
                        .map(|(_, m)| m.text.clone())
                        .unwrap_or_default();
                    out.push((text, name.clone()));
                }
            }
        }
        out
    }
}

/// Run the bridge until `done` holds, failing on any run that does not
/// complete.
async fn settle<T, F, Fut>(runner: &mut BotRunner, what: &str, mut done: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    // Steps that did not succeed meanwhile, shown when it times out.
    let mut unfinished = Vec::new();
    loop {
        for report in runner.step().await.unwrap() {
            unfinished.extend(
                report
                    .steps
                    .iter()
                    .filter(|step| {
                        !matches!(step.status, tilo_automation::StepStatus::Succeeded { .. })
                    })
                    .map(|step| format!("{} {step:?}", report.trigger)),
            );
            assert_eq!(
                report.outcome,
                tilo_automation::RunOutcome::Completed,
                "{what}: {report:#?}"
            );
        }
        if let Some(found) = done().await {
            return found;
        }
        let dead: Vec<_> = runner.dead_letters().collect();
        assert!(dead.is_empty(), "{what}: {dead:#?}");
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: timed out; steps that did not succeed: {unfinished:#?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// How a test's bridge is set up.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent way a test's server and room start"
)]
struct Setup {
    /// Bridge only this Discord channel.
    only: Option<&'static str>,
    /// Make channels created in Tilo later in Discord too.
    mirror: bool,
    /// Keep Tilo's roles as roles the bridge owns in Discord.
    roles: bool,
    /// A server that already had channels of its own when its bridge first ran,
    /// and the room's Chat holding channels of its own already.
    existing: bool,
    /// The room's Chat as a public room's opens: "Channels" with #general.
    room_seeded: bool,
    /// Messages Bo wrote before the bridge first ran: channel, text, when
    /// (Unix ms).
    history: &'static [(&'static str, &'static str, u64)],
    /// Messages an earlier bridge of this bot posted through its webhook,
    /// before this one first ran: channel, shown name, text, when.
    bridged_before: &'static [(&'static str, &'static str, &'static str, u64)],
}

/// A Discord server laid out before its bridge: a category "Channels" with #general and a
/// voice channel, three channels at the top, and an empty category.
const CATEGORY_CHANNELS: &str = fake_discord::CATEGORY_CHAT;
const CHANNEL_ROADMAP: &str = "1104";
const CHANNEL_PROGRESS: &str = "1105";
const CATEGORY_EMPTY: &str = "1106";

fn existing_layout() -> Vec<Value> {
    let channel = |id: &str, kind: u8, name: &str, parent: Option<&str>, position: u8| serde_json::json!({"id": id, "type": kind, "name": name, "parent_id": parent, "position": position, "topic": null});
    vec![
        channel(CATEGORY_CHANNELS, 4, "Channels", None, 0),
        channel(CHANNEL_GENERAL, 0, "general", Some(CATEGORY_CHANNELS), 0),
        channel(
            fake_discord::CHANNEL_VOICE,
            2,
            "voice",
            Some(CATEGORY_CHANNELS),
            0,
        ),
        channel(fake_discord::CHANNEL_GOAL, 0, "goal", None, 0),
        channel(CHANNEL_ROADMAP, 0, "roadmap", None, 1),
        channel(CHANNEL_PROGRESS, 0, "progress", None, 2),
        channel(CATEGORY_EMPTY, 4, "chat", None, 1),
    ]
}

/// A room whose Chat has Ada in it, bridged to a fresh fake server the
/// bridge has adopted.
struct Bridge {
    fake: FakeDiscord,
    room: Room,
    blobs: Arc<Blobs>,
    /// The Chat channel Discord's #general is bridged to.
    general: String,
    ada: String,
    _relay: tilo_relay::daemon::Relay,
    _scratch: tempfile::TempDir,
}

impl Bridge {
    async fn start() -> (Self, BotRunner) {
        Self::start_with(Setup::default()).await
    }

    /// The bridge as `setup` has it: confined to one channel both in its
    /// record and in its worker, opted into more, or on a server laid out before it.
    async fn start_with(setup: Setup) -> (Self, BotRunner) {
        let only = setup.only;
        let fake = if setup.existing {
            FakeDiscord::start_with_channels(existing_layout()).await
        } else {
            FakeDiscord::start().await
        }
        .unwrap();
        let bo = User::new("3001", "Bo");
        for (channel, text, at_ms) in setup.history {
            fake.post_written(channel, &bo, text, *at_ms);
        }
        for (channel, name, text, at_ms) in setup.bridged_before {
            let (webhook, _) = fake.create_webhook(channel);
            fake.post_written_through(&webhook, channel, name, text, *at_ms);
        }
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        // The host decides what members and the bot change against the
        // surfaces of the apps it runs, as a room host does.
        let _ = AppCatalogue::native();
        tilo_relay::surfaces::decide_receipt_from_registry("http://127.0.0.1:9");
        let relay = tilo_relay::daemon::Relay::start_with_relay_mode(
            tilo_relay::RelayConfig {
                state_root: scratch.path().join("relay"),
                ..Default::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        let rooms = relay.rooms().unwrap().clone();
        let room = Room {
            id: rooms.create("Bridged").await.unwrap().room,
            rooms,
            cert: cert.clone(),
            catalogue: AppCatalogue::native(),
        };
        let owner = hex::encode(cert.user_public_key);
        let ada = hex::encode(ADA);

        room.rooms
            .prepare_application(&room.id, &cert, &room.catalogue, CHAT, "")
            .await
            .unwrap();
        room.chat_as({
            let (owner, ada) = (owner.clone(), ada.clone());
            move |doc| {
                lay_out_with_general_and_lounge(doc, &owner)?;
                chat::demo::introduce(doc, &ada, "Ada", "AD").map_err(|error| error.to_string())?;
                if setup.existing {
                    seed_channels(doc, &owner)?;
                }
                if setup.room_seeded {
                    seed_as_a_room(doc, &owner)?;
                }
                Ok(())
            }
        })
        .await;
        if setup.existing {
            // Someone wrote in #help before the bridge came.
            let help = room
                .chat()
                .await
                .channels
                .iter()
                .find(|(_, c)| c.name == "help")
                .map(|(id, _)| id.as_str().to_owned())
                .expect("the room's #help");
            room.part_as(&ada, chat::demo::post(&help, "where do I start?", None))
                .await;
        }

        room.rooms
            .prepare_application(&room.id, &cert, &room.catalogue, APP, "")
            .await
            .unwrap();
        room.rooms
            .author_application(&room.id, &cert, &room.catalogue, APP, "", {
                let owner = owner.clone();
                move |doc| {
                    let mut replica = Replica::<Guild>::from_doc(std::mem::take(doc))
                        .as_invoker(&owner)
                        .in_app(APP, "Discord");
                    let applied = replica
                        .apply_confirmed(Guild::CONNECT.with(guild::Connect {
                            guild_id: GUILD.into(),
                            name: "Fake".into(),
                            only_channel: only.map(str::to_owned),
                            mirror_channels: Some(setup.mirror),
                            map_roles: Some(setup.roles),
                        }))
                        .map(drop);
                    std::mem::swap(doc, replica.doc_mut());
                    applied.map_err(|error| error.to_string())
                }
            })
            .await
            .unwrap();

        let secrets = ui::secrets::SecretStore::memory(APP);
        secrets
            .item("bot-token", "default")
            .put(TOKEN.as_bytes())
            .await
            .unwrap();
        let blobs = Arc::new(Blobs::default());
        let process = ConnectorProcess::start(
            APP,
            WorkerCommand {
                program: tilo_test_env::package_binary!("discord-worker"),
                args: Vec::new(),
                env: [
                    ("DISCORD_API_BASE".into(), fake.api_base()),
                    ("DISCORD_CDN_BASE".into(), fake.cdn_base()),
                    ("DISCORD_ALLOW_LOOPBACK".into(), "1".into()),
                ]
                .into_iter()
                .chain(only.map(|channel| ("DISCORD_CHANNEL_ID".into(), channel.to_owned())))
                .collect(),
                metrics_file: None,
            },
            &scratch.path().join("connector"),
            secrets,
            Arc::clone(&blobs) as Arc<dyn BlobAccess>,
        )
        .await
        .unwrap();

        let program = concat!(env!("CARGO_MANIFEST_DIR"), "/../bridge/program.json");
        let bot = installed::install(&scratch.path().join("bot"), program, serde_json::Map::new())
            .await
            .unwrap();
        let certifier = HostRootCertifier::new(SigningKey::from_bytes(&[1; 32]));
        keep_member(&room.rooms, &room.id, &cert, &certifier, &bot)
            .await
            .unwrap();
        let mut runner = BotRunner::new(
            room.rooms.clone(),
            room.id.clone(),
            cert.clone(),
            certifier.device_key(&bot.installation),
            bot,
            BTreeMap::from([(APP.to_owned(), Arc::new(process))]),
            scratch.path().join("progress.json"),
        )
        .unwrap();

        // The server is adopted into Chat, and the bridge makes a webhook
        // for each text channel.
        settle(&mut runner, "adopt", || async {
            let guild = room.guild().await;
            (guild.ownership == Ownership::Adopted
                && fake
                    .webhooks()
                    .iter()
                    .any(|(_, channel)| channel == CHANNEL_GENERAL))
            .then_some(())
        })
        .await;
        let general = settle(&mut runner, "general mapped", || async {
            room.guild()
                .await
                .by_discord(MappedKind::Channel, CHANNEL_GENERAL)
                .map(|mapping| mapping.tilo.clone())
        })
        .await;
        let bridge = Self {
            fake,
            room,
            blobs,
            general,
            ada,
            _relay: relay,
            _scratch: scratch,
        };
        (bridge, runner)
    }

    /// Ada does `invoke` in Chat from her own device.
    async fn as_ada(&self, invoke: Invoke<chat::Chat>) {
        let ada = self.ada.clone();
        self.room
            .chat_as(move |doc| {
                chat::demo::apply_as(doc, &ada, invoke).map_err(|error| error.to_string())
            })
            .await;
    }

    /// Ada does `invoke` to Chat's messages from her own device.
    async fn as_ada_on_messages(&self, invoke: impl Into<Invoke<chat::parts::ChatPart>>) {
        self.room.part_as(&self.ada, invoke.into()).await;
    }

    /// Ada reacts to `message` with `emoji` (`added`), or takes it back: the
    /// framework's field edit of the message's reactions, as her page makes
    /// it.
    async fn ada_reacts(&self, message: &str, emoji: &str, added: bool) {
        let reactions = chat::parts::ChatPart::paths()
            .messages
            .item(&Id::from_raw(message))
            .reactions;
        let edit = if added {
            reactions.react(emoji)
        } else {
            reactions.unreact(emoji)
        };
        self.as_ada_on_messages(edit).await;
    }

    /// The reactions on Chat message `id`: emoji, persona and name.
    async fn reactions_on(&self, id: &str) -> Vec<(String, Option<String>, Option<String>)> {
        chat::messages::read_part(&self.room.part().await)
            .messages
            .iter()
            .find(|(message, _)| message.as_str() == id)
            .map(|(_, message)| {
                message
                    .reactions
                    .iter()
                    .map(|r| {
                        (
                            r.emoji.0.clone(),
                            r.persona.as_ref().map(|p| p.0.clone()),
                            r.name.as_ref().map(|n| n.0.clone()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ada posts `text` in #general; the new message's id.
    async fn ada_says(&self, text: &str) -> String {
        self.room
            .part_as(&self.ada, chat::demo::post(&self.general, text, None))
            .await
            .pop()
            .expect("the post is in a part")
    }

    async fn chat_message(&self, text: &str) -> Option<(String, chat::record::Message)> {
        chat::messages::read_part(&self.room.part().await)
            .messages
            .iter()
            .find(|(_, message)| message.text == text)
            .map(|(id, message)| (id.as_str().to_owned(), message.clone()))
    }

    /// The Discord message the bridge mapped Chat message `tilo` to.
    async fn discord_of(&self, kind: MappedKind, tilo: &str) -> Option<String> {
        self.room
            .guild()
            .await
            .mapping(kind, tilo)
            .map(|mapping| mapping.discord.clone())
    }

    /// The Discord message in #general or any thread with `content`.
    fn in_discord(&self, content: &str) -> Option<Value> {
        self.fake
            .channels()
            .iter()
            .filter_map(|channel| channel["id"].as_str().map(str::to_owned))
            .chain([CHANNEL_GENERAL.to_owned()])
            .flat_map(|channel| self.fake.messages_in(&channel))
            .find(|message| message["content"] == content)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_crosses_the_bridge_both_ways_under_its_authors_name() {
    let (bridge, mut runner) = Bridge::start().await;

    // Tilo to Discord: Ada's post arrives through the webhook as Ada.
    bridge.ada_says("hello from Tilo").await;
    let sent = settle(&mut runner, "Tilo to Discord", || async {
        bridge
            .fake
            .messages_in(CHANNEL_GENERAL)
            .into_iter()
            .find(|message| message["content"] == "hello from Tilo")
    })
    .await;
    assert_eq!(sent["author"]["username"], "Ada", "{sent:#}");
    assert!(sent["webhook_id"].is_string(), "{sent:#}");

    // Discord to Tilo: Bo's post lands in Chat under Bo's name, and the
    // webhook echo of Ada's post comes back as nothing.
    let bo = User::new("3001", "Bo");
    bridge.fake.post(CHANNEL_GENERAL, &bo, "hello from Discord");
    settle(&mut runner, "Discord to Tilo", || async {
        bridge.chat_message("hello from Discord").await.map(drop)
    })
    .await;
    let bridged = bridge.room.bridged().await;
    assert_eq!(
        bridged,
        [("hello from Discord".to_owned(), "Bo".to_owned())],
        "only Bo's message is bridged, under his name"
    );
    let texts: Vec<String> = bridge
        .room
        .part_messages()
        .await
        .messages
        .values()
        .map(|message| message.text.clone())
        .collect();
    assert_eq!(
        texts
            .iter()
            .filter(|text| *text == "hello from Tilo")
            .count(),
        1,
        "the echo of Ada's post is not posted again: {texts:?}"
    );
}

/// Ada links her Discord account: the room's host fetches its avatar small,
/// for her Tilo profile to wear, and what she writes in Tilo shows in
/// Discord as her name with her handle, under the same picture.
#[tokio::test(flavor = "multi_thread")]
async fn a_linked_account_lends_its_picture_and_handle() {
    use discord_wire::event::{link_check, link_proof};
    let (bridge, mut runner) = Bridge::start().await;
    let png = picture(7);
    let _ = bridge
        .fake
        .put_cdn("avatars/4001/f00d.png", "image/png", &png);
    // The bridge offers the account, as `/link` has it, and Ada takes it.
    bridge
        .room
        .rooms
        .author_application(
            &bridge.room.id,
            &bridge.room.cert,
            &bridge.room.catalogue,
            APP,
            "",
            {
                let owner = hex::encode(bridge.room.cert.user_public_key);
                move |doc| {
                    let mut replica = Replica::<Guild>::from_doc(std::mem::take(doc))
                        .as_bot_for("bridge-test", &owner)
                        .in_app(APP, "Discord");
                    let applied = replica
                        .apply(Guild::OFFER_LINK.with(guild::OfferLink {
                            check: link_check(&link_proof("ADA-TOKEN")),
                            discord_user: "4001".into(),
                            user_name: "ada.l".into(),
                            display_name: None,
                            avatar: Some("f00d".into()),
                            interaction: None,
                        }))
                        .map(drop);
                    std::mem::swap(doc, replica.doc_mut());
                    applied.map_err(|error| format!("{error:?}"))
                }
            },
        )
        .await
        .unwrap();
    bridge
        .room
        .rooms
        .author_application(
            &bridge.room.id,
            &bridge.room.cert,
            &bridge.room.catalogue,
            APP,
            "",
            {
                let ada = bridge.ada.clone();
                move |doc| {
                    let mut replica = Replica::<Guild>::from_doc(std::mem::take(doc))
                        .as_invoker(&ada)
                        .in_app(APP, "Discord");
                    let applied = replica
                        .apply_confirmed(Guild::COMPLETE_LINK.with(guild::CompleteLink {
                            proof: link_proof("ADA-TOKEN"),
                        }))
                        .map(drop);
                    std::mem::swap(doc, replica.doc_mut());
                    applied.map_err(|error| error.to_string())
                }
            },
        )
        .await
        .unwrap();
    let photo = settle(&mut runner, "the link's picture", || async {
        let guild = bridge.room.guild().await;
        let link = guild.links.find(&Id::from_raw("4001"))?;
        link.photo.clone().zip(link.photo_avatar.clone())
    })
    .await;
    use base64::Engine as _;
    assert_eq!(
        photo,
        (
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&png)
            ),
            "f00d".to_owned()
        ),
        "the avatar as the profile's picture, of the hash it is"
    );
    let fetched = bridge
        .fake
        .requests()
        .into_iter()
        .find(|request| request.path.contains("/avatars/4001/"))
        .expect("the room's host fetched the avatar");
    assert!(
        fetched
            .query
            .iter()
            .any(|(key, value)| key == "size" && value == "64"),
        "small: {:?}",
        fetched.query
    );

    bridge.ada_says("hello as myself").await;
    let sent = settle(&mut runner, "Tilo to Discord", || async {
        bridge.in_discord("hello as myself")
    })
    .await;
    assert_eq!(sent["author"]["username"], "Ada (@ada.l)", "{sent:#}");
    let executed = bridge
        .fake
        .requests_to("POST", "/webhooks/")
        .into_iter()
        .find(|request| request.json["content"] == "hello as myself")
        .expect("the webhook was executed")
        .json;
    assert!(
        executed["avatar_url"]
            .as_str()
            .is_some_and(|url| url.contains("/avatars/4001/f00d.")),
        "the webhook wears the same picture: {executed:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_ada_does_to_her_messages_in_chat_follows_in_discord() {
    let (bridge, mut runner) = Bridge::start().await;
    let first = bridge.ada_says("first").await;
    let sent = settle(&mut runner, "post", || async {
        bridge.discord_of(MappedKind::Message, &first).await
    })
    .await;

    // An edit rewrites the webhook's message.
    bridge
        .as_ada_on_messages(chat::parts::ChatPart::EDIT_MESSAGE.with(
            chat::messages::chat_part::EditMessage {
                message: Id::from_raw(&first),
                text: "first, edited".into(),
                body: None,
                mentions: None,
            },
        ))
        .await;
    settle(&mut runner, "edit", || async {
        (bridge.fake.message(&sent).unwrap()["content"] == "first, edited").then_some(())
    })
    .await;

    // A reaction is the bot's reaction on it.
    bridge.ada_reacts(&first, "👍", true).await;
    settle(&mut runner, "reaction", || async {
        bridge
            .fake
            .bot_reactions()
            .contains(&(sent.clone(), "👍".to_owned()))
            .then_some(())
    })
    .await;

    // A reply grows a thread from it, holding the reply.
    bridge
        .as_ada_on_messages(chat::demo::post(&bridge.general, "a reply", Some(&first)))
        .await;
    let reply = settle(&mut runner, "reply", || async {
        bridge.in_discord("a reply")
    })
    .await;
    let thread = bridge
        .discord_of(MappedKind::Thread, &first)
        .await
        .expect("the thread is mapped to its root");
    assert_eq!(reply["channel_id"], thread.as_str(), "{reply:#}");
    assert_eq!(reply["author"]["username"], "Ada", "{reply:#}");

    // A file is uploaded with the message.
    let bytes = b"the minutes".to_vec();
    let blob = bridge.blobs.store(&bytes).unwrap();
    bridge
        .as_ada_on_messages(
            chat::parts::ChatPart::POST.with(chat::messages::chat_part::Post {
                message: chat::messages::NewMessage {
                    channel: Id::from_raw(&bridge.general),
                    text: "the notes".into(),
                    parent: None,
                    body: None,
                    attachment: Some(chat::record::AttachmentUpdate {
                        kind: "file".into(),
                        name: "notes.txt".into(),
                        blob,
                        width: 0,
                        height: 0,
                        size: bytes.len() as u64,
                    }),
                    mentions: None,
                    written: None,
                },
            }),
        )
        .await;
    let file = settle(&mut runner, "file", || async {
        bridge.in_discord("the notes")
    })
    .await;
    assert_eq!(file["attachments"][0]["filename"], "notes.txt", "{file:#}");

    // A delete takes the message out of Discord.
    bridge
        .as_ada_on_messages(chat::parts::ChatPart::DELETE_MESSAGE.with(
            chat::messages::chat_part::DeleteMessage {
                message: Id::from_raw(&first),
            },
        ))
        .await;
    settle(&mut runner, "delete", || async {
        bridge.fake.message(&sent).is_none().then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_bo_does_to_his_messages_in_discord_follows_in_chat() {
    let (bridge, mut runner) = Bridge::start().await;
    let bo = User::new("3001", "Bo");
    let posted = bridge.fake.post(CHANNEL_GENERAL, &bo, "from Bo");
    let (message, _) = settle(&mut runner, "post", || async {
        bridge.chat_message("from Bo").await
    })
    .await;

    bridge.fake.edit(&posted, "from Bo, edited");
    settle(&mut runner, "edit", || async {
        bridge.chat_message("from Bo, edited").await.map(drop)
    })
    .await;

    bridge.fake.react(CHANNEL_GENERAL, &posted, &bo, "🎉");
    settle(&mut runner, "reaction", || async {
        let (_, edited) = bridge.chat_message("from Bo, edited").await?;
        edited
            .reactions
            .iter()
            .any(|reacted| reacted.emoji.0 == "🎉")
            .then_some(())
    })
    .await;

    bridge
        .fake
        .reply(CHANNEL_GENERAL, &bo, "Bo's reply", &posted);
    let (_, reply) = settle(&mut runner, "reply", || async {
        bridge.chat_message("Bo's reply").await
    })
    .await;
    assert_eq!(
        reply.parent.as_ref().map(Id::as_str),
        Some(message.as_str()),
        "a Discord reply answers its message in a thread"
    );

    bridge.fake.post_file(
        CHANNEL_GENERAL,
        &bo,
        "Bo's notes",
        ("notes.txt", "text/plain", b"the minutes"),
    );
    let (_, file) = settle(&mut runner, "file", || async {
        bridge.chat_message("Bo's notes").await
    })
    .await;
    let attachment = format!("{:?}", file.attachment);
    assert!(attachment.contains("notes.txt"), "{attachment}");

    // A picture lands as an image of its size: the bot queues fetching it,
    // its connector fetches it, and the bot settles and clears the entry.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&640_u32.to_be_bytes());
    png.extend_from_slice(&480_u32.to_be_bytes());
    bridge.fake.post_file(
        CHANNEL_GENERAL,
        &bo,
        "Bo's photo",
        ("photo.png", "image/png", &png),
    );
    let (_, photo) = settle(&mut runner, "picture", || async {
        bridge.chat_message("Bo's photo").await
    })
    .await;
    assert!(
        matches!(
            &photo.attachment,
            Some(chat::model::Attachment::Image { name, w: 640, h: 480, .. }) if name == "photo.png"
        ),
        "{:?}",
        photo.attachment
    );

    bridge.fake.delete(&posted, None);
    settle(&mut runner, "delete", || async {
        (!bridge
            .room
            .part_messages()
            .await
            .messages
            .iter()
            .any(|(id, _)| id.as_str() == message))
        .then_some(())
    })
    .await;
}

/// Reactions cross both ways, each Discord member their own reaction in
/// Tilo, the server's custom emoji standing for the workspace's emoji of
/// the same name, and nothing mirrored in going back out.
#[tokio::test(flavor = "multi_thread")]
async fn reactions_cross_both_ways_and_custom_emoji_by_name() {
    let (bridge, mut runner) = Bridge::start().await;
    // The server has an emoji `tilo_hello`; the bridge learns it.
    bridge.fake.set_emojis(&[("tilo_hello", "55", false)]);
    settle(&mut runner, "emoji observed", || async {
        (bridge.room.guild().await.emoji("tilo_hello") == Some("55")).then_some(())
    })
    .await;
    let first = bridge.ada_says("react to me").await;
    let sent = settle(&mut runner, "post", || async {
        bridge.discord_of(MappedKind::Message, &first).await
    })
    .await;

    // Tilo to Discord: a Unicode emoji as itself, a workspace emoji as the
    // server's emoji of its name.
    bridge.ada_reacts(&first, "👍", true).await;
    bridge.ada_reacts(&first, ":tilo_hello:", true).await;
    settle(&mut runner, "Tilo reactions in Discord", || async {
        (bridge.fake.bot_reactions_on(&sent) == ["👍", "tilo_hello:55"]).then_some(())
    })
    .await;
    // A workspace emoji the server has none of stays in Tilo.
    bridge.ada_reacts(&first, ":pizza:", true).await;
    let calls = quiet(&mut runner, &bridge, "after pizza").await;
    assert!(
        !calls.iter().any(|call| call.contains("/reactions/")),
        "{calls:?}"
    );
    assert_eq!(bridge.fake.bot_reactions_on(&sent), ["👍", "tilo_hello:55"]);

    // Discord to Tilo: each member's own reaction, under their name; a
    // custom emoji as its `:name:`.
    let bo = User::new("3001", "Bo");
    let cy = User::new("3002", "Cy");
    bridge.fake.react(CHANNEL_GENERAL, &sent, &bo, "🎉");
    bridge.fake.react(CHANNEL_GENERAL, &sent, &cy, "🎉");
    bridge.fake.react(CHANNEL_GENERAL, &sent, &bo, ":party:");
    let bridged = |reactions: &[(String, Option<String>, Option<String>)]| {
        let mut theirs: Vec<(String, String, String)> = reactions
            .iter()
            .filter_map(|(emoji, persona, name)| {
                Some((emoji.clone(), persona.clone()?, name.clone()?))
            })
            .collect();
        theirs.sort();
        theirs
    };
    settle(&mut runner, "Discord reactions in Tilo", || async {
        let theirs = bridged(&bridge.reactions_on(&first).await);
        (theirs
            == [
                (":party:".into(), "discord/3001".into(), "Bo".into()),
                ("🎉".into(), "discord/3001".into(), "Bo".into()),
                ("🎉".into(), "discord/3002".into(), "Cy".into()),
            ])
        .then_some(())
    })
    .await;
    // What came in from Discord does not go back out as the bot's.
    quiet(&mut runner, &bridge, "after Bo and Cy").await;
    assert_eq!(bridge.fake.bot_reactions_on(&sent), ["👍", "tilo_hello:55"]);

    // Taking reactions back follows on both sides.
    bridge.fake.unreact(CHANNEL_GENERAL, &sent, &bo, "🎉");
    settle(&mut runner, "Bo's reaction gone", || async {
        let theirs = bridged(&bridge.reactions_on(&first).await);
        (theirs
            == [
                (":party:".into(), "discord/3001".into(), "Bo".into()),
                ("🎉".into(), "discord/3002".into(), "Cy".into()),
            ])
        .then_some(())
    })
    .await;
    bridge.ada_reacts(&first, "👍", false).await;
    bridge.ada_reacts(&first, ":tilo_hello:", false).await;
    settle(&mut runner, "Ada's reactions gone from Discord", || async {
        bridge.fake.bot_reactions_on(&sent).is_empty().then_some(())
    })
    .await;
}

/// Run `runner` until everything already set going has played out; the
/// REST calls made meanwhile. Bo says `marker` in #general: Discord delivers
/// it after every event the bridge's own calls caused, and the runner takes
/// events in order and finishes a run's calls before the next run, so once
/// the marker is in Chat nothing that came before it is still pending.
async fn quiet(runner: &mut BotRunner, bridge: &Bridge, marker: &str) -> Vec<String> {
    let start = bridge.fake.requests().len();
    bridge
        .fake
        .post(CHANNEL_GENERAL, &User::new("3001", "Bo"), marker);
    settle(runner, marker, || async {
        bridge.chat_message(marker).await.map(drop)
    })
    .await;
    calls(bridge.fake.requests().get(start..).unwrap_or_default())
}

fn calls(requests: &[fake_discord::Recorded]) -> Vec<String> {
    requests
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect()
}

/// The calls that changed something in Discord.
fn changes(fake: &FakeDiscord) -> Vec<String> {
    calls(&fake.requests())
        .into_iter()
        .filter(|call| !call.starts_with("GET "))
        .collect()
}

/// A webhook in each of `channels`, and the bot's own `/link` command,
/// which belongs to the bot rather than the server.
fn webhooks_of(channels: &[&str]) -> std::collections::BTreeSet<String> {
    channels
        .iter()
        .map(|channel| format!("POST /channels/{channel}/webhooks"))
        .chain([format!(
            "PUT /applications/{}/guilds/{GUILD}/commands",
            fake_discord::BOT_USER
        )])
        .collect()
}

/// The server's layout and roles, to compare before and after.
fn layout(fake: &FakeDiscord) -> (Vec<Value>, Vec<Value>) {
    (fake.channels(), fake.roles())
}

#[tokio::test(flavor = "multi_thread")]
async fn adopting_a_server_changes_nothing_in_it_but_webhooks() {
    let (bridge, mut runner) = Bridge::start().await;
    quiet(&mut runner, &bridge, "adopted").await;
    let changed: std::collections::BTreeSet<String> = changes(&bridge.fake).into_iter().collect();
    assert_eq!(
        changed,
        webhooks_of(&[CHANNEL_GENERAL, fake_discord::CHANNEL_GOAL])
    );
    // Idle: nothing changes, so nothing is called.
    let idle = quiet(&mut runner, &bridge, "idle").await;
    assert!(idle.is_empty(), "{idle:#?}");
}

/// A server laid out before its bridge, as the bridge first met it, and a room whose Chat
/// already had "Channels" with #general, #help and #show-and-tell: the
/// bridge once renamed #general to general-2, made a second "Channels",
/// copied the room's channels in and narrowed `@everyone`. Adopted now,
/// the server is left as it was but for the webhooks.
#[tokio::test(flavor = "multi_thread")]
async fn an_existing_server_is_adopted_as_it_is() {
    let fake_before = FakeDiscord::start_with_channels(existing_layout())
        .await
        .unwrap();
    let before = layout(&fake_before);
    drop(fake_before);
    let (bridge, mut runner) = Bridge::start_with(Setup {
        existing: true,
        ..Setup::default()
    })
    .await;
    quiet(&mut runner, &bridge, "adopted").await;
    let changed: std::collections::BTreeSet<String> = changes(&bridge.fake).into_iter().collect();
    assert_eq!(
        changed,
        webhooks_of(&[
            CHANNEL_GENERAL,
            fake_discord::CHANNEL_GOAL,
            CHANNEL_ROADMAP,
            CHANNEL_PROGRESS
        ]),
        "only webhooks are made"
    );
    let strip = |channels: Vec<Value>| -> Vec<Value> {
        channels
            .into_iter()
            .map(|c| {
                serde_json::json!({
                    "id": c["id"], "name": c["name"], "parent": c["parent_id"],
                    "overwrites": c["permission_overwrites"],
                })
            })
            .collect()
    };
    let after = layout(&bridge.fake);
    assert_eq!(strip(after.0), strip(before.0), "the layout is as it was");
    assert_eq!(after.1, before.1, "the roles are as they were");

    // Chat has Discord's layout joined with its own, nothing doubled. Its
    // own #help, written in, stays in Tilo; #show-and-tell and the room's
    // own "Voice" with its Lounge voice channel, none of which the server
    // has and nobody wrote in, go.
    let chat = bridge.room.chat().await;
    let mut names: Vec<&str> = chat.channels.values().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["general", "goal", "help", "progress", "roadmap", "voice"]
    );
    let mut categories: Vec<&str> = chat.categories.values().map(|c| c.name.as_str()).collect();
    categories.sort_unstable();
    assert_eq!(categories, ["Channels", "chat"]);
    let guild = bridge.room.guild().await;
    assert_eq!(guild.tilo_only.len(), 1, "{:?}", guild.tilo_only);

    // And messages cross.
    bridge.ada_says("hello there").await;
    settle(&mut runner, "Tilo to #general", || async {
        bridge.in_discord("hello there").map(drop)
    })
    .await;
}

/// When Bo wrote the server's first messages, long before the bridge ran.
const WRITTEN_MS: u64 = 1_751_317_200_000;

/// A public room's Chat opens with "Channels" › #general; the server has
/// #goal at the top and #general and a voice channel under "chat", and
/// messages written before the bridge first ran. Adopted, the room lists
/// exactly the server's channels under the same categories, and holds its
/// messages under their author's name, at the time they were written.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_room_takes_on_its_servers_layout_and_history() {
    let (bridge, mut runner) = Bridge::start_with(Setup {
        room_seeded: true,
        history: &[
            (
                fake_discord::CHANNEL_GOAL,
                "What we are building",
                WRITTEN_MS,
            ),
            (CHANNEL_GENERAL, "Hi?", WRITTEN_MS + 60_000),
        ],
        ..Setup::default()
    })
    .await;
    let hi = settle(&mut runner, "the history is in Chat", || async {
        bridge.chat_message("What we are building").await?;
        bridge.chat_message("Hi?").await
    })
    .await;
    assert_eq!(hi.1.channel.as_str(), bridge.general);
    assert_eq!(
        hi.1.ts,
        i64::try_from((WRITTEN_MS + 60_000) / 1000).unwrap(),
        "shown when it was written in Discord"
    );
    assert_eq!(
        hi.1.appearance.as_ref().map(|a| a.name.as_str()),
        Some("Bo")
    );

    // Exactly the server's layout: "Channels" is gone, #general is under
    // "chat", and nothing is Tilo's alone.
    let chat = bridge.room.chat().await;
    let category = |id: &Option<Id>| {
        id.as_ref()
            .and_then(|id| chat.categories.get(id))
            .map(|c| c.name.clone())
    };
    let mut layout: Vec<(String, Option<String>)> = chat
        .channels
        .values()
        .map(|c| (c.name.clone(), category(&c.category)))
        .collect();
    layout.sort();
    assert_eq!(
        layout,
        [
            ("general".to_owned(), Some("chat".to_owned())),
            ("goal".to_owned(), None),
            ("voice".to_owned(), Some("chat".to_owned())),
        ]
    );
    let categories: Vec<&str> = chat.categories.values().map(|c| c.name.as_str()).collect();
    assert_eq!(categories, ["chat"]);
    assert!(bridge.room.guild().await.tilo_only.is_empty());
}

/// A room meeting a server that a bridge of the same bot posted in before
/// reads its history before it adopts that bridge's webhook: what the
/// bridge posted there is never taken in as written in Discord.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_room_leaves_out_what_its_bot_posted_before() {
    let (bridge, mut runner) = Bridge::start_with(Setup {
        room_seeded: true,
        bridged_before: &[(CHANNEL_GENERAL, "Ada", "from the Lounge", WRITTEN_MS)],
        history: &[(CHANNEL_GENERAL, "Hi?", WRITTEN_MS + 60_000)],
        ..Setup::default()
    })
    .await;
    settle(&mut runner, "Bo's history is in Chat", || async {
        bridge.chat_message("Hi?").await
    })
    .await;
    assert!(
        bridge.chat_message("from the Lounge").await.is_none(),
        "the bridge's own post came back in as Discord's"
    );
}

/// Opted in, the bridge adds roles of its own and permissions for them, and
/// makes in Discord only the channels Tilo makes later; still nothing of
/// the server's own is changed.
#[tokio::test(flavor = "multi_thread")]
async fn opted_in_the_bridge_adds_its_own_roles_and_new_channels_only() {
    let (bridge, mut runner) = Bridge::start_with(Setup {
        existing: true,
        mirror: true,
        roles: true,
        ..Setup::default()
    })
    .await;
    quiet(&mut runner, &bridge, "adopted").await;
    let bridge_roles: Vec<String> = bridge
        .fake
        .roles()
        .into_iter()
        .filter(|r| r["managed"] != true)
        .filter(|r| r["name"].as_str().is_some_and(|n| n.starts_with("Tilo ")))
        .filter_map(|r| r["id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(bridge_roles.len(), 5);
    for call in changes(&bridge.fake) {
        let allowed = call.ends_with("/webhooks")
            || call.ends_with("/commands")
            || call == format!("POST /guilds/{GUILD}/roles")
            || (call.starts_with("PUT /channels/")
                && bridge_roles
                    .iter()
                    .any(|role| call.ends_with(&format!("/permissions/{role}"))));
        assert!(allowed, "{call}");
    }

    let channels = bridge
        .room
        .chat()
        .await
        .categories
        .iter()
        .find(|(_, c)| c.name == "Channels")
        .map(|(id, _)| id.clone())
        .unwrap();
    bridge
        .as_ada(
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                name: "ideas".into(),
                category: Some(channels),
                kind: None,
                topic: None,
            }),
        )
        .await;
    let made = settle(&mut runner, "#ideas in Discord", || async {
        bridge
            .fake
            .channels()
            .into_iter()
            .find(|c| c["name"] == "ideas")
    })
    .await;
    assert_eq!(made["parent_id"], CATEGORY_CHANNELS);
    assert!(
        bridge
            .fake
            .channels()
            .iter()
            .all(|c| c["name"] != "help" && c["name"] != "show-and-tell"),
        "what Tilo had before the adoption stays Tilo's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_of_one_channel_touches_nothing_else_in_the_server() {
    let (bridge, mut runner) = Bridge::start_with(Setup {
        only: Some(CHANNEL_GENERAL),
        ..Setup::default()
    })
    .await;
    quiet(&mut runner, &bridge, "adopted").await;

    // Only #general came into Chat, and the one change in the server is
    // its webhook: no roles, permissions, layout or slash commands.
    let guild = bridge.room.guild().await;
    let mapped: Vec<_> = guild
        .map
        .iter()
        .filter(|m| matches!(m.kind, MappedKind::Channel | MappedKind::Category))
        .map(|m| m.discord.clone())
        .collect();
    assert_eq!(mapped, [CHANNEL_GENERAL]);
    let changes: Vec<String> = calls(&bridge.fake.requests())
        .into_iter()
        .filter(|call| !call.starts_with("GET "))
        .collect();
    assert_eq!(
        changes,
        [format!("POST /channels/{CHANNEL_GENERAL}/webhooks")]
    );

    // Messages cross both ways there; what is said elsewhere stays there.
    bridge.ada_says("only here").await;
    settle(&mut runner, "Tilo to #general", || async {
        bridge.in_discord("only here").map(drop)
    })
    .await;
    let bo = User::new("3001", "Bo");
    bridge
        .fake
        .post(fake_discord::CHANNEL_GOAL, &bo, "elsewhere");
    bridge.fake.post(CHANNEL_GENERAL, &bo, "here");
    settle(&mut runner, "#general to Tilo", || async {
        bridge.chat_message("here").await.map(drop)
    })
    .await;
    // #goal's message came first, so it was handled before "here" was.
    assert!(bridge.chat_message("elsewhere").await.is_none());
}

/// Lay the room's Chat out as its owner did before any bridge ran:
/// "Channels" with #general, and "Voice" with the Lounge, in place of the
/// `#chat` and `#voice` a room opens with.
fn lay_out_with_general_and_lounge(doc: &mut AutoCommit, owner: &str) -> Result<(), String> {
    let chat = chat::Chat::read(doc).map_err(|error| error.to_string())?;
    for channel in chat.channels.ids().cloned() {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::DELETE_CHANNEL.with(chat::record::chat::DeleteChannel { channel }),
        )
        .map_err(|error| error.to_string())?;
    }
    for category in chat.categories.ids().cloned() {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::DELETE_CATEGORY.with(chat::record::chat::DeleteCategory { category }),
        )
        .map_err(|error| error.to_string())?;
    }
    for (category, channel, kind) in [
        ("Channels", "general", chat::model::ChannelKind::Text),
        ("Voice", "Lounge", chat::model::ChannelKind::Voice),
    ] {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::CREATE_CATEGORY.with(chat::record::chat::CreateCategory {
                name: category.into(),
            }),
        )
        .map_err(|error| error.to_string())?;
        let made = chat::Chat::read(doc)
            .map_err(|error| error.to_string())?
            .categories
            .iter()
            .find(|(_, c)| c.name == category)
            .map(|(id, _)| id.clone());
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                name: channel.into(),
                category: made,
                kind: Some(kind),
                topic: None,
            }),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Leave the room's Chat as a public room's opened: "Channels" with
/// #general, and nothing else.
fn seed_as_a_room(doc: &mut AutoCommit, owner: &str) -> Result<(), String> {
    let chat = chat::Chat::read(doc).map_err(|error| error.to_string())?;
    let channels: Vec<Id> = chat
        .channels
        .iter()
        .filter(|(_, c)| c.name != "general")
        .map(|(id, _)| id.clone())
        .collect();
    let categories: Vec<Id> = chat
        .categories
        .iter()
        .filter(|(_, c)| c.name != "Channels")
        .map(|(id, _)| id.clone())
        .collect();
    for channel in channels {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::DELETE_CHANNEL.with(chat::record::chat::DeleteChannel { channel }),
        )
        .map_err(|error| error.to_string())?;
    }
    for category in categories {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::DELETE_CATEGORY.with(chat::record::chat::DeleteCategory { category }),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The room's own "Channels" with #general, #help and #show-and-tell, made
/// by its owner before any bridge ran.
fn seed_channels(doc: &mut AutoCommit, owner: &str) -> Result<(), String> {
    // The room already has "Channels" with #general in it.
    let category = chat::Chat::read(doc)
        .map_err(|error| error.to_string())?
        .categories
        .iter()
        .find(|(_, c)| c.name == "Channels")
        .map(|(id, _)| id.clone());
    for name in ["help", "show-and-tell"] {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                name: name.into(),
                category: category.clone(),
                kind: None,
                topic: None,
            }),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// A picture an emoji may keep: a PNG's signature and `fill`.
fn picture(fill: u8) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(std::iter::repeat_n(fill, 64));
    png
}

impl Bridge {
    /// Ada changes the workspace's emoji (Chat's workspace record, which
    /// every app of the room opens) from her own device: `change` makes the
    /// invocation, applied to the record as the room holds it now.
    async fn ada_emoji(
        &self,
        change: impl FnOnce() -> Invoke<ui::tilo_workspace_emoji::WorkspaceEmoji>,
    ) {
        let record = collab_types::workspace_record_id(ui::tilo_workspace_emoji::KIND);
        let saved = self
            .room
            .rooms
            .record_document(&self.room.id, CHAT, &record)
            .await
            .unwrap();
        let bytes = self.emoji_changed(&record, saved, change());
        self.room
            .rooms
            .author_room_record(
                &self.room.id,
                &self.room.cert,
                (CHAT, record.as_str()),
                move |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .unwrap();
    }

    /// The emoji record `saved` with Ada's `invoke` applied, saved.
    fn emoji_changed(
        &self,
        record: &str,
        saved: Option<Vec<u8>>,
        invoke: Invoke<ui::tilo_workspace_emoji::WorkspaceEmoji>,
    ) -> Vec<u8> {
        use ui::tilo_workspace_emoji::{KIND, WorkspaceEmoji};
        let mut doc = saved
            .map(|bytes| AutoCommit::load(&bytes).unwrap())
            .unwrap_or_default();
        ui::record::init_scoped(
            &mut doc,
            &ui::record::RecordId::parse(record).unwrap(),
            &ui::record::RecordKind::new(KIND),
            CHAT,
            Some(&self.room.id),
        );
        let mut replica = Replica::<WorkspaceEmoji>::from_doc(doc)
            .as_invoker(&self.ada)
            .in_app(CHAT, "Chat");
        replica.apply_confirmed(invoke).unwrap();
        replica.doc_mut().save()
    }

    async fn ada_adds_emoji(&self, name: &str, fill: u8) {
        use ui::tilo_workspace_emoji::workspace_emoji::AddEmoji;
        use ui::tilo_workspace_emoji::{EmojiImage, WorkspaceEmoji};
        self.ada_emoji(|| {
            WorkspaceEmoji::ADD_EMOJI.with(AddEmoji {
                name: name.into(),
                image: EmojiImage(picture(fill)).to_base64(),
            })
        })
        .await;
    }

    async fn ada_removes_emoji(&self, name: &str) {
        use ui::tilo_workspace_emoji::WorkspaceEmoji;
        use ui::tilo_workspace_emoji::workspace_emoji::RemoveEmoji;
        self.ada_emoji(|| WorkspaceEmoji::REMOVE_EMOJI.with(RemoveEmoji { name: name.into() }))
            .await;
    }

    /// The id of the server's emoji `name` the bot uploaded.
    fn uploaded(&self, name: &str) -> Option<String> {
        self.fake
            .emojis()
            .iter()
            .find(|e| {
                e.get("name").and_then(Value::as_str) == Some(name)
                    && e.pointer("/user/id").and_then(Value::as_str) == Some(fake_discord::BOT_USER)
            })
            .and_then(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
    }
}

/// The workspace's emoji reach the server as emoji the bridge uploads,
/// renames and deletes, and stand for `:name:` in reactions and messages;
/// the server's own emoji are never touched; a permission the bot lacks or
/// a full server is said on the bridge's record, and reactions carry on.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_emoji_are_uploaded_renamed_and_deleted_in_the_server() {
    let (bridge, mut runner) = Bridge::start().await;
    // The server has an emoji of its own named like a workspace emoji.
    bridge.fake.set_emojis(&[("party", "55", false)]);
    settle(&mut runner, "server emoji observed", || async {
        (bridge.room.guild().await.emoji("party") == Some("55")).then_some(())
    })
    .await;

    // Added in Tilo: uploaded, with its picture, under its name.
    bridge.ada_adds_emoji("tilo_wave", 1).await;
    bridge.ada_adds_emoji("party", 2).await;
    let wave = settle(&mut runner, "uploaded", || async {
        let id = bridge.uploaded("tilo_wave")?;
        let mapped = bridge.discord_of(MappedKind::Emoji, "tilo_wave").await?;
        (mapped == id).then_some(id)
    })
    .await;
    assert_eq!(bridge.fake.emoji_picture(&wave), Some(picture(1)));
    // The server's own `party` stands for the workspace's: nothing else is
    // uploaded, and it is left as it was.
    assert_eq!(bridge.uploaded("party"), None);
    assert_eq!(bridge.discord_of(MappedKind::Emoji, "party").await, None);

    // Text and reactions from Tilo use the server's emoji.
    let first = bridge.ada_says("hi :tilo_wave: and :party:").await;
    let sent = settle(&mut runner, "post", || async {
        bridge.discord_of(MappedKind::Message, &first).await
    })
    .await;
    assert!(
        bridge
            .in_discord(&format!("hi <:tilo_wave:{wave}> and <:party:55>"))
            .is_some(),
        "{:#?}",
        bridge.fake.messages_in(CHANNEL_GENERAL)
    );
    bridge.ada_reacts(&first, ":tilo_wave:", true).await;
    settle(&mut runner, "reaction", || async {
        (bridge.fake.bot_reactions_on(&sent) == [format!("tilo_wave:{wave}")]).then_some(())
    })
    .await;

    // Renamed in Tilo (removed and added again with its picture): the same
    // Discord emoji, renamed.
    bridge.ada_removes_emoji("tilo_wave").await;
    bridge.ada_adds_emoji("hello", 1).await;
    settle(&mut runner, "renamed", || async {
        (bridge.uploaded("hello").as_deref() == Some(wave.as_str())
            && bridge
                .discord_of(MappedKind::Emoji, "hello")
                .await
                .as_deref()
                == Some(wave.as_str())
            && bridge
                .discord_of(MappedKind::Emoji, "tilo_wave")
                .await
                .is_none())
        .then_some(())
    })
    .await;

    // Removed in Tilo: deleted in Discord; the server's own stays.
    bridge.ada_removes_emoji("hello").await;
    bridge.ada_removes_emoji("party").await;
    settle(&mut runner, "deleted", || async {
        (bridge
            .fake
            .emojis()
            .iter()
            .all(|e| e["id"] != wave.as_str())
            && bridge
                .discord_of(MappedKind::Emoji, "hello")
                .await
                .is_none())
        .then_some(())
    })
    .await;
    assert!(bridge.fake.emojis().iter().any(|e| e["id"] == "55"));
    assert!(
        !calls(&bridge.fake.requests())
            .iter()
            .any(|call| call.contains("/emojis/55")),
        "the server's own emoji is never changed"
    );

    // Without Create Expressions: said on the bridge's record, nothing
    // uploaded, and the bridge carries on.
    bridge.fake.set_expressions(false, false, 50);
    bridge.ada_adds_emoji("cat", 3).await;
    settle(&mut runner, "permission problem", || async {
        (bridge.room.guild().await.emoji_problem.as_deref()
            == Some(discord_worker::perform::EMOJI_PERMISSION_PROBLEM))
        .then_some(())
    })
    .await;
    assert_eq!(bridge.uploaded("cat"), None);
    // Once granted (Discord reports the role's change), it is brought over
    // and the line is cleared.
    bridge.fake.set_expressions(true, false, 50);
    settle(&mut runner, "uploaded once allowed", || async {
        (bridge.uploaded("cat").is_some() && bridge.room.guild().await.emoji_problem.is_none())
            .then_some(())
    })
    .await;

    // A full server: said, and a reaction with the emoji stays in Tilo.
    bridge.fake.set_expressions(true, false, 2);
    bridge.ada_adds_emoji("dog", 4).await;
    settle(&mut runner, "full", || async {
        (bridge.room.guild().await.emoji_problem.as_deref()
            == Some(discord_worker::perform::EMOJI_FULL_PROBLEM))
        .then_some(())
    })
    .await;
    assert_eq!(bridge.uploaded("dog"), None);
    bridge.ada_reacts(&first, ":dog:", true).await;
    let calls = quiet(&mut runner, &bridge, "after dog").await;
    assert!(
        !calls.iter().any(|call| call.contains("/reactions/")),
        "{calls:?}"
    );
}
