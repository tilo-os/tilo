//! The room's bots on this host: installing them, starting the connectors
//! their programs listen to, and handing the Discord bridge its sign-ins.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ed25519_dalek::SigningKey;
use tilo_relay::rooms::Rooms;
use tilo_user_identity::DeviceCert;

use super::{BotSpec, PersistentArgs, PersistentRoom, input_value};

/// Connect the room to the Discord server a bridge's inputs name
/// ([`discord::link_bridge`](tilo_relay::room_bots::discord::link_bridge)),
/// its posts pointing at what `connectors`' host serves publicly.
async fn link_discord(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    spec: &BotSpec,
    bot: &mut tilo_relay::room_bots::installed::InstalledBot,
    connectors: &tilo_relay::room_bots::installed::ConnectorHost,
) -> Result<Vec<(String, String)>, String> {
    let public = &connectors.public;
    tilo_relay::room_bots::discord::link_bridge(rooms, room, host_cert, &spec.inputs, bot, public)
        .await
}

/// Install the bots the operator named (once: a restart finds the same
/// installations), start the connectors their programs listen to, and run
/// each bot on this host. Each is kept a governed member of the room, its
/// delegation renewed with the room's root as it runs out.
pub(super) async fn start_bots(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    root: &SigningKey,
    bots: &[BotSpec],
    connector_host: &tilo_relay::room_bots::installed::ConnectorHost,
    persistent: &PersistentArgs,
    state: &Path,
) -> anyhow::Result<(
    Vec<tokio::task::JoinHandle<()>>,
    Option<Arc<tilo_connector::ConnectorProcess>>,
)> {
    use tilo_relay::room_bots::discord;
    use tilo_relay::room_bots::installed::{self, BotRunner, InstalledBot};
    if let Some(path) = &persistent.discord_token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read the Discord token file {}", path.display()))?;
        // Never logged: only kept where the connector's worker asks for it.
        installed::put_connector_secret(
            discord::APP,
            discord::TOKEN_PURPOSE,
            token.trim().as_bytes(),
        )
        .map_err(|error| anyhow::anyhow!("keep the Discord token: {error}"))?;
    }
    let mut installed_bots = Vec::new();
    let mut connectors = BTreeSet::new();
    let mut settings = BTreeMap::new();
    for spec in bots {
        let inputs = spec
            .inputs
            .iter()
            .map(|(key, value)| (key.clone(), input_value(value)))
            .collect();
        let dir = state.join("bots").join(slug_of(&spec.entry));
        let mut bot = installed::install(&dir, &spec.entry, inputs)
            .await
            .map_err(|error| anyhow::anyhow!("install {}: {error}", spec.entry))?;
        let shortcut = bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?;
        let wanted = InstalledBot::connectors(&shortcut);
        if wanted.contains(discord::APP) {
            match link_discord(rooms, room, host_cert, spec, &mut bot, connector_host).await {
                Ok(env) => {
                    settings.insert(discord::APP.to_owned(), env);
                }
                Err(error) => {
                    tracing::error!(entry = %spec.entry, %error, "not running the Discord bridge");
                    continue;
                }
            }
        }
        connectors.extend(wanted);
        tracing::info!(entry = %spec.entry, installation = %bot.installation, "bot installed");
        installed_bots.push((dir, bot));
    }
    if installed_bots.is_empty() {
        return Ok((Vec::new(), None));
    }
    let blobs = connector_host.blobs(rooms);
    // The room serves without its bots rather than not at all: a connector
    // whose worker does not start (a missing binary, an unreachable service)
    // leaves the bots that need it out, said in the log, as a bridge that
    // does not link does.
    let started = match installed::start_connectors(
        &connectors,
        state,
        &connector_host.workers,
        &settings,
        &blobs,
    )
    .await
    {
        Ok(started) => started,
        Err(error) => {
            tracing::error!(%error, "not running the room's bots: a connector did not start");
            return Ok((Vec::new(), None));
        }
    };
    let mut runners = Vec::new();
    for (dir, bot) in installed_bots {
        let wanted =
            InstalledBot::connectors(&bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?);
        let own = started
            .iter()
            .filter(|(app, _)| wanted.contains(*app))
            .map(|(app, process)| (app.clone(), Arc::clone(process)))
            .collect::<BTreeMap<_, _>>();
        let certifier = tilo_relay::room_bots::membership::HostRootCertifier::new(root.clone());
        if let Some(lane) = own.get(discord::APP).cloned() {
            runners.extend(carry_calls(rooms, room, host_cert, &certifier, &bot, lane));
        }
        let runner = BotRunner::new(
            rooms.clone(),
            room.to_owned(),
            host_cert.clone(),
            certifier.device_key(&bot.installation),
            bot.clone(),
            own,
            dir.join("progress.json"),
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        runners.push(tokio::spawn(
            tilo_relay::room_bots::membership::keep_renewed(
                rooms.clone(),
                room.to_owned(),
                host_cert.clone(),
                certifier,
                bot.clone(),
            ),
        ));
        runners.push(tokio::spawn(runner.run()));
    }
    Ok((runners, started.get(discord::APP).cloned()))
}

