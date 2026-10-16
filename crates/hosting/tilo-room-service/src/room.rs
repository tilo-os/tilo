//! One room: a `tilo-relay room` process on its own state directory, with
//! its own throwaway user root. Browsers join it by device certification;
//! the relay certifies a browser only once this service admitted its
//! pairing code against a single-use ticket. The two processes speak
//! `tilo_relay_protocol::room_operator`. Nothing outlives the room: the
//! directory is removed when it expires or its relay exits.
use crate::config::Config;
use crate::tls::HostIdentity;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tilo_relay_protocol::room_operator::{
    self, LineError, OperatorCommand, RefusalReason, RoomEvent,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, broadcast, oneshot};

/// How long the relay may take to announce that it serves browsers.
const START_WAIT: Duration = Duration::from_secs(60);
/// How long an admission waits for the relay's answer: the relay's own
/// bound on an admitted code, and a margin for the answer to arrive.
const ADMIT_ANSWER_WAIT: Duration = Duration::from_secs(room_operator::ADMIT_WAIT_SECS + 10);

/// A browser the room certified, as `/rooms/<id>/admit` and the `member`
/// event report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub label: String,
    pub device: String,
    pub pairing_code: String,
    pub at: u64,
}

/// What a room announces to its subscribers.
#[derive(Clone, Debug)]
pub enum Event {
    Member(Member),
    Expired,
}

/// The relay's answer to one admission: the browser it certified, or why
/// it certified nobody.
type Answer = std::result::Result<Member, RefusalReason>;

/// Where a single-use ticket stands.
#[derive(Debug)]
enum TicketState {
    Free,
    /// The relay was told to certify the browser showing `pairing_code` and
    /// has not answered yet; the ticket admits nobody else meanwhile. The
    /// answer goes to exactly the admission that asked, so it can be
    /// neither missed nor mistaken for an earlier admission of the same
    /// code.
    Admitting {
        pairing_code: String,
        answer: oneshot::Sender<Answer>,
    },
    /// A browser was certified with it.
    Spent,
}

impl TicketState {
    fn admits(&self, code: &str) -> bool {
        matches!(self, Self::Admitting { pairing_code, .. } if pairing_code == code)
    }
}

/// A single-use permission to have one browser device certified.
struct Ticket {
    secret: String,
    state: TicketState,
}

/// Where a room lives, decided before its relay starts.
struct Placement {
    id: String,
    port: u16,
    origin: String,
    expires_at_ms: u64,
    directory: PathBuf,
}

type OperatorInput = Box<dyn AsyncWrite + Send + Unpin>;

/// The `--state` of a room's relay, under the room's directory.
pub(crate) fn relay_state(directory: &Path) -> PathBuf {
    directory.join("state")
}

pub struct Room {
    pub id: String,
    pub port: u16,
    pub origin: String,
    /// The room uuid pages join (`connectPeer({room})`).
    pub uuid: String,
    /// The room's user root (`connectPeer({userPublicKey})`).
    pub user_public_key: String,
    pub expires_at_ms: u64,
    /// The capability of whoever created the room (the page holding the
    /// `POST /rooms` answer) to mint invitations and watch its members. It
    /// appears in no guest link, and neither the room's id (its hostname)
    /// nor its uuid (in every guest link) leads to it.
    owner_key: String,
    stdin: Mutex<OperatorInput>,
    events: broadcast::Sender<Event>,
    history: std::sync::Mutex<Vec<Member>>,
    tickets: std::sync::Mutex<Vec<Ticket>>,
    /// Notified once when the relay breaks the operator protocol.
    broken: Notify,
    directory: PathBuf,
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

fn uuid_v4() -> String {
    let mut bytes = rand::random::<[u8; 16]>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let part = |range: std::ops::Range<usize>| hex::encode(bytes.get(range).unwrap_or_default());
    format!(
        "{}-{}-{}-{}-{}",
        part(0..4),
        part(4..6),
        part(6..8),
        part(8..10),
        part(10..16)
    )
}

/// A fresh room owner key: 32 bytes from the operating system, in hex.
fn owner_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("room owner key: {error}"))?;
    Ok(hex::encode(bytes))
}

