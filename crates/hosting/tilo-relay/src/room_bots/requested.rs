//! The bots a room asks its host to run.
//!
//! A room member who may install bots asks for one from the registry's
//! directory (`request-bot`); the request is kept in the room's governance,
//! so every member sees which bots the room wants. The host that governs
//! the room follows those requests: it installs each requested bot as
//! itself, keeps it a governed member and runs it, and when a request is
//! taken back it stops the bot and uninstalls it. Bots the operator named
//! when starting the room are the operator's and are left alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Map, Value};
use tilo_peer_state::RequestedBot;
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_user_identity::DeviceCert;

use super::installed::{self, BotRunner, InstalledBot};
use super::membership::{HostRootCertifier, keep_renewed};
use crate::rooms::Rooms;

/// How long the host waits before it reads the room's requests again.
const FOLLOW: Duration = Duration::from_secs(30);

/// What following the room's requests changes now.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Requests no bot runs for yet.
    pub start: Vec<RequestedBot>,
    /// Requests for another release than runs: the bot stays installed and
    /// runs the new program from now on.
    pub replace: Vec<RequestedBot>,
    /// Entries whose bot runs without a request standing for it.
    pub stop: Vec<String>,
}

/// The release a request asks for: its version, and the hash of the
/// developer's program it carries, if any, so pushing a changed program at
/// the same version still replaces the running one.
#[must_use]
pub fn release(request: &tilo_peer_state::workspace_membership::BotRequest) -> String {
    match &request.program {
        Some(program) => format!(
            "{}#{}",
            request.version,
            tilo_automation::program::program_hash(program.as_bytes())
        ),
        None => request.version.clone(),
    }
}

/// Compare the room's `requests` with the releases `running` by entry.
/// Entries in `operator` are the operator's bots: never started or stopped
/// here.
#[must_use]
pub fn plan(
    requests: &[RequestedBot],
    running: &BTreeMap<String, String>,
    operator: &BTreeSet<String>,
) -> Plan {
    let mut plan = Plan::default();
    for request in requests {
        let entry = &request.request.entry;
        if operator.contains(entry) {
            continue;
        }
        match running.get(entry) {
            Some(runs) if runs == &release(&request.request) => {}
            Some(_) => plan.replace.push(request.clone()),
            None => plan.start.push(request.clone()),
        }
    }
    let asked: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.request.entry.as_str())
        .collect();
    for entry in running.keys() {
        if !asked.contains(entry.as_str()) && !operator.contains(entry) {
            plan.stop.push(entry.clone());
        }
    }
    plan
}

/// What the host keeps to follow a room's requests.
pub struct Follower {
    /// The rooms this host governs.
    pub rooms: Rooms,
    /// The room whose requests it follows.
    pub room: String,
    /// The host's device in the room, which governs the bots it installs.
    pub host: DeviceCert,
    /// The room root the host signs its bots' devices and delegations with.
    pub root: SigningKey,
    /// The host's state directory; each requested bot keeps its
    /// installation under `requested-bots/<entry>`.
    pub state: PathBuf,
    pub operator: BTreeSet<String>,
    /// The connectors this host can run for a requested bot (the Discord
    /// bridge); `None` where it runs none.
    pub connectors: Option<installed::ConnectorHost>,
}

