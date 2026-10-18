//! The bridge live against a real Discord server: a room on an in-process
//! relay with Chat and the guild record, the bridge bot installed from its
//! program file, and this binary started again as the `discord-worker`
//! under the connector host. Ada's side is scripted through her own Chat
//! changes; the Discord side through a webhook of its own, so what it posts
//! reads as someone in Discord. Every case is checked by reading Discord's
//! REST API back.
//!
//! This changes the server: run it only against a server meant for it.
//!
//! ```text
//! live_bridge <token-file> <guild-id>
//! ```
//!
//! The room is linked to the server as a hosted room links it
//! (`tilo_relay::room_bots::discord`), and the worker runs with the settings
//! that link gives it.
//!
//! The token file has a `DISCORD_TOKEN=` line; the token goes to the worker
//! through the host's secret store and is never printed.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::indexing_slicing,
    reason = "a manual tool that reports to whoever runs it"
)]

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::discord::record::{Guild, MappedKind, Ownership};
use automerge::AutoCommit;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_agent_participant::AppCatalogue;
use tilo_connector::{BlobAccess, ConnectorProcess, WorkerCommand};
use tilo_net::Method;
use tilo_relay::room_bots::discord::{self, DiscordLink};
use tilo_relay::room_bots::installed::{self, BotRunner};
use tilo_relay::room_bots::membership::{HostRootCertifier, keep_member};
use tilo_relay::room_bots::room_record;
use tilo_relay::rooms::Rooms;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use ui::tilo_actions::ActionContext;
use ui::tilo_resource::{Id, Invoke, Resource as _};

const APP: &str = "so.tilo.discord";
const CHAT: &str = "so.tilo.chat";
const ADA: [u8; 32] = [8; 32];
/// Real Discord answers slower than the fake, and rate limits.
const WAIT: Duration = Duration::from_secs(120);
/// Started with this as its first argument, the binary is the worker.
const AS_WORKER: &str = "--discord-worker";
const API: &str = "https://discord.com/api/v10";

fn main() -> ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
        return ExitCode::from(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    tilo_net::install_crypto_provider();
    if args.first().map(String::as_str) == Some(AS_WORKER) {
        return runtime.block_on(worker());
    }
    let [token_file, guild_id] = args.as_slice() else {
        eprintln!("usage: live_bridge <token-file> <guild-id>");
        return ExitCode::FAILURE;
    };
    let token = std::fs::read_to_string(token_file)
        .expect("the token file reads")
        .lines()
        .find_map(|line| line.trim().strip_prefix("DISCORD_TOKEN="))
        .map(|token| token.trim().trim_matches('"').to_owned())
        .expect("the token file has a DISCORD_TOKEN= line");
    let failed = runtime.block_on(live(token, guild_id.clone()));
    if failed == 0 {
        println!("all cases passed");
        ExitCode::SUCCESS
    } else {
        println!("{failed} case(s) failed");
        ExitCode::FAILURE
    }
}

async fn worker() -> ExitCode {
    let config = match discord_worker::Config::from_vars(|name| std::env::var(name).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("discord-worker: {error}");
            return ExitCode::FAILURE;
        }
    };
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    match discord_worker::run(config, input, tokio::io::stdout()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("discord-worker: {error}");
            ExitCode::FAILURE
        }
    }
}

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

/// Discord's REST API as a reader checking what the bridge did, and as the
/// someone in Discord its own webhook posts as.
struct Rest {
    http: tilo_net::Client,
    token: String,
}

impl Rest {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .http
            .request(method, format!("{API}{path}"))
            .header("authorization", format!("Bot {}", self.token));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("Discord answers");
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap_or_default();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, path: &str) -> Value {
        let (status, value) = self.call(Method::GET, path, None).await;
        if status == 200 { value } else { Value::Null }
    }

    /// A file posted through a webhook, as Discord takes uploads.
    async fn upload(&self, path: &str, payload: &Value, name: &str, bytes: &[u8]) -> Value {
        let boundary = "tilo-live-bridge-boundary";
        let mut body = format!(
            "--{boundary}\r\ncontent-disposition: form-data; name=\"payload_json\"\r\n\
             content-type: application/json\r\n\r\n{payload}\r\n--{boundary}\r\n\
             content-disposition: form-data; name=\"files[0]\"; filename=\"{name}\"\r\n\
             content-type: text/plain\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = self
            .http
            .request(Method::POST, format!("{API}{path}"))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .expect("Discord answers");
        let bytes = response.bytes().await.unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }
}

