//! A read-only look at a real Discord server through the worker: it
//! connects with `DISCORD_READ_ONLY=1`, so no op can change anything there,
//! reports what the gateway sends, and prints what the bridge would plan
//! for the server's current layout without performing any of it.
//!
//! `DISCORD_TOKEN_FILE` names a file with a `DISCORD_TOKEN=` line; the token
//! is handed to the worker as the host would and never printed.
//! `DISCORD_GUILD_ID` picks the server when the bot is in several;
//! `DISCORD_CHANNEL_ID` confines the worker and the plan to one channel;
//! `SMOKE_SECONDS` is how long to listen (default 30).

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "a manual tool that reports to whoever runs it"
)]

use std::collections::BTreeMap;
use std::time::Duration;

use discord::layout;
use discord::record::{Guild, MappedKind, Mapping, Ownership};
use discord::roles;
use discord::tilo::{Category, Channel, RoomChat};
use discord_wire::event::{LayoutChanged, source};
use serde_json::{Map, Value};
use tilo_connector::framing::{read_frame_async, write_frame_async};
use tilo_connector::protocol::{Bytes, FromWorker, PROTOCOL_VERSION, ToWorker};
use tokio::sync::mpsc;

fn token() -> String {
    let path =
        std::env::var("DISCORD_TOKEN_FILE").expect("DISCORD_TOKEN_FILE names the token file");
    let text = std::fs::read_to_string(path).expect("the token file reads");
    text.lines()
        .find_map(|line| line.trim().strip_prefix("DISCORD_TOKEN="))
        .map(|token| token.trim().trim_matches('"').to_owned())
        .expect("the token file has a DISCORD_TOKEN= line")
}

#[tokio::main]
async fn main() {
    tilo_net::install_crypto_provider();
    let token = token();
    let guild_id = std::env::var("DISCORD_GUILD_ID").ok();
    let only_channel = std::env::var("DISCORD_CHANNEL_ID").ok();
    let seconds = std::env::var("SMOKE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let config = discord_worker::Config::from_vars(|name| match name {
        "DISCORD_READ_ONLY" => Some("1".to_owned()),
        "DISCORD_GUILD_ID" => guild_id.clone(),
        "DISCORD_CHANNEL_ID" => only_channel.clone(),
        _ => None,
    })
    .expect("config");
    assert!(config.read_only, "the smoke test only runs read-only");

    let (host_end, worker_end) = tokio::io::duplex(1 << 22);
    let (worker_read, worker_write) = tokio::io::split(worker_end);
    tokio::spawn(discord_worker::run(
        config,
        tokio::io::BufReader::new(worker_read),
        worker_write,
    ));
    let (host_read, mut host_write) = tokio::io::split(host_end);
    let (to_worker, mut outgoing) = mpsc::unbounded_channel::<ToWorker>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if write_frame_async(&mut host_write, &message).await.is_err() {
                return;
            }
        }
    });
    to_worker
        .send(ToWorker::Start {
            version: PROTOCOL_VERSION,
            checkpoint: None,
        })
        .unwrap();

    let mut input = tokio::io::BufReader::new(host_read);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut layout: Option<Map<String, Value>> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let frame =
            tokio::time::timeout_at(deadline, read_frame_async::<_, FromWorker>(&mut input));
        let Ok(Ok(Some(message))) = frame.await else {
            break;
        };
        match message {
            FromWorker::NeedSecret { purpose, item } => {
                println!("worker asks for secret {purpose}/{item}");
                let bytes = (purpose == "bot-token").then(|| Bytes(token.clone().into_bytes()));
                to_worker
                    .send(ToWorker::Secret {
                        purpose,
                        item,
                        bytes,
                    })
                    .unwrap();
            }
            FromWorker::Status { healthy, message } => {
                println!("status: healthy={healthy} {message}");
            }
            FromWorker::Event {
                source, payload, ..
            } => {
                let channel = payload
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("event {source} {channel}");
                *counts.entry(source.clone()).or_default() += 1;
                if source == source::LAYOUT_CHANGED {
                    layout = Some(payload);
                }
            }
            FromWorker::Result { request, .. } => println!("result for {request}"),
            FromWorker::StoreSecret { purpose, .. } => println!("worker stores a {purpose}"),
            FromWorker::Checkpoint { .. }
            | FromWorker::Metrics { .. }
            | FromWorker::Ready { .. }
            | FromWorker::Blob { .. } => {}
        }
    }
    let _ = to_worker.send(ToWorker::Stop);
    println!("\nevents in {seconds}s: {counts:?}");
    let Some(layout) = layout else {
        println!("no layout event: the import has nothing to read");
        return;
    };
    let layout: LayoutChanged = serde_json::from_value(Value::Object(layout)).unwrap();
    plan(&layout, only_channel);
}