fn deadline(after: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(after).unwrap_or(now)
}

/// What a room relay announced when it started.
pub(crate) struct Started {
    pub user_public_key: String,
    pub origin: String,
    /// The governed room of a persistent room's relay.
    pub room: Option<String>,
}

/// Reads the relay's stdout up to its [`RoomEvent::Started`]. Lines without
/// the protocol prefix (logs) are passed over; any other protocol line
/// first, or another protocol version, is an error.
pub(crate) async fn read_started<R: AsyncRead + Unpin>(
    lines: &mut Lines<BufReader<R>>,
) -> Result<Started> {
    let started = deadline(START_WAIT);
    loop {
        let line = tokio::time::timeout_at(started, lines.next_line())
            .await
            .context("the room relay did not start in time")??
            .context("the room relay exited before it started")?;
        match RoomEvent::from_line(&line) {
            Err(LineError::NotProtocol) => {}
            Err(error) => return Err(error).context("the room relay's first protocol line"),
            Ok(RoomEvent::Started {
                version,
                user_public_key,
                origin,
                room,
                ..
            }) => {
                ensure!(
                    version == room_operator::VERSION,
                    "the room relay speaks operator protocol {version}, this service {}",
                    room_operator::VERSION
                );
                return Ok(Started {
                    user_public_key,
                    origin,
                    room,
                });
            }
            Ok(other) => bail!("the room relay reported {other:?} before it started"),
        }
    }
}