/// The room, as the governor's copy holds its records.
struct Room {
    rooms: Rooms,
    id: String,
    cert: DeviceCert,
    catalogue: AppCatalogue,
    owner: String,
    ada: String,
}

impl Room {
    async fn doc(&self, app: &str) -> AutoCommit {
        AutoCommit::load(
            &self
                .rooms
                .record_document(&self.id, app, &room_record(&self.catalogue, app))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }

    async fn chat(&self) -> chat::Chat {
        chat::Chat::read(&self.doc(CHAT).await).unwrap()
    }

    async fn guild(&self) -> Guild {
        Guild::read(&self.doc(APP).await).unwrap()
    }

    async fn chat_as<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut AutoCommit) -> Result<T, String> + Send + 'static,
    ) -> T {
        self.rooms
            .author_application(
                &self.id,
                &self.cert,
                &self.catalogue,
                CHAT,
                &room_record(&self.catalogue, CHAT),
                change,
            )
            .await
            .unwrap()
    }

    /// The chat's first part of messages, as the room holds it.
    async fn part(&self) -> AutoCommit {
        self.rooms
            .record_document(&self.id, CHAT, chat::parts::part(0).as_str())
            .await
            .unwrap()
            .map(|bytes| AutoCommit::load(&bytes).unwrap())
            .unwrap_or_default()
    }

    /// Change the chat's first part of messages as its author's device
    /// would, reading the chat record where it is changed.
    async fn part_as<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut AutoCommit, &chat::Chat) -> Result<T, String> + Send + 'static,
    ) -> T {
        let chat = self.chat().await;
        self.rooms
            .author_application(
                &self.id,
                &self.cert,
                &self.catalogue,
                CHAT,
                chat::parts::part(0).as_str(),
                move |part| change(part, &chat),
            )
            .await
            .unwrap()
    }

    /// `who` does `invoke` to Chat's messages from their own device.
    async fn act_on_messages(&self, who: &str, invoke: Invoke<chat::parts::ChatPart>) {
        let who = who.to_owned();
        self.part_as(move |part, chat| {
            chat::demo::apply_part_doc(part, chat, &who, invoke).map_err(|error| error.to_string())
        })
        .await;
    }

    /// `who` does `invoke` in Chat from their own device.
    async fn act(&self, who: &str, invoke: Invoke<chat::Chat>) {
        let who = who.to_owned();
        self.chat_as(move |doc| {
            chat::demo::apply_as(doc, &who, invoke).map_err(|error| error.to_string())
        })
        .await;
    }

    async fn ada_says(&self, channel: &str, text: &str) -> String {
        let (ada, channel, text) = (self.ada.clone(), channel.to_owned(), text.to_owned());
        self.part_as(move |part, chat| {
            chat::demo::say_in_part(part, chat, &ada, &channel, &text)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn message(&self, text: &str) -> Option<(String, chat::record::Message)> {
        chat::messages::read_part(&self.part().await)
            .messages
            .iter()
            .find(|(_, message)| message.text == text)
            .map(|(id, message)| (id.as_str().to_owned(), message.clone()))
    }

    async fn discord_of(&self, kind: MappedKind, tilo: &str) -> Option<String> {
        self.guild()
            .await
            .mapping(kind, tilo)
            .map(|mapping| mapping.discord.clone())
    }

    /// The name Chat shows on the message with `text`, from its change's
    /// entry.
    async fn shown_as(&self, text: &str) -> Option<String> {
        let (id, _) = self.message(text).await?;
        let doc = self.part().await;
        doc.clone().get_changes(&[]).into_iter().find_map(|change| {
            let name = change
                .message()
                .and_then(ActionContext::from_change_message)
                .and_then(|context| context.invoker?.appearance)
                .map(|appearance| appearance.name)?;
            let after = doc.clone().fork_at(&[change.hash()]).ok()?;
            let before = doc.clone().fork_at(change.deps()).ok()?;
            let has = |doc: &AutoCommit| {
                chat::messages::read_part(doc)
                    .messages
                    .iter()
                    .any(|(m, _)| m.as_str() == id)
            };
            (has(&after) && !has(&before)).then_some(name)
        })
    }
}

/// Run the bridge until `done` holds or [`WAIT`] passes; runs that did not
/// complete are printed as they happen.
async fn settle<T, F, Fut>(runner: &mut BotRunner, what: &str, mut done: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match runner.step().await {
            Ok(reports) => {
                for report in reports {
                    if report.outcome != tilo_automation::RunOutcome::Completed {
                        eprintln!("  [{what}] run {}: {:?}", report.trigger, report.outcome);
                    }
                }
            }
            Err(error) => eprintln!("  [{what}] step: {error}"),
        }
        if let Some(found) = done().await {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            for dead in runner.dead_letters() {
                eprintln!("  [{what}] dead letter: {dead:?}");
            }
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The outcome of the cases so far.
#[derive(Default)]
struct Report {
    failed: usize,
}

impl Report {
    fn case(&mut self, name: &str, passed: bool, detail: impl std::fmt::Display) {
        if !passed {
            self.failed += 1;
        }
        println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    }
}

fn link(guild: &str, channel: &str, message: Option<&str>) -> String {
    match message {
        Some(message) => format!("https://discord.com/channels/{guild}/{channel}/{message}"),
        None => format!("https://discord.com/channels/{guild}/{channel}"),
    }
}

async fn live(token: String, guild_id: String) -> usize {
    let rest = Rest {
        http: tilo_net::Client::builder()
            .user_agent("DiscordBot (https://tilo.so, 1)")
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap(),
        token: token.clone(),
    };
    let mut report = Report::default();
    let server = rest.get(&format!("/guilds/{guild_id}")).await;
    let Some(server_name) = server["name"].as_str().map(str::to_owned) else {
        report.case("server", false, "the bot cannot read this server");
        return report.failed;
    };
    println!("server: {server_name} ({guild_id})");

    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
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
        id: rooms.create(&server_name).await.unwrap().room,
        rooms,
        cert: cert.clone(),
        catalogue: AppCatalogue::native(),
        owner: hex::encode(cert.user_public_key),
        ada: hex::encode(ADA),
    };

    // Connected as a hosted room connects it: from the bridge's inputs,
    // as the room's owner.
    let bridged = DiscordLink::from_inputs(
        &[("guild", guild_id.clone()), ("server", server_name.clone())]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
    .unwrap()
    .unwrap();
    discord::connect(&room.rooms, &room.id, &cert, &bridged, &server_name)
        .await
        .unwrap();
    room.chat_as({
        let ada = room.ada.clone();
        move |doc| chat::demo::introduce(doc, &ada, "Ada", "AD").map_err(|error| error.to_string())
    })
    .await;

    let secrets = ui::secrets::SecretStore::memory(APP);
    secrets
        .item("bot-token", "default")
        .put(token.as_bytes())
        .await
        .unwrap();
    let blobs = Arc::new(Blobs::default());
    let process = ConnectorProcess::start(
        APP,
        WorkerCommand {
            program: std::env::current_exe().unwrap(),
            args: vec![AS_WORKER.into()],
            env: bridged.worker_env(),
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

    // The import: Discord's text channels come into Chat, then Tilo owns
    // the layout and every text channel gets the bridge's webhook.
    let text_channels: Vec<Value> = rest
        .get(&format!("/guilds/{guild_id}/channels"))
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|channel| channel["type"] == 0)
        .collect();
    let Some(first_text) = text_channels
        .first()
        .and_then(|channel| channel["id"].as_str())
        .map(str::to_owned)
    else {
        report.case("import", false, "the server has no text channel");
        return report.failed;
    };
    let imported = settle(&mut runner, "import", || async {
        let guild = room.guild().await;
        let general = guild
            .by_discord(MappedKind::Channel, &first_text)
            .map(|mapping| mapping.tilo.clone())?;
        (guild.ownership == Ownership::Adopted
            && guild.is_bridge_webhook(
                &guild
                    .map
                    .iter()
                    .find(|m| m.kind == MappedKind::Webhook)
                    .map(|m| m.discord.clone())
                    .unwrap_or_default(),
            ))
        .then_some(general)
    })
    .await;
    let Some(general) = imported else {
        report.case("import", false, "the layout never came into Chat");
        return report.failed;
    };
    let names: Vec<String> = room
        .chat()
        .await
        .channels
        .values()
        .map(|channel| channel.name.clone())
        .collect();
    report.case("import", true, format!("Chat channels {names:?}"));

    // Webhooks: one of the bridge's in each text channel.
    let mut hooked = Vec::new();
    for channel in &text_channels {
        let id = channel["id"].as_str().unwrap_or_default();
        let found = settle(&mut runner, "webhooks", || async {
            let hooks = rest.get(&format!("/channels/{id}/webhooks")).await;
            let guild = room.guild().await;
            hooks.as_array().and_then(|hooks| {
                hooks
                    .iter()
                    .any(|hook| {
                        hook["id"]
                            .as_str()
                            .is_some_and(|w| guild.is_bridge_webhook(w))
                    })
                    .then_some(())
            })
        })
        .await;
        hooked.push((
            channel["name"].as_str().unwrap_or_default().to_owned(),
            found.is_some(),
        ));
    }
    report.case(
        "webhooks",
        hooked.iter().all(|(_, ok)| *ok),
        format!("{hooked:?}"),
    );

    // Roles: the room's roles in Discord, and their overwrites on channels.
    let wanted = ["Viewer", "Commenter", "Editor", "Moderator", "Owner"];
    let roles = settle(&mut runner, "roles", || async {
        let roles = rest.get(&format!("/guilds/{guild_id}/roles")).await;
        let names: Vec<String> = roles
            .as_array()?
            .iter()
            .filter_map(|role| role["name"].as_str().map(str::to_owned))
            .collect();
        wanted
            .iter()
            .all(|want| names.iter().any(|name| name == want))
            .then_some(names)
    })
    .await;
    report.case(
        "roles",
        roles.is_some(),
        format!("{:?}", roles.unwrap_or_default()),
    );
    let overwrites = rest.get(&format!("/channels/{first_text}")).await["permission_overwrites"]
        .as_array()
        .map_or(0, Vec::len);
    report.case(
        "overwrites",
        overwrites > 0,
        format!("{overwrites} on the first text channel"),
    );

    // Layout: a channel made in Tilo appears in Discord.
    room.act(
        &room.owner,
        chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
            name: "made-in-tilo".into(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .await;
    let made = settle(&mut runner, "channel from Tilo", || async {
        rest.get(&format!("/guilds/{guild_id}/channels"))
            .await
            .as_array()?
            .iter()
            .find(|channel| channel["name"] == "made-in-tilo")
            .and_then(|channel| channel["id"].as_str().map(str::to_owned))
    })
    .await;
    report.case(
        "layout: channel made in Tilo",
        made.is_some(),
        made.as_deref()
            .map_or_else(|| "missing".to_owned(), |id| link(&guild_id, id, None)),
    );

    // Tilo to Discord: a post under Ada's name through the webhook.
    let first = room.ada_says(&general, "hello from Tilo").await;
    let sent = settle(&mut runner, "post", || async {
        room.discord_of(MappedKind::Message, &first).await
    })
    .await
    .unwrap_or_default();
    let message = rest
        .get(&format!("/channels/{first_text}/messages/{sent}"))
        .await;
    report.case(
        "Tilo->Discord post",
        message["content"] == "hello from Tilo"
            && message["author"]["username"] == "Ada"
            && message["webhook_id"].is_string(),
        link(&guild_id, &first_text, Some(&sent)),
    );

    room.act_on_messages(
        &room.ada,
        chat::parts::ChatPart::EDIT_MESSAGE.with(chat::messages::chat_part::EditMessage {
            message: Id::from_raw(&first),
            text: "hello from Tilo, edited".into(),
            body: None,
            mentions: None,
        }),
    )
    .await;
    let edited = settle(&mut runner, "edit", || async {
        (rest
            .get(&format!("/channels/{first_text}/messages/{sent}"))
            .await["content"]
            == "hello from Tilo, edited")
            .then_some(())
    })
    .await;
    report.case("Tilo->Discord edit", edited.is_some(), "content rewritten");

    room.act_on_messages(
        &room.ada,
        chat::parts::ChatPart::REACT.with(chat::messages::chat_part::React {
            message: Id::from_raw(&first),
            emoji: "👍".into(),
        }),
    )
    .await;
    let reacted = settle(&mut runner, "reaction", || async {
        let path = format!(
            "/channels/{first_text}/messages/{sent}/reactions/{}",
            percent_encoding::utf8_percent_encode("👍", percent_encoding::NON_ALPHANUMERIC)
        );
        rest.get(&path)
            .await
            .as_array()
            .is_some_and(|users| !users.is_empty())
            .then_some(())
    })
    .await;
    report.case("Tilo->Discord reaction", reacted.is_some(), "👍 by the bot");

    room.act_on_messages(
        &room.ada,
        chat::demo::post(&general, "a reply from Tilo", Some(&first)),
    )
    .await;
    let thread = settle(&mut runner, "reply", || async {
        let thread = room.discord_of(MappedKind::Thread, &first).await?;
        rest.get(&format!("/channels/{thread}/messages"))
            .await
            .as_array()?
            .iter()
            .any(|m| m["content"] == "a reply from Tilo" && m["author"]["username"] == "Ada")
            .then_some(thread)
    })
    .await;
    report.case(
        "Tilo->Discord reply (thread)",
        thread.is_some(),
        thread
            .as_deref()
            .map_or_else(|| "missing".to_owned(), |id| link(&guild_id, id, None)),
    );

    let bytes = b"the minutes, from Tilo".to_vec();
    let blob = blobs.store(&bytes).unwrap();
    room.act_on_messages(
        &room.ada,
        chat::parts::ChatPart::POST.with(chat::messages::chat_part::Post {
            message: chat::messages::NewMessage {
                channel: Id::from_raw(&general),
                text: "notes from Tilo".into(),
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
        rest.get(&format!("/channels/{first_text}/messages?limit=20"))
            .await
            .as_array()?
            .iter()
            .find(|m| m["content"] == "notes from Tilo")
            .cloned()
    })
    .await
    .unwrap_or_default();
    report.case(
        "Tilo->Discord file",
        file["attachments"][0]["filename"] == "notes.txt",
        link(&guild_id, &first_text, file["id"].as_str()),
    );

    let doomed = room.ada_says(&general, "deleted in Tilo").await;
    let doomed_sent = settle(&mut runner, "post to delete", || async {
        room.discord_of(MappedKind::Message, &doomed).await
    })
    .await
    .unwrap_or_default();
    room.act_on_messages(
        &room.ada,
        chat::parts::ChatPart::DELETE_MESSAGE.with(chat::messages::chat_part::DeleteMessage {
            message: Id::from_raw(&doomed),
        }),
    )
    .await;
    let deleted = settle(&mut runner, "delete", || async {
        let (status, _) = rest
            .call(
                Method::GET,
                &format!("/channels/{first_text}/messages/{doomed_sent}"),
                None,
            )
            .await;
        (status == 404).then_some(())
    })
    .await;
    report.case(
        "Tilo->Discord delete",
        !doomed_sent.is_empty() && deleted.is_some(),
        "gone from Discord",
    );

    // Discord to Tilo, through a webhook that is not the bridge's: what it
    // posts reads as Bo, someone in Discord.
    let (status, hook) = rest
        .call(
            Method::POST,
            &format!("/channels/{first_text}/webhooks"),
            Some(json!({"name": "live test"})),
        )
        .await;
    let (Some(hook_id), Some(hook_token)) = (
        hook["id"].as_str().map(str::to_owned),
        hook["token"].as_str().map(str::to_owned),
    ) else {
        report.case(
            "Discord->Tilo",
            false,
            format!("no webhook to post with ({status})"),
        );
        return report.failed;
    };
    let execute = format!("/webhooks/{hook_id}/{hook_token}");
    let (_, posted) = rest
        .call(
            Method::POST,
            &format!("{execute}?wait=true"),
            Some(json!({"content": "hello from Discord", "username": "Bo"})),
        )
        .await;
    let posted = posted["id"].as_str().unwrap_or_default().to_owned();
    let landed = settle(&mut runner, "Discord post", || async {
        room.message("hello from Discord").await.map(drop)
    })
    .await;
    let shown = room.shown_as("hello from Discord").await;
    report.case(
        "Discord->Tilo post",
        landed.is_some() && shown.as_deref() == Some("Bo"),
        format!("shown as {shown:?}"),
    );

    rest.call(
        Method::PATCH,
        &format!("{execute}/messages/{posted}"),
        Some(json!({"content": "hello from Discord, edited"})),
    )
    .await;
    let edited = settle(&mut runner, "Discord edit", || async {
        room.message("hello from Discord, edited").await.map(drop)
    })
    .await;
    report.case(
        "Discord->Tilo edit",
        edited.is_some(),
        "Chat text rewritten",
    );

    if let Some(thread) = &thread {
        rest.call(
            Method::POST,
            &format!("{execute}?wait=true&thread_id={thread}"),
            Some(json!({"content": "Bo answers in the thread", "username": "Bo"})),
        )
        .await;
        let answered = settle(&mut runner, "Discord thread reply", || async {
            room.message("Bo answers in the thread").await
        })
        .await;
        report.case(
            "Discord->Tilo reply (in the thread)",
            answered
                .as_ref()
                .and_then(|(_, m)| m.parent.as_ref())
                .is_some_and(|parent| parent.as_str() == first),
            "answers Ada's message in Chat",
        );
    }

    rest.upload(
        &format!("{execute}?wait=true"),
        &json!({"content": "notes from Discord", "username": "Bo"}),
        "minutes.txt",
        b"the minutes, from Discord",
    )
    .await;
    let file = settle(&mut runner, "Discord file", || async {
        room.message("notes from Discord").await
    })
    .await;
    let attachment = file.map(|(_, m)| format!("{:?}", m.attachment));
    report.case(
        "Discord->Tilo file",
        attachment
            .as_deref()
            .is_some_and(|a| a.contains("minutes.txt")),
        attachment.unwrap_or_else(|| "missing".into()),
    );

    let (_, doomed) = rest
        .call(
            Method::POST,
            &format!("{execute}?wait=true"),
            Some(json!({"content": "deleted in Discord", "username": "Bo"})),
        )
        .await;
    let doomed = doomed["id"].as_str().unwrap_or_default().to_owned();
    settle(&mut runner, "Discord post to delete", || async {
        room.message("deleted in Discord").await.map(drop)
    })
    .await;
    rest.call(
        Method::DELETE,
        &format!("{execute}/messages/{doomed}"),
        None,
    )
    .await;
    let gone = settle(&mut runner, "Discord delete", || async {
        room.message("deleted in Discord")
            .await
            .is_none()
            .then_some(())
    })
    .await;
    report.case("Discord->Tilo delete", gone.is_some(), "gone from Chat");

    rest.call(Method::DELETE, &format!("/webhooks/{hook_id}"), None)
        .await;
    drop(relay);
    report.failed
}
