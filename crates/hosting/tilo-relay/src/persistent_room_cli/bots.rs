//! The workspace's bots on this host: installing them, starting the connectors
//! their programs listen to, and handing the Discord bridge its sign-ins.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ed25519_dalek::SigningKey;
use tilo_relay::workspaces::Workspaces;
use tilo_user_identity::DeviceCert;

use super::{BotSpec, PersistentArgs, input_value};

/// Connect the workspace to the Discord server a bridge's inputs name
/// ([`discord::link_bridge`](tilo_relay::room_bots::discord::link_bridge)),
/// its posts pointing at what `connectors`' host serves publicly.
async fn link_discord(
    workspaces: &Workspaces,
    workspace: &str,
    host_cert: &DeviceCert,
    spec: &BotSpec,
    bot: &mut tilo_relay::room_bots::installed::InstalledBot,
    connectors: &tilo_relay::room_bots::installed::ConnectorHost,
) -> Result<Vec<(String, String)>, String> {
    let public = &connectors.public;
    tilo_relay::room_bots::discord::link_bridge(
        workspaces,
        workspace,
        host_cert,
        &spec.inputs,
        bot,
        public,
    )
    .await
}

/// Install the bots the operator named (once: a restart finds the same
/// installations), start the connectors their programs listen to, and run
/// each bot on this host. Each is kept a governed member of the workspace, its
/// delegation renewed with the workspace's root as it runs out.
pub(super) async fn start_bots(
    workspaces: &Workspaces,
    workspace: &str,
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
    use tilo_relay::room_bots::installed::{self, InstalledBot};
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
            match link_discord(
                workspaces,
                workspace,
                host_cert,
                spec,
                &mut bot,
                connector_host,
            )
            .await
            {
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
    let blobs = connector_host
        .blobs(workspace)
        .map_err(|error| anyhow::anyhow!("the workspace's content: {error}"))?;
    // The workspace serves without its bots rather than not at all: a connector
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
            tracing::error!(%error, "not running the workspace's bots: a connector did not start");
            return Ok((Vec::new(), None));
        }
    };
    let mut runners = Vec::new();
    for (dir, bot) in installed_bots {
        runners.extend(run_bot(
            workspaces,
            workspace,
            host_cert,
            root,
            (dir, bot),
            &started,
        )?);
    }
    Ok((runners, started.get(discord::APP).cloned()))
}

/// Run `bot` on this host, its connector `own` reaching the workspace and its
/// delegation renewed with the workspace's root as it runs out.
fn run_bot(
    workspaces: &Workspaces,
    workspace: &str,
    host_cert: &DeviceCert,
    root: &SigningKey,
    (dir, bot): (
        std::path::PathBuf,
        tilo_relay::room_bots::installed::InstalledBot,
    ),
    started: &BTreeMap<String, Arc<tilo_connector::ConnectorProcess>>,
) -> anyhow::Result<[tokio::task::JoinHandle<()>; 2]> {
    use tilo_relay::room_bots::installed::BotRunner;
    let wanted = tilo_relay::room_bots::installed::InstalledBot::connectors(
        &bot.shortcut().map_err(|error| anyhow::anyhow!("{error}"))?,
    );
    let own = started
        .iter()
        .filter(|(app, _)| wanted.contains(*app))
        .map(|(app, process)| (app.clone(), Arc::clone(process)))
        .collect::<BTreeMap<_, _>>();
    let certifier = tilo_relay::room_bots::membership::HostRootCertifier::new(root.clone());
    let runner = BotRunner::new(
        workspaces.clone(),
        workspace.to_owned(),
        host_cert.clone(),
        certifier.device_key(&bot.installation),
        bot.clone(),
        own,
        dir.join("progress.json"),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok([
        tokio::spawn(tilo_relay::room_bots::membership::keep_renewed(
            workspaces.clone(),
            workspace.to_owned(),
            host_cert.clone(),
            certifier,
            bot,
        )),
        tokio::spawn(runner.run()),
    ])
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

/// Hand a sign-in with Discord that came back to the host to the workspace's
/// Discord bridge, as an event of its connector: the bridge offers the
/// account to the person who started it. Nothing secret is in it: the
/// state and the token arrive hashed. Whether the bridge got it.
pub(super) async fn hand_to_discord(
    discord: Option<&Arc<tilo_connector::ConnectorProcess>>,
    authorized: &super::discord_sign_in::Authorized,
) -> bool {
    let Some(discord) = discord else {
        tracing::warn!(
            "a sign-in with Discord came back, but the workspace runs no Discord bridge"
        );
        return false;
    };
    let event = discord::event::Authorized {
        user_id: authorized.user_id.clone(),
        user: Some(discord::event::UserName {
            user_name: authorized.user_name.clone(),
            display_name: authorized.display_name.clone(),
        }),
        avatar: authorized.avatar.clone(),
        state_sha256: authorized.state_sha256.clone(),
        link_check: authorized.link_check.clone(),
    };
    let id = discord::event::event_id::authorized(&event.link_check);
    let Ok(serde_json::Value::Object(payload)) = serde_json::to_value(&event) else {
        return false;
    };
    match discord
        .emit(discord::event::source::AUTHORIZED, &id, payload)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, "could not hand a sign-in with Discord to the bridge");
            false
        }
    }
}

/// Tell the Discord bridge whether, and through which consent page, the
/// workspace offers signing in with Discord: members are offered "Connect
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
