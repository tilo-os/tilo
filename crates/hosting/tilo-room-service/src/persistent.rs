//! The persistent public room: one `tilo-relay room --persistent` child on
//! the persistent volume, restarted when it exits. Guests are admitted by
//! `POST /join` and watching pages by `POST /spectate`, each under the
//! room's limits.

use crate::config::{Config, PersistentConfig};
use crate::guests::GuestLedger;
use crate::limits::{IpHasher, JoinLimits, Seats};
use crate::room::read_started;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tilo_relay_protocol::public_avatars::is_avatar_name;
use tilo_relay_protocol::room_operator::{LineError, OperatorCommand, RoomEvent, hex32};
use tilo_relay_protocol::rooms::RoomRole;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::Command;
use tokio::sync::{Mutex, oneshot};

/// How long `/join` and `/spectate` wait for the child's answer.
const ANSWER_WAIT: Duration = Duration::from_secs(10);
/// What `/join` and `/spectate` tell a visitor to wait when the room is full.
pub const FULL_RETRY_SECS: u64 = 60;
/// The secret store of this service: the key its address hashes use.
const SECRET_APP: &str = "so.tilo.room-service";
/// A child that ran this long restarts after the shortest wait again.
const SETTLED: Duration = Duration::from_secs(600);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinRequest {
    pub user_public_key: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinAnswer {
    pub invitation: String,
    pub admission_token: String,
    pub origin: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpectateRequest {
    pub device_public_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectateAnswer {
    /// The device certificate in the form the browser runtime takes.
    pub certificate: serde_json::Value,
    pub origin: String,
    pub room: String,
}

#[derive(Clone, Copy, Debug)]
pub enum JoinRefusal {
    Invalid(&'static str),
    CoolingDown,
    TooMany { retry_after_secs: u64 },
    Full { retry_after_secs: u64 },
    Unavailable,
}

/// What the child answered one waiting request.
enum Answer {
    Invitation(String),
    Certificate(serde_json::Value),
}

type Waiters = HashMap<String, oneshot::Sender<std::result::Result<Answer, String>>>;
type OperatorInput = Box<dyn AsyncWrite + Send + Unpin>;

/// The running child: where to write commands, and what it announced.
struct Child {
    stdin: OperatorInput,
    origin: String,
    room: String,
}

/// What outlives one child.
pub(crate) struct Shared {
    dir: PathBuf,
    hasher: IpHasher,
    joins: std::sync::Mutex<JoinLimits>,
    spectates: std::sync::Mutex<JoinLimits>,
    guests: std::sync::Mutex<GuestLedger>,
    /// Guest seats, under the guest cap.
    seats: std::sync::Mutex<Seats>,
    /// Spectator seats, under the spectator cap.
    watching: std::sync::Mutex<Seats>,
}

impl Shared {
    fn new(config: &Config, name: &str, dir: PathBuf) -> Result<Self> {
        let item = tilo_secrets::SecretStore::for_app(SECRET_APP).item("ip-hash-key", name);
        let key = if let Some(stored) = item.get_blocking().context("read the address hash key")? {
            <[u8; 32]>::try_from(stored.expose())
                .map_err(|_| anyhow::anyhow!("the stored address hash key is corrupt"))?
        } else {
            let key: [u8; 32] = rand::random();
            item.put_blocking(&key)
                .context("keep the address hash key")?;
            key
        };
        let persistent = config
            .persistent
            .as_ref()
            .context("not a persistent room")?;
        Self::with_key(dir, key, config.max_guests, persistent.max_spectators)
    }

    fn with_key(
        dir: PathBuf,
        key: [u8; 32],
        max_guests: usize,
        max_spectators: usize,
    ) -> Result<Self> {
        std::fs::create_dir_all(dir.join("avatars"))
            .with_context(|| format!("create {}", dir.display()))?;
        let guests = GuestLedger::open(dir.join("guests.json"))?;
        Ok(Self {
            dir,
            hasher: IpHasher::new(key),
            joins: std::sync::Mutex::new(JoinLimits::guests()),
            spectates: std::sync::Mutex::new(JoinLimits::spectators()),
            guests: std::sync::Mutex::new(guests),
            seats: std::sync::Mutex::new(Seats::new(max_guests)),
            watching: std::sync::Mutex::new(Seats::new(max_spectators)),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(dir: &Path, max_guests: usize, max_spectators: usize) -> Arc<Self> {
        Arc::new(
            Self::with_key(dir.to_path_buf(), [7; 32], max_guests, max_spectators)
                .unwrap_or_else(|error| panic!("{error:#}")),
        )
    }

    pub(crate) fn avatar_path(&self, name: &str) -> Option<PathBuf> {
        is_avatar_name(name).then(|| self.dir.join("avatars").join(name))
    }
}

pub struct Persistent {
    shared: Arc<Shared>,
    /// The current child; `None` while it restarts.
    child: Mutex<Option<Child>>,
    waiters: std::sync::Mutex<Waiters>,
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ms()
}

fn token_digest(token: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(token.as_bytes()))
}

/// The name a visitor joins as: none (they are "Guest"), or a display name
/// as every host keeps one; anything else is refused with the reason.
fn printable_name(name: &str) -> Result<String, &'static str> {
    if name.trim().is_empty() {
        return Ok(String::new());
    }
    tilo_user_identity::display_name(name)
        .map(str::to_owned)
        .map_err(tilo_user_identity::DisplayNameError::reason)
}

/// The relay's TLS listener: the direct WebTransport origin when one is
/// configured, else a loopback (or, on direct ports, public) one.
fn tls_listener(
    command: &mut Command,
    config: &Config,
    persistent: &crate::config::PersistentConfig,
) {
    if let Some(live) = &persistent.live {
        // The TLS listener is the direct WebTransport origin, with its own
        // public certificate; pages still load through the ingress.
        let (chain, key) = &live.certificate;
        command
            .arg("--web")
            .arg(live.listen.to_string())
            .arg("--tls")
            .arg(format!("files:{},{}", chain.display(), key.display()))
            .arg("--public-host")
            .arg(&live.host)
            .arg("--live-origin")
            .arg(&live.origin);
    } else {
        command
            .arg("--web")
            .arg(
                if config.direct_ports && persistent.relay_origin.is_none() {
                    format!("0.0.0.0:{}", config.port_base)
                } else {
                    format!("127.0.0.1:{}", config.port_base)
                },
            )
            .arg("--tls")
            .arg(match &config.certificate {
                Some((chain, key)) if persistent.relay_origin.is_none() => {
                    format!("files:{},{}", chain.display(), key.display())
                }
                _ => "dev".to_owned(),
            });
    }
}

/// The child's command line.
fn child_command(config: &Config, dir: &Path) -> Result<Command> {
    let persistent = config
        .persistent
        .as_ref()
        .context("not a persistent room")?;
    let mut command = Command::new(&config.relay_binary);
    command
        .env("TILO_RELAY_LOG", "warn")
        .arg("room")
        .arg("--state")
        .arg(dir.join("relay"));
    tls_listener(&mut command, config, persistent);
    command.arg("--allowed-origin").arg(config.api_origin());
    for origin in &config.site_origins {
        command.arg("--allowed-origin").arg(origin);
    }
    if let Some(origin) = &persistent.relay_origin {
        // Browsers reach the relay through the ingress, which terminates
        // TLS and forwards to the relay's plain listener.
        command
            .arg("--web-plain")
            .arg(persistent.relay_plain.to_string())
            .arg("--public-origin")
            .arg(origin);
    } else if config.local {
        command.arg("--web-plain").arg(format!(
            "127.0.0.1:{}",
            config
                .port_base
                .saturating_add(crate::config::LOCAL_PLAIN_OFFSET)
        ));
    }
    command
        .arg("--persistent")
        .arg(&persistent.name)
        .arg("--root-file")
        .arg(&persistent.root_file)
        .arg("--room-id-file")
        .arg(dir.join("room-id"))
        .arg("--member-limits")
        .arg("--public-avatars")
        .arg(dir.join("avatars"))
        .arg("--public-avatars-url")
        .arg(format!("{}/avatars", config.public_api));
    if let Some(shown) = &persistent.shown_name {
        command.arg("--workspace-name").arg(shown);
    }
    for channel in &persistent.chat_channels {
        command.arg("--chat-channel").arg(channel);
    }
    for owner in &persistent.owners {
        command.arg("--owner").arg(owner);
    }
    for moderator in &persistent.moderators {
        command.arg("--moderator").arg(moderator);
    }
    for app in &persistent.seed_apps {
        command.arg("--seed-app").arg(app);
    }
    command
        .arg("--reset-every")
        .arg(persistent.reset_secs.to_string());
    size_sessions(&mut command, config, persistent);
    if !persistent.bots.is_empty() {
        for bot in &persistent.bots {
            command.arg("--bot").arg(bot);
        }
        // The token only matters to the bridge bot; without bots the
        // mounted file is left alone.
        if let Some(token) = &persistent.discord_token_file {
            command.arg("--discord-token-file").arg(token);
        }
    }
    // The moderation agent runs only with a key; the model is the
    // relay's default unless the operator names one.
    if let Some(key) = &persistent.moderation_key_file {
        command.arg("--moderation-key-file").arg(key);
        if let Some(model) = &persistent.moderation_model {
            command.arg("--moderation-model").arg(model);
        }
    }
    Ok(command)
}

/// Size the relay's session limits and allocator for the room's caps.
fn size_sessions(command: &mut Command, config: &Config, persistent: &PersistentConfig) {
    // Every guest and spectator holds a web session on the relay, whose own
    // defaults suit a small room: above them a visitor the service admitted
    // would get a broken page. Sized from the room's caps unless the
    // operator set them; the grant counts are what a 1000-guest load run
    // needed.
    let sessions = config
        .max_guests
        .saturating_add(persistent.max_spectators)
        .saturating_add(64);
    for (name, value) in [
        ("TILO_RELAY_MAX_WEB_SESSIONS", sessions),
        ("TILO_RELAY_MAX_HANDSHAKES", 1024),
        ("TILO_RELAY_MAX_LIVE_GRANTS", sessions.saturating_mul(4)),
        ("TILO_RELAY_MAX_ROOM_GRANTS", sessions.saturating_mul(16)),
    ] {
        if std::env::var_os(name).is_none() {
            command.env(name, value.to_string());
        }
    }
    // glibc's default of one malloc arena per core more than doubled the
    // relay's memory under hundreds of sessions, for no measurable speed.
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        command.env("MALLOC_ARENA_MAX", "2");
    }
}

impl Persistent {
    fn new(shared: Arc<Shared>) -> Arc<Self> {
        Arc::new(Self {
            shared,
            child: Mutex::new(None),
            waiters: std::sync::Mutex::default(),
        })
    }

    /// Opens the room's state and starts supervising its child.
    pub fn start(config: Arc<Config>) -> Result<Arc<Self>> {
        let persistent = config
            .persistent
            .as_ref()
            .context("not a persistent room")?;
        // Checked here too, so the refusal is in the service's own log and
        // not only in a restarting child's.
        ensure!(
            persistent.root_file.is_file(),
            "persistent mode refuses to start without its room root ({})",
            persistent.root_file.display()
        );
        let dir = config.state.join(&persistent.name);
        let shared = Arc::new(Shared::new(&config, &persistent.name, dir)?);
        let this = Self::new(shared);
        tokio::spawn(Arc::clone(&this).supervise(config));
        Ok(this)
    }

    async fn supervise(self: Arc<Self>, config: Arc<Config>) {
        let mut wait = Duration::from_secs(1);
        loop {
            let started = tokio::time::Instant::now();
            if let Err(error) = self.run_child(&config).await {
                eprintln!("tilo-room-service: the persistent room's relay stopped: {error:#}");
            }
            if started.elapsed() >= SETTLED {
                wait = Duration::from_secs(1);
            }
            tokio::time::sleep(wait).await;
            wait = wait.saturating_mul(2).min(Duration::from_secs(60));
        }
    }

    async fn run_child(&self, config: &Config) -> Result<()> {
        let mut child = child_command(config, &self.shared.dir)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("run tilo-relay room --persistent")?;
        let stdin = child.stdin.take().context("relay stdin")?;
        let stdout = child.stdout.take().context("relay stdout")?;
        let result = self
            .attach(Box::new(stdin), stdout, Some(config.persistent_origin()))
            .await;
        let _ = child.kill().await;
        result
    }

    /// Asks the running child to stop and waits for it a little, so the
    /// room's store is closed cleanly before the pod goes away.
    pub async fn shutdown(&self) {
        {
            let mut child = self.child.lock().await;
            if let Some(child) = child.as_mut() {
                let _ = child
                    .stdin
                    .write_all(OperatorCommand::Shutdown {}.to_line().as_bytes())
                    .await;
                let _ = child.stdin.flush().await;
            }
        }
        let started = tokio::time::Instant::now();
        while self.child.lock().await.is_some() && started.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Serves one child over its stdin and stdout until it ends.
    async fn attach(
        &self,
        stdin: OperatorInput,
        stdout: impl AsyncRead + Send + Unpin + 'static,
        origin: Option<String>,
    ) -> Result<()> {
        let mut lines = BufReader::new(stdout).lines();
        let started = read_started(&mut lines).await?;
        let room = started
            .room
            .context("the persistent room's relay named no governed room")?;
        *self.child.lock().await = Some(Child {
            stdin,
            origin: origin.unwrap_or(started.origin),
            room,
        });
        let result = self.observe(&mut lines).await;
        *self.child.lock().await = None;
        self.fail_waiters();
        result
    }

    async fn observe<R: AsyncRead + Unpin>(&self, lines: &mut Lines<BufReader<R>>) -> Result<()> {
        while let Some(line) = lines.next_line().await? {
            match RoomEvent::from_line(&line) {
                Err(LineError::NotProtocol) => {}
                Err(error) => return Err(error.into()),
                Ok(RoomEvent::PreAdmitted {
                    token_sha256,
                    invitation,
                }) => self.settle(
                    &format!("join:{token_sha256}"),
                    Ok(Answer::Invitation(invitation)),
                ),
                Ok(RoomEvent::PreAdmitRefused {
                    token_sha256,
                    reason,
                }) => {
                    eprintln!("tilo-room-service: a pre-admission was refused: {reason}");
                    self.settle(&format!("join:{token_sha256}"), Err(reason));
                }
                Ok(RoomEvent::SpectatorCertified {
                    device_public_key,
                    certificate,
                }) => self.settle(
                    &format!("spectate:{device_public_key}"),
                    serde_json::to_value(&certificate)
                        .map(Answer::Certificate)
                        .map_err(|error| error.to_string()),
                ),
                Ok(RoomEvent::SpectateRefused {
                    device_public_key,
                    reason,
                }) => {
                    eprintln!("tilo-room-service: a spectator was refused: {reason}");
                    self.settle(&format!("spectate:{device_public_key}"), Err(reason));
                }
                Ok(RoomEvent::Online { people, spectators }) => {
                    for (seats, online) in [
                        (&self.shared.seats, people),
                        (&self.shared.watching, spectators),
                    ] {
                        seats
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .counted(online);
                    }
                    if let Some(ops) = crate::ops::get() {
                        ops.set_online(u64::from(people), u64::from(spectators));
                    }
                }
                Ok(RoomEvent::MemberRemoved {
                    user_public_key,
                    banned,
                }) => {
                    let cooled = self
                        .shared
                        .guests
                        .lock()
                        .map_err(|_| anyhow::anyhow!("guest ledger poisoned"))?
                        .removed(&user_public_key, now_ms())?;
                    eprintln!(
                        "tilo-room-service: a guest was {}; their address {}",
                        if banned { "banned" } else { "removed" },
                        if cooled {
                            "cools down for a day"
                        } else {
                            "is not known"
                        }
                    );
                }
                Ok(
                    event @ (RoomEvent::Started { .. }
                    | RoomEvent::MemberCertified { .. }
                    | RoomEvent::AdmitRefused { .. }),
                ) => bail!("the persistent room's relay reported {event:?}"),
            }
        }
        Ok(())
    }

    fn settle(&self, key: &str, answer: std::result::Result<Answer, String>) {
        let waiter = self
            .waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(key));
        if let Some(waiter) = waiter {
            let _ = waiter.send(answer);
        }
    }

    fn fail_waiters(&self) {
        let drained: Vec<_> = self
            .waiters
            .lock()
            .map(|mut waiters| waiters.drain().collect())
            .unwrap_or_default();
        for (_, waiter) in drained {
            let _ = waiter.send(Err("the room restarted".into()));
        }
    }

    /// Sends `command` to the child and waits for the answer keyed `key`.
    /// Returns the answer with the child's origin and room.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the command is written under the lock so lines never interleave"
    )]
    async fn ask(
        &self,
        key: String,
        command: OperatorCommand,
    ) -> std::result::Result<(Answer, String, String), JoinRefusal> {
        let (sender, receiver) = oneshot::channel();
        let (origin, room) = {
            let mut child = self.child.lock().await;
            let Some(child) = child.as_mut() else {
                return Err(JoinRefusal::Unavailable);
            };
            if let Ok(mut waiters) = self.waiters.lock() {
                waiters.insert(key.clone(), sender);
            }
            if child
                .stdin
                .write_all(command.to_line().as_bytes())
                .await
                .is_err()
            {
                if let Ok(mut waiters) = self.waiters.lock() {
                    waiters.remove(&key);
                }
                return Err(JoinRefusal::Unavailable);
            }
            (child.origin.clone(), child.room.clone())
        };
        if let Ok(Ok(Ok(answer))) = tokio::time::timeout(ANSWER_WAIT, receiver).await {
            return Ok((answer, origin, room));
        }
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&key);
        }
        Err(JoinRefusal::Unavailable)
    }

    pub fn online(&self) -> u32 {
        Self::counted(&self.shared.seats)
    }

    pub fn watching(&self) -> u32 {
        Self::counted(&self.shared.watching)
    }

    fn counted(seats: &std::sync::Mutex<Seats>) -> u32 {
        seats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .online()
    }

    /// Gives back a seat taken by [`Self::admit_address`] for a visitor who
    /// got no answer.
    fn release(seats: &std::sync::Mutex<Seats>) {
        seats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release();
    }

    pub fn avatar_path(&self, name: &str) -> Option<PathBuf> {
        self.shared.avatar_path(name)
    }

    fn admit_address(
        &self,
        ip: IpAddr,
        limits: &std::sync::Mutex<JoinLimits>,
        seats: &std::sync::Mutex<Seats>,
    ) -> std::result::Result<String, JoinRefusal> {
        let ip_hash = self.shared.hasher.hash(ip);
        let now = now_ms();
        if self
            .shared
            .guests
            .lock()
            .map_err(|_| JoinRefusal::Unavailable)?
            .cooling_down(&ip_hash, now)
        {
            return Err(JoinRefusal::CoolingDown);
        }
        if !seats
            .lock()
            .map_err(|_| JoinRefusal::Unavailable)?
            .take(now)
        {
            return Err(JoinRefusal::Full {
                retry_after_secs: FULL_RETRY_SECS,
            });
        }
        let recorded = limits
            .lock()
            .map_err(|_| JoinRefusal::Unavailable)
            .and_then(|mut limits| {
                limits
                    .check_and_record(&ip_hash, now)
                    .map_err(|retry_after_secs| JoinRefusal::TooMany { retry_after_secs })
            });
        if let Err(refusal) = recorded {
            Self::release(seats);
            return Err(refusal);
        }
        Ok(ip_hash)
    }

    /// `POST /join`: pre-admit the visitor's own guest person.
    pub async fn join(
        &self,
        ip: IpAddr,
        request: JoinRequest,
    ) -> std::result::Result<JoinAnswer, JoinRefusal> {
        if hex32(&request.user_public_key).is_none() {
            return Err(JoinRefusal::Invalid(
                "userPublicKey must be 64 lowercase hex characters",
            ));
        }
        let name = printable_name(&request.display_name).map_err(JoinRefusal::Invalid)?;
        let ip_hash = self.admit_address(ip, &self.shared.joins, &self.shared.seats)?;
        let token = hex::encode(rand::random::<[u8; 32]>());
        let token_sha256 = token_digest(&token);
        let (answer, origin, _) = self
            .ask(
                format!("join:{token_sha256}"),
                OperatorCommand::PreAdmit {
                    user_public_key: request.user_public_key.clone(),
                    token_sha256: token_sha256.clone(),
                    label: if name.is_empty() {
                        "Guest".into()
                    } else {
                        name
                    },
                    role: RoomRole::Editor,
                },
            )
            .await
            .inspect_err(|_| Self::release(&self.shared.seats))?;
        let Answer::Invitation(invitation) = answer else {
            Self::release(&self.shared.seats);
            return Err(JoinRefusal::Unavailable);
        };
        if let Ok(mut guests) = self.shared.guests.lock()
            && let Err(error) = guests.record_join(&request.user_public_key, &ip_hash, now_ms())
        {
            eprintln!("tilo-room-service: could not record a join: {error:#}");
        }
        Ok(JoinAnswer {
            invitation,
            admission_token: token,
            origin,
        })
    }

    /// `POST /spectate`: certify a watching page's in-memory device as the
    /// room's spectator person.
    pub async fn spectate(
        &self,
        ip: IpAddr,
        request: SpectateRequest,
    ) -> std::result::Result<SpectateAnswer, JoinRefusal> {
        if hex32(&request.device_public_key).is_none() {
            return Err(JoinRefusal::Invalid(
                "devicePublicKey must be 64 lowercase hex characters",
            ));
        }
        self.admit_address(ip, &self.shared.spectates, &self.shared.watching)?;
        let (answer, origin, room) = self
            .ask(
                format!("spectate:{}", request.device_public_key),
                OperatorCommand::Spectate {
                    device_public_key: request.device_public_key,
                },
            )
            .await
            .inspect_err(|_| Self::release(&self.shared.watching))?;
        let Answer::Certificate(certificate) = answer else {
            Self::release(&self.shared.watching);
            return Err(JoinRefusal::Unavailable);
        };
        Ok(SpectateAnswer {
            certificate,
            origin,
            room,
        })
    }

    /// The room over an in-memory child, for tests: served until its
    /// stdout ends.
    #[cfg(test)]
    pub(crate) async fn attach_for_test(
        shared: Arc<Shared>,
        stdin: OperatorInput,
        stdout: impl AsyncRead + Send + Unpin + 'static,
    ) -> Arc<Self> {
        let this = Self::new(shared);
        let serving = Arc::clone(&this);
        tokio::spawn(async move {
            let _ = serving.attach(stdin, stdout, None).await;
        });
        for _ in 0..500 {
            if this.child.lock().await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        this
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use tilo_relay_protocol::room_operator;
    use tokio::io::{BufReader, DuplexStream, Lines};

    struct Relay {
        commands: Lines<BufReader<DuplexStream>>,
        events: DuplexStream,
    }

    impl Relay {
        async fn report(&mut self, event: &RoomEvent) {
            self.events
                .write_all(format!("{}\n", event.to_line()).as_bytes())
                .await
                .unwrap();
        }
        async fn command(&mut self) -> OperatorCommand {
            let line = tokio::time::timeout(Duration::from_secs(5), self.commands.next_line())
                .await
                .expect("no command")
                .unwrap()
                .expect("stdin closed");
            OperatorCommand::from_line(&line).unwrap()
        }
    }

    async fn persistent(
        max_guests: usize,
        max_spectators: usize,
    ) -> (Arc<Persistent>, Relay, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_test(dir.path(), max_guests, max_spectators);
        let (stdin, relay_stdin) = tokio::io::duplex(64 * 1024);
        let (mut relay_stdout, stdout) = tokio::io::duplex(64 * 1024);
        let started = RoomEvent::Started {
            version: room_operator::VERSION,
            origin: "https://rooms.example".into(),
            plain_origin: None,
            user_public_key: "ee".repeat(32),
            certificate_sha256: None,
            allowed_origins: Vec::new(),
            room: Some("room-uuid".into()),
        };
        relay_stdout
            .write_all(format!("{}\n", started.to_line()).as_bytes())
            .await
            .unwrap();
        let persistent = Persistent::attach_for_test(shared, Box::new(stdin), stdout).await;
        (
            persistent,
            Relay {
                commands: BufReader::new(relay_stdin).lines(),
                events: relay_stdout,
            },
            dir,
        )
    }

    fn request(key: u8) -> JoinRequest {
        JoinRequest {
            user_public_key: hex::encode([key; 32]),
            display_name: "Brave Otter".into(),
        }
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    async fn join_once(persistent: &Arc<Persistent>, relay: &mut Relay, address: &str, key: u8) {
        let joining = tokio::spawn({
            let persistent = Arc::clone(persistent);
            let address = ip(address);
            async move { persistent.join(address, request(key)).await }
        });
        let OperatorCommand::PreAdmit { token_sha256, .. } = relay.command().await else {
            panic!("expected a pre-admission");
        };
        relay
            .report(&RoomEvent::PreAdmitted {
                token_sha256,
                invitation: "INV".into(),
            })
            .await;
        joining.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_join_is_pre_admitted_for_the_guests_own_key() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        let joining = tokio::spawn({
            let persistent = Arc::clone(&persistent);
            async move { persistent.join(ip("203.0.113.1"), request(7)).await }
        });
        let OperatorCommand::PreAdmit {
            user_public_key,
            token_sha256,
            label,
            role,
        } = relay.command().await
        else {
            panic!("expected a pre-admission");
        };
        assert_eq!(user_public_key, hex::encode([7; 32]));
        assert_eq!(label, "Brave Otter");
        assert_eq!(role, RoomRole::Editor);
        relay
            .report(&RoomEvent::PreAdmitted {
                token_sha256: token_sha256.clone(),
                invitation: "INV".into(),
            })
            .await;
        let answer = joining.await.unwrap().unwrap();
        assert_eq!(answer.invitation, "INV");
        assert_eq!(answer.origin, "https://rooms.example");
        assert_eq!(token_digest(&answer.admission_token), token_sha256);
    }

    #[tokio::test]
    async fn stopping_tells_the_relay_to_shut_down_and_waits_for_it() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        let stopping = tokio::spawn({
            let persistent = Arc::clone(&persistent);
            async move { persistent.shutdown().await }
        });
        assert!(matches!(
            relay.command().await,
            OperatorCommand::Shutdown {}
        ));
        assert!(!stopping.is_finished());
        drop(relay);
        tokio::time::timeout(Duration::from_secs(5), stopping)
            .await
            .expect("stopping did not end with the relay")
            .unwrap();
    }

    #[tokio::test]
    async fn a_refused_pre_admission_answers_unavailable_at_once() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        let joining = tokio::spawn({
            let persistent = Arc::clone(&persistent);
            async move { persistent.join(ip("203.0.113.1"), request(7)).await }
        });
        let OperatorCommand::PreAdmit { token_sha256, .. } = relay.command().await else {
            panic!()
        };
        relay
            .report(&RoomEvent::PreAdmitRefused {
                token_sha256,
                reason: "full".into(),
            })
            .await;
        assert!(matches!(
            joining.await.unwrap(),
            Err(JoinRefusal::Unavailable)
        ));
    }

    #[tokio::test]
    async fn the_room_is_full_at_the_online_cap() {
        let (persistent, mut relay, _dir) = persistent(2, 1000).await;
        relay
            .report(&RoomEvent::Online {
                people: 2,
                spectators: 5,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!((persistent.online(), persistent.watching()), (2, 5));
        assert!(matches!(
            persistent.join(ip("203.0.113.1"), request(7)).await,
            Err(JoinRefusal::Full {
                retry_after_secs: FULL_RETRY_SECS
            })
        ));
    }

    #[tokio::test]
    async fn admitted_guests_fill_the_room_before_the_relay_counts_them() {
        let (persistent, mut relay, _dir) = persistent(2, 1000).await;
        join_once(&persistent, &mut relay, "203.0.113.1", 7).await;
        join_once(&persistent, &mut relay, "203.0.113.2", 8).await;
        assert!(matches!(
            persistent.join(ip("203.0.113.3"), request(9)).await,
            Err(JoinRefusal::Full { .. })
        ));
        // Both came online and one left: one seat is free again.
        relay
            .report(&RoomEvent::Online {
                people: 2,
                spectators: 0,
            })
            .await;
        relay
            .report(&RoomEvent::Online {
                people: 1,
                spectators: 0,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        join_once(&persistent, &mut relay, "203.0.113.3", 9).await;
    }

    #[tokio::test]
    async fn a_removed_guest_cannot_join_again_from_the_same_address_for_a_day() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        join_once(&persistent, &mut relay, "203.0.113.1", 7).await;
        relay
            .report(&RoomEvent::MemberRemoved {
                user_public_key: hex::encode([7; 32]),
                banned: false,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            persistent.join(ip("203.0.113.1"), request(8)).await,
            Err(JoinRefusal::CoolingDown)
        ));
        // Another address is not affected.
        join_once(&persistent, &mut relay, "203.0.113.2", 9).await;
    }

    #[tokio::test]
    async fn the_sixth_join_in_ten_minutes_from_one_address_waits() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        for key in 0..5u8 {
            join_once(&persistent, &mut relay, "203.0.113.1", key).await;
        }
        assert!(matches!(
            persistent.join(ip("203.0.113.1"), request(9)).await,
            Err(JoinRefusal::TooMany { .. })
        ));
    }

    #[tokio::test]
    async fn a_pending_join_ends_when_the_relay_goes_away() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        let joining = tokio::spawn({
            let persistent = Arc::clone(&persistent);
            async move { persistent.join(ip("203.0.113.1"), request(7)).await }
        });
        relay.command().await;
        drop(relay);
        let ended = tokio::time::timeout(Duration::from_secs(5), joining)
            .await
            .expect("the join hung")
            .unwrap();
        assert!(matches!(ended, Err(JoinRefusal::Unavailable)));
    }

    #[tokio::test]
    async fn a_name_of_100_bytes_joins_as_written() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        for (key, name) in [
            (8, "😀".repeat(25)),
            (9, "界".repeat(33)),
            (10, "x".repeat(100)),
        ] {
            let joining = tokio::spawn({
                let persistent = Arc::clone(&persistent);
                let display_name = name.clone();
                async move {
                    persistent
                        .join(
                            ip(&format!("203.0.113.{key}")),
                            JoinRequest {
                                user_public_key: hex::encode([key; 32]),
                                display_name,
                            },
                        )
                        .await
                }
            });
            let OperatorCommand::PreAdmit {
                token_sha256,
                label,
                ..
            } = relay.command().await
            else {
                panic!("expected a pre-admission");
            };
            assert_eq!(label, name);
            relay
                .report(&RoomEvent::PreAdmitted {
                    token_sha256,
                    invitation: "INV".into(),
                })
                .await;
            joining.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn bad_join_requests_are_refused_before_anything_is_spent() {
        let (persistent, _relay, _dir) = persistent(200, 1000).await;
        let bad_key = JoinRequest {
            user_public_key: "zz".into(),
            display_name: String::new(),
        };
        assert!(matches!(
            persistent.join(ip("203.0.113.1"), bad_key).await,
            Err(JoinRefusal::Invalid(_))
        ));
        let long_name = JoinRequest {
            user_public_key: hex::encode([1; 32]),
            display_name: "界".repeat(43),
        };
        assert!(matches!(
            persistent.join(ip("203.0.113.1"), long_name).await,
            Err(JoinRefusal::Invalid("the name is longer than 128 bytes"))
        ));
    }

    #[tokio::test]
    async fn a_spectator_gets_the_childs_certificate_and_the_room() {
        let (persistent, mut relay, _dir) = persistent(200, 1000).await;
        let device = hex::encode([4; 32]);
        let watching = tokio::spawn({
            let persistent = Arc::clone(&persistent);
            let device = device.clone();
            async move {
                persistent
                    .spectate(
                        ip("203.0.113.1"),
                        SpectateRequest {
                            device_public_key: device,
                        },
                    )
                    .await
            }
        });
        let OperatorCommand::Spectate { device_public_key } = relay.command().await else {
            panic!("expected a spectate command");
        };
        assert_eq!(device_public_key, device);
        let certificate = tilo_user_identity_cert();
        relay
            .report(&RoomEvent::SpectatorCertified {
                device_public_key,
                certificate: certificate.clone(),
            })
            .await;
        let answer = watching.await.unwrap().unwrap();
        assert_eq!(answer.room, "room-uuid");
        assert_eq!(answer.origin, "https://rooms.example");
        assert_eq!(
            answer.certificate,
            serde_json::to_value(&certificate).unwrap()
        );
    }

    #[tokio::test]
    async fn spectators_have_their_own_cap() {
        let (persistent, mut relay, _dir) = persistent(1, 3).await;
        relay
            .report(&RoomEvent::Online {
                people: 1,
                spectators: 3,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            persistent
                .spectate(
                    ip("203.0.113.1"),
                    SpectateRequest {
                        device_public_key: hex::encode([4; 32]),
                    },
                )
                .await,
            Err(JoinRefusal::Full { .. })
        ));
    }

    fn tilo_user_identity_cert() -> tilo_user_identity::DeviceCert {
        tilo_user_identity::DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [4; 32],
            device_label: "Watching visitor".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: Some(2),
            signature: [3; 64],
        }
    }

    #[test]
    fn only_published_avatar_names_map_to_files() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_test(dir.path(), 1, 1);
        assert!(shared.avatar_path("../guests.json").is_none());
        assert!(shared.avatar_path(&"ab".repeat(32)).is_some());
    }

    #[test]
    fn a_direct_live_origin_gives_the_relay_its_own_certificate_and_keeps_the_ingress() {
        let vars = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_RELAY_ORIGIN", "https://rooms.example"),
            ("TILO_ROOMS_LIVE_ORIGIN", "https://live.rooms.example"),
        ];
        let config = Config::from_lookup(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
        .unwrap();
        let command = child_command(&config, Path::new("/state")).unwrap();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].as_str()
        };
        assert_eq!(after("--web"), "0.0.0.0:9781");
        assert_eq!(
            after("--tls"),
            "files:/run/live-tls/tls.crt,/run/live-tls/tls.key"
        );
        assert_eq!(after("--public-host"), "live.rooms.example");
        assert_eq!(after("--live-origin"), "https://live.rooms.example");
        assert_eq!(after("--public-origin"), "https://rooms.example");
        assert_eq!(after("--web-plain"), "0.0.0.0:9780");
    }

    #[test]
    fn the_relay_opens_chat_with_the_channels_the_operator_names() {
        let channel_args = |vars: &[(&str, &str)]| {
            let config = Config::from_lookup(|name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            })
            .unwrap();
            let command = child_command(&config, Path::new("/state")).unwrap();
            let args: Vec<String> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            args.windows(2)
                .filter(|pair| pair[0] == "--chat-channel")
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };
        let base = [
            ("TILO_ROOMS_PERSISTENT", "plaza"),
            ("TILO_ROOMS_RELAY_ORIGIN", "https://rooms.example"),
        ];
        assert!(channel_args(&base).is_empty());
        assert_eq!(
            channel_args(&[
                base[0],
                base[1],
                ("TILO_ROOMS_CHAT_CHANNELS", "general,show-and-tell"),
            ]),
            ["general", "show-and-tell"]
        );
    }
}