impl Room {
    /// Starts the room's relay and waits until it serves browsers.
    pub async fn start(
        config: &Config,
        identity: &HostIdentity,
        id: String,
        port: u16,
    ) -> Result<(Arc<Self>, Child)> {
        let directory = config.state.join(&id);
        std::fs::create_dir(&directory).context("create room directory")?;
        let (chain, key) = (
            directory.join("host-cert.pem"),
            directory.join("host-key.pem"),
        );
        write_private(&chain, identity.certificate_pem.as_bytes())?;
        write_private(&key, identity.key_pem.as_bytes())?;
        let mut command = Command::new(&config.relay_binary);
        command
            .env("TILO_RELAY_LOG", "warn")
            .arg("room")
            .arg("--state")
            .arg(relay_state(&directory))
            .arg("--web")
            .arg(if config.direct_ports {
                format!("0.0.0.0:{port}")
            } else {
                format!("127.0.0.1:{port}")
            })
            .arg("--tls")
            .arg(
                match (config.direct_ports || config.local, &config.certificate) {
                    // Browsers reach the relay itself: the public certificate.
                    (true, Some((public_chain, public_key))) => {
                        format!("files:{},{}", public_chain.display(), public_key.display())
                    }
                    // Local: the relay's development certificate, which the
                    // page's WebTransport pins by the hash the relay announces.
                    (true, None) => "dev".to_owned(),
                    // Behind the TLS front: the internal certificate it pins.
                    (false, _) => format!("files:{},{}", chain.display(), key.display()),
                },
            )
            .arg("--allowed-origin")
            .arg(config.api_origin());
        for origin in &config.site_origins {
            command.arg("--allowed-origin").arg(origin);
        }
        if config.local {
            command.arg("--web-plain").arg(format!(
                "127.0.0.1:{}",
                port.saturating_add(crate::config::LOCAL_PLAIN_OFFSET)
            ));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("run tilo-relay room")?;
        let stdin = child.stdin.take().context("relay stdin")?;
        let stdout = child.stdout.take().context("relay stdout")?;
        let placement = Placement {
            origin: config.origin(&id, port),
            id,
            port,
            expires_at_ms: tilo_clock::unix_now_ms()
                .saturating_add(u64::try_from(config.ttl.as_millis()).unwrap_or(u64::MAX)),
            directory,
        };
        let room = Self::attach(placement, Box::new(stdin), stdout).await?;
        Ok((room, child))
    }

    /// The room over its relay's stdin and stdout, once the relay started.
    async fn attach(
        placement: Placement,
        stdin: OperatorInput,
        stdout: impl AsyncRead + Send + Unpin + 'static,
    ) -> Result<Arc<Self>> {
        let mut lines = BufReader::new(stdout).lines();
        let user_public_key = read_started(&mut lines).await?.user_public_key;
        let (events, _) = broadcast::channel(64);
        let room = Arc::new(Self {
            id: placement.id,
            port: placement.port,
            origin: placement.origin,
            uuid: uuid_v4(),
            user_public_key,
            expires_at_ms: placement.expires_at_ms,
            owner_key: owner_key()?,
            stdin: Mutex::new(stdin),
            events,
            history: std::sync::Mutex::default(),
            tickets: std::sync::Mutex::default(),
            broken: Notify::new(),
            directory: placement.directory,
        });
        let reader = Arc::clone(&room);
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Err(error) = reader.observe(&line) {
                    eprintln!("tilo-room-service: room {}: {error:#}", reader.id);
                    reader.broken.notify_one();
                    break;
                }
            }
            // Nothing will answer a pending admission any more.
            reader.abandon_admissions();
        });
        Ok(room)
    }

    /// The owner's key, for the URLs only the room's creator is given.
    pub fn owner_key(&self) -> &str {
        &self.owner_key
    }

    /// Whether `presented` is the room's owner key, compared in constant
    /// time so the answer's timing says nothing about how much matched.
    pub fn owned_by(&self, presented: &str) -> bool {
        use subtle::ConstantTimeEq;
        presented.len() == self.owner_key.len()
            && bool::from(presented.as_bytes().ct_eq(self.owner_key.as_bytes()))
    }

    /// Resolves once the relay broke the operator protocol; the room must
    /// then be stopped.
    pub async fn broken(&self) {
        self.broken.notified().await;
    }

    /// Takes in one line of the relay's stdout.
    fn observe(&self, line: &str) -> Result<()> {
        match RoomEvent::from_line(line) {
            // A short-lived room shows no presence count.
            Err(LineError::NotProtocol) | Ok(RoomEvent::Online { .. }) => {}
            Err(error) => return Err(error.into()),
            Ok(RoomEvent::Started { .. }) => bail!("the room relay announced its start twice"),
            Ok(RoomEvent::MemberCertified {
                device,
                label,
                pairing_code,
            }) => {
                let member = Member {
                    label,
                    device,
                    pairing_code,
                    at: tilo_clock::unix_now_ms(),
                };
                if let Ok(mut history) = self.history.lock() {
                    history.push(member.clone());
                    drop(history);
                }
                self.settle(&member.pairing_code, Ok(member.clone()));
                let _ = self.events.send(Event::Member(member));
            }
            Ok(RoomEvent::AdmitRefused {
                pairing_code,
                reason,
            }) => self.settle(&pairing_code, Err(reason)),
            // A short-lived room never pre-admits, spectates or removes.
            Ok(
                event @ (RoomEvent::PreAdmitted { .. }
                | RoomEvent::PreAdmitRefused { .. }
                | RoomEvent::SpectatorCertified { .. }
                | RoomEvent::SpectateRefused { .. }
                | RoomEvent::MemberRemoved { .. }),
            ) => bail!("a short-lived room relay reported {event:?}"),
        }
        Ok(())
    }

    /// A new single-use ticket, unless the room already issued `limit`.
    pub fn ticket(&self, limit: usize) -> Option<String> {
        let mut tickets = self.tickets.lock().ok()?;
        if tickets.len() >= limit {
            return None;
        }
        let secret = hex::encode(rand::random::<[u8; 20]>());
        tickets.push(Ticket {
            secret: secret.clone(),
            state: TicketState::Free,
        });
        drop(tickets);
        Some(secret)
    }

    /// Holds `ticket` for the browser that shows `pairing_code` until the
    /// relay answers, and returns where that answer will arrive.
    fn reserve(&self, ticket: &str, pairing_code: &str) -> Result<oneshot::Receiver<Answer>> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| anyhow::anyhow!("tickets poisoned"))?;
        ensure!(
            !tickets.iter().any(|entry| entry.state.admits(pairing_code)),
            "this pairing code is already being admitted"
        );
        let entry = tickets
            .iter_mut()
            .find(|entry| entry.secret == ticket)
            .context("this invitation was already used or does not exist")?;
        match entry.state {
            TicketState::Free => {}
            TicketState::Admitting { .. } => {
                bail!("this invitation is already admitting a device")
            }
            TicketState::Spent => bail!("this invitation was already used or does not exist"),
        }
        let (answer, answered) = oneshot::channel();
        entry.state = TicketState::Admitting {
            pairing_code: pairing_code.to_owned(),
            answer,
        };
        drop(tickets);
        Ok(answered)
    }

    /// Settles the ticket admitting `pairing_code` with the relay's answer:
    /// spent on a certified browser, usable again after a refusal.
    fn settle(&self, pairing_code: &str, answer: Answer) {
        let Ok(mut tickets) = self.tickets.lock() else {
            return;
        };
        let Some(entry) = tickets
            .iter_mut()
            .find(|entry| entry.state.admits(pairing_code))
        else {
            drop(tickets);
            eprintln!(
                "tilo-room-service: room {}: an answer for a code no ticket is admitting",
                self.id
            );
            return;
        };
        let settled = if answer.is_ok() {
            TicketState::Spent
        } else {
            TicketState::Free
        };
        if let TicketState::Admitting { answer: waiter, .. } =
            std::mem::replace(&mut entry.state, settled)
        {
            // The admission may have stopped waiting; the ticket is settled
            // all the same.
            let _ = waiter.send(answer);
        }
        drop(tickets);
    }

    /// Frees the ticket admitting `pairing_code` without an answer: the
    /// relay was never told.
    fn release(&self, pairing_code: &str) {
        let Ok(mut tickets) = self.tickets.lock() else {
            return;
        };
        for entry in tickets.iter_mut() {
            if entry.state.admits(pairing_code) {
                entry.state = TicketState::Free;
            }
        }
        drop(tickets);
    }

    /// Ends every pending admission, whose waiters then learn the room
    /// ended: the relay will answer none of them.
    fn abandon_admissions(&self) {
        let Ok(mut tickets) = self.tickets.lock() else {
            return;
        };
        for entry in tickets.iter_mut() {
            if matches!(entry.state, TicketState::Admitting { .. }) {
                entry.state = TicketState::Free;
            }
        }
        drop(tickets);
    }

    /// Spends `ticket` on the browser that displays `pairing_code`: tells
    /// the relay to certify it and waits until it did. The ticket stays
    /// usable when the relay certified nobody.
    pub async fn admit(&self, ticket: &str, pairing_code: &str) -> Result<Member> {
        ensure!(
            !pairing_code.is_empty()
                && pairing_code.len() <= 64
                && pairing_code
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' '),
            "invalid pairing code"
        );
        let answered = self.reserve(ticket, pairing_code)?;
        let admit = OperatorCommand::Admit {
            pairing_code: pairing_code.to_owned(),
        };
        if let Err(error) = self.tell(&admit).await {
            self.release(pairing_code);
            return Err(error);
        }
        match tokio::time::timeout_at(deadline(ADMIT_ANSWER_WAIT), answered).await {
            Ok(Ok(Ok(member))) => Ok(member),
            Ok(Ok(Err(reason))) => bail!("{}", reason.message()),
            Ok(Err(_)) => bail!("the room ended"),
            Err(_) => bail!("the room did not answer in time"),
        }
    }

    /// Writes one operator command to the relay.
    async fn tell(&self, command: &OperatorCommand) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(command.to_line().as_bytes()).await?;
        stdin.flush().await?;
        drop(stdin);
        Ok(())
    }

    pub fn subscribe(&self) -> (Vec<Member>, broadcast::Receiver<Event>) {
        let receiver = self.events.subscribe();
        let history = self
            .history
            .lock()
            .map(|history| history.clone())
            .unwrap_or_default();
        (history, receiver)
    }

    /// Asks the relay to stop, waits briefly, then kills it and removes
    /// every byte the room wrote.
    pub async fn stop(&self, mut child: Child) {
        let _ = self.events.send(Event::Expired);
        self.abandon_admissions();
        let _ = self.tell(&OperatorCommand::Shutdown {}).await;
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A room on in-memory pipes, with the relay's ends to speak for it.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use tokio::io::DuplexStream;

    /// The relay's ends of a room's pipes.
    pub(crate) struct Relay {
        commands: Lines<BufReader<DuplexStream>>,
        events: DuplexStream,
    }

    impl Relay {
        pub(crate) async fn say(&mut self, line: &str) {
            self.events
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }

        pub(crate) async fn report(&mut self, event: &RoomEvent) {
            self.say(&event.to_line()).await;
        }

        pub(crate) async fn command(&mut self) -> OperatorCommand {
            let line = tokio::time::timeout(Duration::from_secs(5), self.commands.next_line())
                .await
                .expect("the service sent no command")
                .unwrap()
                .expect("the service closed stdin");
            OperatorCommand::from_line(&line).unwrap()
        }
    }

    pub(crate) fn started(version: u16, user: &str) -> RoomEvent {
        RoomEvent::Started {
            version,
            origin: "https://127.0.0.1:9700".into(),
            plain_origin: None,
            user_public_key: user.into(),
            certificate_sha256: None,
            allowed_origins: Vec::new(),
            room: None,
        }
    }

    fn placement(id: &str) -> Placement {
        Placement {
            id: id.into(),
            port: 9700,
            origin: format!("https://{id}.localhost"),
            expires_at_ms: u64::MAX,
            directory: PathBuf::from("/nonexistent/tilo-room-test"),
        }
    }

    /// Attaches a room to in-memory pipes after the relay said `first`.
    pub(crate) async fn attach_after(first: &[String]) -> (Result<Arc<Room>>, Relay) {
        attach_as("room", first).await
    }

    /// [`attach_after`] for the room with service id `id`.
    async fn attach_as(id: &str, first: &[String]) -> (Result<Arc<Room>>, Relay) {
        let (stdin, relay_stdin) = tokio::io::duplex(64 * 1024);
        let (mut relay_stdout, stdout) = tokio::io::duplex(64 * 1024);
        for line in first {
            relay_stdout
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }
        let room = Room::attach(placement(id), Box::new(stdin), stdout).await;
        let relay = Relay {
            commands: BufReader::new(relay_stdin).lines(),
            events: relay_stdout,
        };
        (room, relay)
    }

    pub(crate) async fn running_room() -> (Arc<Room>, Relay) {
        running_room_as("room").await
    }

    /// A started room with service id `id` on in-memory pipes; the relay's
    /// ends must be kept, or the room sees its relay gone.
    pub(crate) async fn running_room_as(id: &str) -> (Arc<Room>, Relay) {
        let (room, relay) = attach_as(id, &[started(room_operator::VERSION, "ab").to_line()]).await;
        (room.unwrap(), relay)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::testing::{attach_after, running_room, started};
    use super::*;

    #[tokio::test]
    async fn each_room_has_its_own_unguessable_owner_key_and_only_it_opens_the_room() {
        let (room, _relay) = running_room().await;
        let (other, _other_relay) = running_room().await;
        let key = room.owner_key().to_owned();
        assert_eq!(key.len(), 64, "32 bytes in hex: {key}");
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()), "{key}");
        assert_ne!(key, other.owner_key(), "two rooms share an owner key");
        assert!(room.owned_by(&key));
        for wrong in [
            String::new(),
            room.id.clone(),
            room.uuid.clone(),
            other.owner_key().to_owned(),
            key.get(..63).unwrap().to_owned(),
            format!("{key}0"),
            key.to_uppercase(),
        ] {
            assert!(!room.owned_by(&wrong), "{wrong:?} opened the room");
        }
    }

    #[tokio::test]
    async fn the_room_root_comes_from_the_started_event_not_from_a_log_line() {
        let (room, _relay) = attach_after(&[
            "WARN relay: starting".into(),
            "{\"userPublicKey\":\"ff\"}".into(),
            started(room_operator::VERSION, "ab").to_line(),
        ])
        .await;
        assert_eq!(room.unwrap().user_public_key, "ab");
    }

    #[tokio::test]
    async fn a_relay_of_another_protocol_version_or_a_malformed_start_is_refused() {
        let (room, _relay) =
            attach_after(&[started(room_operator::VERSION + 1, "ab").to_line()]).await;
        assert!(format!("{:#}", room.err().unwrap()).contains("operator protocol"));
        let (room, _relay) = attach_after(&[format!(
            "{}{{\"event\":\"started\",\"userPublicKey\":\"ab\"}}",
            room_operator::LINE_PREFIX
        )])
        .await;
        assert!(room.is_err(), "a malformed start line was accepted");
        let (room, _relay) = attach_after(&[RoomEvent::MemberCertified {
            device: "cd".into(),
            label: "x".into(),
            pairing_code: "1".into(),
        }
        .to_line()])
        .await;
        assert!(room.is_err(), "a member before the start was accepted");
    }

    #[tokio::test]
    async fn a_refused_admission_says_why_and_leaves_the_ticket_usable_until_a_certificate() {
        let (room, mut relay) = running_room().await;
        let ticket = room.ticket(2).unwrap();

        let admitting = tokio::spawn({
            let (room, ticket) = (Arc::clone(&room), ticket.clone());
            async move { room.admit(&ticket, "111111").await }
        });
        assert_eq!(
            relay.command().await,
            OperatorCommand::Admit {
                pairing_code: "111111".into()
            }
        );
        relay
            .report(&RoomEvent::AdmitRefused {
                pairing_code: "111111".into(),
                reason: RefusalReason::NoMatchingRequest,
            })
            .await;
        let refused = admitting.await.unwrap().unwrap_err();
        assert_eq!(
            refused.to_string(),
            RefusalReason::NoMatchingRequest.message()
        );

        let admitting = tokio::spawn({
            let (room, ticket) = (Arc::clone(&room), ticket.clone());
            async move { room.admit(&ticket, "222222").await }
        });
        assert_eq!(
            relay.command().await,
            OperatorCommand::Admit {
                pairing_code: "222222".into()
            }
        );
        relay
            .report(&RoomEvent::MemberCertified {
                device: "cd".repeat(32),
                label: "Phone".into(),
                pairing_code: "222222".into(),
            })
            .await;
        let member = admitting.await.unwrap().unwrap();
        assert_eq!(
            (
                member.device.as_str(),
                member.label.as_str(),
                member.pairing_code.as_str()
            ),
            ("cd".repeat(32).as_str(), "Phone", "222222")
        );
        let json = serde_json::to_value(&member).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["at", "device", "label", "pairingCode"],
            "the admit answer keeps its HTTP shape"
        );
        assert_eq!(room.subscribe().0, vec![member]);

        let spent = room.admit(&ticket, "333333").await.unwrap_err();
        assert!(spent.to_string().contains("already used"), "{spent:#}");
    }

    #[tokio::test]
    async fn a_ticket_or_a_code_admits_one_browser_at_a_time() {
        let (room, mut relay) = running_room().await;
        let (first, second) = (room.ticket(3).unwrap(), room.ticket(3).unwrap());
        let admitting = tokio::spawn({
            let (room, ticket) = (Arc::clone(&room), first.clone());
            async move { room.admit(&ticket, "111111").await }
        });
        relay.command().await;
        let busy = room.admit(&first, "222222").await.unwrap_err();
        assert!(busy.to_string().contains("already admitting"), "{busy:#}");
        let same_code = room.admit(&second, "111111").await.unwrap_err();
        assert!(
            same_code.to_string().contains("already being admitted"),
            "{same_code:#}"
        );
        relay
            .report(&RoomEvent::MemberCertified {
                device: "cd".into(),
                label: "Phone".into(),
                pairing_code: "111111".into(),
            })
            .await;
        admitting.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_answer_reaches_its_admission_however_busy_the_room_and_never_an_older_one() {
        let (room, mut relay) = running_room().await;
        let (first, second) = (room.ticket(2).unwrap(), room.ticket(2).unwrap());
        let admitting = tokio::spawn({
            let (room, ticket) = (Arc::clone(&room), first.clone());
            async move { room.admit(&ticket, "111111").await }
        });
        relay.command().await;
        let certified = RoomEvent::MemberCertified {
            device: "cd".into(),
            label: "Phone".into(),
            pairing_code: "111111".into(),
        };
        relay.report(&certified).await;
        admitting.await.unwrap().unwrap();

        // The same code again, on another ticket, while more members are
        // announced than the room's event channel holds.
        let admitting = tokio::spawn({
            let (room, ticket) = (Arc::clone(&room), second.clone());
            async move { room.admit(&ticket, "111111").await }
        });
        relay.command().await;
        for index in 0..200 {
            relay
                .report(&RoomEvent::MemberCertified {
                    device: format!("{index:02x}"),
                    label: "Other".into(),
                    pairing_code: format!("other-{index}"),
                })
                .await;
        }
        relay
            .report(&RoomEvent::AdmitRefused {
                pairing_code: "111111".into(),
                reason: RefusalReason::NoMatchingRequest,
            })
            .await;
        let refused = tokio::time::timeout(Duration::from_secs(5), admitting)
            .await
            .expect("the refusal did not reach its admission")
            .unwrap()
            .expect_err("an earlier certificate of the same code answered this admission");
        assert_eq!(
            refused.to_string(),
            RefusalReason::NoMatchingRequest.message()
        );
    }

    #[tokio::test]
    async fn a_pending_admission_ends_with_its_relay() {
        let (room, mut relay) = running_room().await;
        let ticket = room.ticket(1).unwrap();
        let admitting = tokio::spawn({
            let room = Arc::clone(&room);
            async move { room.admit(&ticket, "111111").await }
        });
        relay.command().await;
        drop(relay);
        let ended = tokio::time::timeout(Duration::from_secs(5), admitting)
            .await
            .expect("the admission outlived its relay")
            .unwrap()
            .unwrap_err();
        assert_eq!(ended.to_string(), "the room ended");
    }

    #[tokio::test]
    async fn a_malformed_protocol_line_breaks_the_room_instead_of_being_skipped() {
        let (room, mut relay) = running_room().await;
        relay.say("INFO relay: a log line is not protocol").await;
        relay
            .say(&format!(
                "{}{{\"event\":\"member_certified\",\"pairingCode\":\"1\"}}",
                room_operator::LINE_PREFIX
            ))
            .await;
        tokio::time::timeout(Duration::from_secs(5), room.broken())
            .await
            .expect("the room kept going after a malformed protocol line");
    }
}