/// Carry `room`'s calls to Discord and back as `bot`'s device, through the
/// Discord connector's worker `lane` the bot's runner uses too.
fn carry_calls(
    rooms: &Rooms,
    room: &str,
    host_cert: &DeviceCert,
    certifier: &tilo_relay::room_bots::membership::HostRootCertifier,
    bot: &tilo_relay::room_bots::installed::InstalledBot,
    lane: Arc<impl tilo_relay::room_bots::discord_voice::CallLane>,
) -> Option<tokio::task::JoinHandle<()>> {
    match certifier.device(room, bot, tilo_clock::unix_now_ns()) {
        Ok(device) => Some(tokio::spawn(
            tilo_relay::room_bots::discord_voice::VoiceBridge::new(
                rooms.clone(),
                room.to_owned(),
                host_cert.clone(),
                device.cert,
                lane,
            )
            .run(),
        )),
        Err(error) => {
            tracing::error!(entry = %bot.entry, %error, "not carrying the Discord bridge's calls");
            None
        }
    }
}

/// A file name for directory entry or program path `entry`.
fn slug_of(entry: &str) -> String {
    entry
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hand a sign-in with Discord that came back to the service to the room's
/// Discord bridge, as an event of its connector: the bridge offers the
/// account to the person who started it. Nothing secret is in it: the
/// state and the token arrive hashed.
pub(super) async fn hand_to_discord(
    context: &PersistentRoom,
    user_id: String,
    user_name: String,
    display_name: Option<String>,
    avatar: Option<String>,
    state_sha256: String,
    link_check: String,
) {
    let Some(discord) = &context.discord else {
        tracing::warn!("a sign-in with Discord came back, but the room runs no Discord bridge");
        return;
    };
    let event = discord::event::Authorized {
        user_id,
        user: Some(discord::event::UserName {
            user_name,
            display_name,
        }),
        avatar,
        state_sha256,
        link_check,
    };
    let id = discord::event::event_id::authorized(&event.link_check);
    let payload = match serde_json::to_value(&event) {
        Ok(serde_json::Value::Object(payload)) => payload,
        _ => return,
    };
    if let Err(error) = discord
        .emit(discord::event::source::AUTHORIZED, &id, payload)
        .await
    {
        tracing::error!(%error, "could not hand a sign-in with Discord to the bridge");
    }
}

/// Tell the Discord bridge whether, and through which consent page, the
/// room offers signing in with Discord: members are offered "Connect
/// Discord" while it does. Said at each start, so settings taken away are
/// withdrawn too.
pub(super) async fn offer_sign_in(
    bridge: &tilo_connector::ConnectorProcess,
    persistent: &PersistentArgs,
) {
    let page = match (
        &persistent.discord_client_id,
        &persistent.discord_redirect_url,
        &persistent.discord_authorize_url,
    ) {
        (Some(client), Some(redirect), Some(authorize)) => {
            discord::event::sign_in_page(authorize, client, redirect)
        }
        _ => String::new(),
    };
    let offered = discord::event::SignInOffered { page };
    let id = discord::event::event_id::sign_in(&offered.page);
    let Ok(serde_json::Value::Object(payload)) = serde_json::to_value(&offered) else {
        return;
    };
    if let Err(error) = bridge
        .emit(discord::event::source::SIGN_IN, &id, payload)
        .await
    {
        tracing::error!(%error, "could not tell the bridge about signing in with Discord");
    }
}