/// A requested bot running here.
struct Running {
    release: String,
    installation: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    fn halt(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

/// The requested bots running here, by entry.
#[derive(Default)]
pub struct Followed(BTreeMap<String, Running>);

impl Followed {
    /// The installation running for `entry`, and the release it runs.
    #[must_use]
    pub fn running(&self, entry: &str) -> Option<(&str, &str)> {
        self.0
            .get(entry)
            .map(|bot| (bot.installation.as_str(), bot.release.as_str()))
    }
}

impl Drop for Followed {
    fn drop(&mut self) {
        for (_, bot) in std::mem::take(&mut self.0) {
            bot.halt();
        }
    }
}

impl Follower {
    /// Follow the room's requests for as long as the host runs: at once
    /// when the room's governance changes (a request asked for, replaced or
    /// taken back), and every so often besides.
    pub async fn run(self) {
        let mut followed = Followed::default();
        let mut changes = self.rooms.governance_changes();
        loop {
            self.follow_requests(&mut followed).await;
            let wait = tokio::time::sleep(FOLLOW);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    () = &mut wait => break,
                    change = changes.recv() => match change {
                        Ok(room) if room == self.room => break,
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            wait.as_mut().await;
                            break;
                        }
                    },
                }
            }
        }
    }

    /// Read the room's requests and start, replace and stop bots to match.
    pub async fn follow_requests(&self, followed: &mut Followed) {
        match self.rooms.bot_requests(&self.room).await {
            Ok(requests) => self.follow(&requests, &mut followed.0).await,
            Err(error) => tracing::warn!(%error, "could not read the room's bot requests"),
        }
    }

    async fn follow(&self, requests: &[RequestedBot], running: &mut BTreeMap<String, Running>) {
        let releases = running
            .iter()
            .map(|(entry, bot)| (entry.clone(), bot.release.clone()))
            .collect();
        let plan = plan(requests, &releases, &self.operator);
        for entry in plan.stop {
            if let Some(bot) = running.remove(&entry) {
                self.stop(&entry, bot).await;
            }
        }
        for request in plan.replace {
            // The same bot runs the new release: its runs stop, the room's
            // row takes the new program and grants, and it carries on from
            // where its runs had read up to.
            if let Some(bot) = running.remove(&request.request.entry) {
                bot.halt();
            }
            self.begin(&request, running, "requested bot runs a new release")
                .await;
        }
        for request in plan.start {
            self.begin(&request, running, "requested bot runs").await;
        }
    }

    async fn begin(
        &self,
        request: &RequestedBot,
        running: &mut BTreeMap<String, Running>,
        what: &str,
    ) {
        let entry = request.request.entry.clone();
        match self.start(request).await {
            Ok(bot) => {
                tracing::info!(%entry, installation = %bot.installation, release = %bot.release, "{what}");
                running.insert(entry, bot);
            }
            Err(error) => tracing::warn!(%entry, %error, "requested bot does not run"),
        }
    }

    fn dir(&self, entry: &str) -> PathBuf {
        bot_dir(&self.state, entry)
    }

    async fn start(&self, request: &RequestedBot) -> Result<Running, String> {
        let entry = &request.request.entry;
        let dir = self.dir(entry);
        let mut inputs = Map::new();
        for (name, value) in &request.request.inputs {
            inputs.insert(name.clone(), Value::String(value.clone()));
        }
        inputs.insert(
            "version".into(),
            Value::String(request.request.version.clone()),
        );
        let mut bot: InstalledBot =
            installed::install_release(&dir, entry, inputs, request.request.program.as_deref())
                .await?;
        let shortcut = bot.shortcut()?;
        let connectors = self
            .connectors_for(&shortcut, &request.request.inputs, &mut bot)
            .await?;
        let certifier = HostRootCertifier::new(self.root.clone());
        // The room's row names this program and its grants before the bot
        // runs it, so its first changes are judged by them.
        super::membership::keep_member(&self.rooms, &self.room, &self.host, &certifier, &bot)
            .await?;
        let running = tokio::spawn(
            BotRunner::new(
                self.rooms.clone(),
                self.room.clone(),
                self.host.clone(),
                certifier.device_key(&bot.installation),
                bot.clone(),
                connectors,
                dir.join("progress.json"),
            )?
            .run(),
        );
        let installation = bot.installation.clone();
        let tasks = vec![
            tokio::spawn(keep_renewed(
                self.rooms.clone(),
                self.room.clone(),
                self.host.clone(),
                certifier,
                bot,
            )),
            running,
        ];
        Ok(Running {
            release: release(&request.request),
            installation,
            tasks,
        })
    }

    /// Start the connectors a requested bot listens to. A member who may
    /// install bots sets one up as the room's operator would: its secret
    /// through the room's `set-connector-secret` (kept in this host's store),
    /// its settings as the request's inputs (a bridge's `guild=` and
    /// `server=`). Only the Discord bridge is set up this way.
    ///
    /// # Errors
    /// The host runs no connectors, the connector is not one a member can
    /// set up, its secret has not been given, or the link was refused.
    async fn connectors_for(
        &self,
        shortcut: &tilo_automation::Shortcut,
        inputs: &BTreeMap<String, String>,
        bot: &mut InstalledBot,
    ) -> Result<BTreeMap<String, std::sync::Arc<tilo_connector::ConnectorProcess>>, String> {
        use super::discord;
        let wanted = InstalledBot::connectors(shortcut);
        if wanted.is_empty() {
            return Ok(BTreeMap::new());
        }
        let host = self
            .connectors
            .as_ref()
            .ok_or("this host runs no connectors for a requested bot")?;
        let mut settings = BTreeMap::new();
        for app in &wanted {
            if app != discord::APP {
                return Err(format!("{app} is not a connector a room member can set up"));
            }
            if !installed::has_connector_secret(app, discord::TOKEN_PURPOSE) {
                return Err("the Discord bridge waits for its bot token".into());
            }
            let env = discord::link_bridge(
                &self.rooms,
                &self.room,
                &self.host,
                inputs,
                bot,
                &host.public,
            )
            .await?;
            settings.insert(app.clone(), env);
        }
        let blobs = host.blobs(&self.rooms);
        installed::start_connectors(&wanted, &self.state, &host.workers, &settings, &blobs).await
    }

    async fn stop(&self, entry: &str, bot: Running) {
        let installation = bot.installation.clone();
        bot.halt();
        let installation = match tilo_peer_state::InstallationId::parse(&installation) {
            Ok(installation) => installation,
            Err(error) => {
                tracing::warn!(%entry, %error, "requested bot's installation id is unreadable");
                return;
            }
        };
        match self
            .rooms
            .govern_bot(
                &self.room,
                &self.host,
                WorkspaceMembershipCommand::UninstallBot { installation },
            )
            .await
        {
            Ok(_) => tracing::info!(%entry, "requested bot uninstalled"),
            Err(error) => tracing::warn!(%entry, %error, "requested bot not uninstalled"),
        }
        // A new request for the entry installs it anew.
        let _ = std::fs::remove_file(self.dir(entry).join("bot.json"));
    }
}