/// The adoption into an empty room's Chat, round by round, then the
/// writes the bridge would queue for Discord once the server is adopted.
fn plan(layout: &LayoutChanged, only_channel: Option<String>) {
    let mut guild = Guild {
        guild_id: layout.guild_id.clone(),
        only_channel,
        ownership: Ownership::Importing,
        ..Guild::default()
    };
    guild.observe(
        layout.bot_user.clone(),
        layout.channels.clone(),
        layout.roles.clone(),
    );
    println!(
        "\nlayout: {} channels, {} roles",
        layout.channels.len(),
        layout.roles.len()
    );
    for channel in &layout.channels {
        println!(
            "  {:?} {:?} {} parent={:?} position={}",
            channel.kind, channel.name, channel.id, channel.parent, channel.position
        );
    }
    let mut workspace = RoomChat::default();
    for round in 1.. {
        let import = discord::import::plan(&guild, &workspace);
        if !import.finish.is_empty() {
            println!("import round {round}: finished");
            break;
        }
        println!(
            "import round {round}: {} categories, {} channels, {} adopted",
            import.categories.len(),
            import.channels.len(),
            import.adopt.len()
        );
        for category in import.categories {
            println!("  create category {:?}", category.name);
            let id = format!("t{}", category.discord);
            workspace.categories.push(Category {
                id: id.clone(),
                name: category.name,
            });
            map(&mut guild, MappedKind::Category, &id, &category.discord);
        }
        for channel in import.channels {
            println!("  create {} channel {:?}", channel.kind, channel.name);
            let id = format!("t{}", channel.discord);
            workspace.channels.push(Channel {
                id: id.clone(),
                name: channel.name,
                voice: channel.kind == "voice",
                category: channel.category,
                ..Channel::default()
            });
            map(&mut guild, MappedKind::Channel, &id, &channel.discord);
        }
        assert!(round < 10, "the import does not converge");
    }
    guild.ownership = Ownership::Adopted;
    let layout_plan = layout::plan(&guild, &workspace);
    let roles_plan = roles::plan(&guild);
    println!("\nonce the server is adopted, the first reconcile would queue:");
    for apply in &layout_plan.apply {
        println!("  apply-channel {:?}", apply.op);
    }
    for delete in &layout_plan.delete {
        println!("  delete-channel {delete:?}");
    }
    println!(
        "  create-webhook in {} channels",
        layout_plan.webhooks.len()
    );
    for role in &roles_plan.apply {
        println!(
            "  apply-role {:?} id={:?} permissions={}",
            role.op.name, role.op.id, role.op.permissions
        );
    }
    println!(
        "  set-overwrite: {} (more follow once the roles exist)",
        roles_plan.overwrites.len()
    );
}

fn map(guild: &mut Guild, kind: MappedKind, tilo: &str, discord: &str) {
    guild.map.push(Mapping {
        key: discord::record::map_key(kind, tilo),
        kind,
        tilo: tilo.into(),
        discord: discord.into(),
        origin: discord::record::Origin::Discord,
        ..Mapping::default()
    });
}
