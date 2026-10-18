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
/// Long enough for the worker's gateway session and the bridge's passes.
const WAIT: Duration = Duration::from_secs(60);

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
                &ui::tilo_actions::door::ActionAddress::new(
                    ui::framework::chain_resource_id(&chat::parts::chain()),
                    invoke.id,
                ),
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
    let deadline = tokio::time::Instant::now() + WAIT;
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
    /// The bridged Chat workspace.
    workspace: String,
    /// The Chat channel Discord's #general is bridged to.
    general: String,
    ada: String,
    _relay: tilo_relay::daemon::Relay,
    _scratch: tempfile::TempDir,
}

impl Bridge {
    /// The bridge, and the runner that runs its bot.
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

        // The room's Chat, with Ada in it.
        room.rooms
            .prepare_application(&room.id, &cert, &room.catalogue, CHAT, "")
            .await
            .unwrap();
        let workspace = room
            .chat_as({
                let (owner, ada) = (owner.clone(), ada.clone());
                move |doc| {
                    chat::demo::apply_as(
                        doc,
                        &owner,
                        chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace {
                            name: "Bridged".into(),
                        }),
                    )
                    .map_err(|error| error.to_string())?;
                    chat::demo::introduce(doc, &ada, "Ada", "AD")
                        .map_err(|error| error.to_string())?;
                    let chat = chat::Chat::read(doc).map_err(|error| error.to_string())?;
                    let workspace = chat
                        .workspaces
                        .iter()
                        .find(|(_, w)| w.name == "Bridged")
                        .unwrap()
                        .0
                        .clone();
                    if setup.existing {
                        seed_channels(doc, &owner, &workspace)?;
                    }
                    Ok(workspace.as_str().to_owned())
                }
            })
            .await;

        // The guild record, connected to the fake server and that workspace.
        room.rooms
            .prepare_application(&room.id, &cert, &room.catalogue, APP, "")
            .await
            .unwrap();
        room.rooms
            .author_application(&room.id, &cert, &room.catalogue, APP, "", {
                let (owner, workspace) = (owner.clone(), workspace.clone());
                move |doc| {
                    let mut replica = Replica::<Guild>::from_doc(std::mem::take(doc))
                        .as_invoker(&owner)
                        .in_app(APP, "Discord");
                    let applied = replica
                        .apply_confirmed(Guild::CONNECT.with(guild::Connect {
                            guild_id: GUILD.into(),
                            name: "Fake".into(),
                            workspace,
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

        // The worker, holding the bot token in the host's store.
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
                program: env!("CARGO_BIN_EXE_discord-worker").into(),
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

        // The bridge, installed from its program file.
        let program = concat!(env!("CARGO_MANIFEST_DIR"), "/../bridge/program.json");
        let bot = installed::install(&scratch.path().join("bot"), program, serde_json::Map::new())
            .await
            .unwrap();
        // A member of the room, signing what it changes with its own device.
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
            workspace,
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

    /// Ada posts `text` in #general; the new message's id.
    async fn ada_says(&self, text: &str) -> String {
        self.room
            .part_as(&self.ada, chat::demo::post(&self.general, text, None))
            .await
            .pop()
            .expect("the post is in a part")
    }

    /// The Chat message with `text`, by id.
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
    bridge
        .as_ada_on_messages(
            chat::parts::ChatPart::REACT.with(chat::messages::chat_part::React {
                message: Id::from_raw(&first),
                emoji: "👍".into(),
            }),
        )
        .await;
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
            .any(|reacted| reacted.emoji == "🎉")
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
    let attachment = format!("{:?}", file.attachment.0);
    assert!(attachment.contains("notes.txt"), "{attachment}");

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

/// Run `runner` until a whole `window` passes with no REST call, at most
/// for [`WAIT`]; the REST calls made meanwhile.
async fn quiet(runner: &mut BotRunner, fake: &FakeDiscord, window: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let start = fake.requests().len();
    let mut seen = start;
    let mut since = tokio::time::Instant::now();
    loop {
        runner.step().await.unwrap();
        let now = fake.requests().len();
        if now != seen {
            (seen, since) = (now, tokio::time::Instant::now());
        } else if since.elapsed() >= window {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never went quiet: {:#?}",
            calls(fake.requests().get(start..).unwrap_or_default())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    calls(fake.requests().get(start..).unwrap_or_default())
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
    quiet(&mut runner, &bridge.fake, Duration::from_secs(2)).await;
    let changed: std::collections::BTreeSet<String> = changes(&bridge.fake).into_iter().collect();
    assert_eq!(
        changed,
        webhooks_of(&[CHANNEL_GENERAL, fake_discord::CHANNEL_GOAL])
    );
    // Idle: nothing changes, so nothing is called.
    let idle = quiet(&mut runner, &bridge.fake, Duration::from_secs(3)).await;
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
    quiet(&mut runner, &bridge.fake, Duration::from_secs(2)).await;
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

    // Chat has Discord's layout joined with its own, nothing doubled; its
    // own #help and #show-and-tell stay in Tilo.
    let chat = bridge.room.chat().await;
    let ours = bridge.workspace.as_str();
    let mut names: Vec<&str> = chat
        .channels
        .values()
        .filter(|c| c.workspace.as_str() == ours)
        .map(|c| c.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "Lounge",
            "general",
            "goal",
            "help",
            "progress",
            "roadmap",
            "show-and-tell",
            "voice"
        ]
    );
    let mut categories: Vec<&str> = chat
        .categories
        .values()
        .filter(|c| c.workspace.as_str() == ours)
        .map(|c| c.name.as_str())
        .collect();
    categories.sort_unstable();
    assert_eq!(categories, ["Channels", "Voice", "chat"]);
    // #help, #show-and-tell, and the workspace's own "Voice" with its
    // Lounge voice channel, none of which the server has.
    let guild = bridge.room.guild().await;
    assert_eq!(guild.tilo_only.len(), 4, "{:?}", guild.tilo_only);

    // And messages cross.
    bridge.ada_says("hello there").await;
    settle(&mut runner, "Tilo to #general", || async {
        bridge.in_discord("hello there").map(drop)
    })
    .await;
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
    quiet(&mut runner, &bridge.fake, Duration::from_secs(2)).await;
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
        .find(|(_, c)| c.workspace.as_str() == bridge.workspace && c.name == "Channels")
        .map(|(id, _)| id.clone())
        .unwrap();
    bridge
        .as_ada(
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                workspace: Id::from_raw(bridge.workspace.as_str()),
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
    quiet(&mut runner, &bridge.fake, Duration::from_secs(2)).await;

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
    quiet(&mut runner, &bridge.fake, Duration::from_secs(1)).await;
    assert!(bridge.chat_message("elsewhere").await.is_none());
}

/// The room's own "Channels" with #general, #help and #show-and-tell, made
/// by its owner before any bridge ran.
fn seed_channels(doc: &mut AutoCommit, owner: &str, workspace: &Id) -> Result<(), String> {
    // A new workspace already has "Channels" with #general in it.
    let category = chat::Chat::read(doc)
        .map_err(|error| error.to_string())?
        .categories
        .iter()
        .find(|(_, c)| &c.workspace == workspace && c.name == "Channels")
        .map(|(id, _)| id.clone());
    for name in ["help", "show-and-tell"] {
        chat::demo::apply_as(
            doc,
            owner,
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                workspace: workspace.clone(),
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