/// Where requested bot `entry` keeps its installation under `state`.
#[must_use]
pub fn bot_dir(state: &Path, entry: &str) -> PathBuf {
    let slug: String = entry
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state.join("requested-bots").join(slug)
}

/// Keep `follower` running beside the room, for as long as the host runs.
#[must_use]
pub fn spawn(follower: Follower) -> tokio::task::JoinHandle<()> {
    tokio::spawn(follower.run())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_peer_state::workspace_membership::BotRequest;

    fn asked(entry: &str, version: &str) -> RequestedBot {
        RequestedBot {
            request: BotRequest {
                entry: entry.into(),
                version: version.into(),
                inputs: BTreeMap::new(),
                program: None,
            },
            requested_by: "ada".into(),
            requested_at_ns: 1,
        }
    }

    #[test]
    fn a_new_request_starts_its_bot_and_a_withdrawn_one_stops_it() {
        let running = BTreeMap::from([("so.tilo.bots.digest".to_owned(), "1.0.0".to_owned())]);
        let plan = plan(
            &[asked("so.tilo.bots.standup", "1.2.0")],
            &running,
            &BTreeSet::new(),
        );
        assert_eq!(plan.start, vec![asked("so.tilo.bots.standup", "1.2.0")]);
        assert_eq!(plan.stop, vec!["so.tilo.bots.digest".to_owned()]);
    }

    #[test]
    fn a_request_for_another_version_replaces_the_running_bot() {
        let running = BTreeMap::from([("so.tilo.bots.standup".to_owned(), "1.2.0".to_owned())]);
        let same = plan(
            &[asked("so.tilo.bots.standup", "1.2.0")],
            &running,
            &BTreeSet::new(),
        );
        assert_eq!(
            same,
            Plan::default(),
            "a bot that runs as asked is left alone"
        );
        let newer = plan(
            &[asked("so.tilo.bots.standup", "1.3.0")],
            &running,
            &BTreeSet::new(),
        );
        assert!(newer.stop.is_empty(), "the same bot carries on");
        assert!(newer.start.is_empty());
        assert_eq!(newer.replace, vec![asked("so.tilo.bots.standup", "1.3.0")]);
        // A developer's program pushed again at the same version is
        // another release all the same.
        let mut pushed = asked("so.tilo.bots.standup", "1.2.0");
        pushed.request.program = Some("{}".into());
        assert_eq!(
            plan(&[pushed.clone()], &running, &BTreeSet::new()).replace,
            vec![pushed]
        );
    }

    #[test]
    fn the_operators_bots_are_never_started_or_stopped_by_requests() {
        let operator = BTreeSet::from(["so.tilo.bots.bridge".to_owned()]);
        let running = BTreeMap::from([("so.tilo.bots.bridge".to_owned(), "2.0.0".to_owned())]);
        assert_eq!(plan(&[], &running, &operator), Plan::default());
        assert_eq!(
            plan(
                &[asked("so.tilo.bots.bridge", "3.0.0")],
                &BTreeMap::new(),
                &operator
            ),
            Plan::default()
        );
    }

    #[test]
    fn every_requested_bot_keeps_its_installation_in_a_directory_of_its_own() {
        let state = Path::new("/var/lib/room");
        assert_eq!(
            bot_dir(state, "so.tilo.bots.standup"),
            Path::new("/var/lib/room/requested-bots/so.tilo.bots.standup")
        );
        assert_ne!(
            bot_dir(state, "so.tilo.bots.standup"),
            bot_dir(state, "so.tilo.bots.digest")
        );
    }
}
