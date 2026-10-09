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
use tokio::sync::{Mutex, Notify, broadcast};

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
    /// An admitted pairing code certified nobody.
    Refused {
        pairing_code: String,
        reason: RefusalReason,
    },
    Expired,
}

/// Where a single-use ticket stands.
#[derive(Debug, PartialEq, Eq)]
enum TicketState {
    Free,
    /// The relay was told to certify the browser showing this code and has
    /// not answered yet; the ticket admits nobody else meanwhile.
    Admitting(String),
    /// A browser was certified with it.
    Spent,
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

pub struct Room {
    pub id: String,
    pub port: u16,
    pub origin: String,
    /// The room uuid pages join (`connectPeer({room})`).
    pub uuid: String,
    /// The room's user root (`connectPeer({userPublicKey})`).
    pub user_public_key: String,
    pub expires_at_ms: u64,
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

fn deadline(after: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(after).unwrap_or(now)
}

/// Reads the relay's stdout up to its [`RoomEvent::Started`] and returns the
/// room's user root. Lines without the protocol prefix (logs) are passed
/// over; any other protocol line first, or another protocol version, is an
/// error.
async fn read_started<R: AsyncRead + Unpin>(lines: &mut Lines<BufReader<R>>) -> Result<String> {
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
                ..
            }) => {
                ensure!(
                    version == room_operator::VERSION,
                    "the room relay speaks operator protocol {version}, this service {}",
                    room_operator::VERSION
                );
                return Ok(user_public_key);
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
            .arg(directory.join("state"))
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
        let user_public_key = read_started(&mut lines).await?;
        let (events, _) = broadcast::channel(64);
        let room = Arc::new(Self {
            id: placement.id,
            port: placement.port,
            origin: placement.origin,
            uuid: uuid_v4(),
            user_public_key,
            expires_at_ms: placement.expires_at_ms,
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
                    return;
                }
            }
        });
        Ok(room)
    }

    /// Resolves once the relay broke the operator protocol; the room must
    /// then be stopped.
    pub async fn broken(&self) {
        self.broken.notified().await;
    }

    /// Takes in one line of the relay's stdout.
    fn observe(&self, line: &str) -> Result<()> {
        match RoomEvent::from_line(line) {
            Err(LineError::NotProtocol) => {}
            Err(error) => return Err(error.into()),
            Ok(RoomEvent::Started { .. }) => bail!("the room relay announced its start twice"),
            Ok(RoomEvent::MemberCertified {
                device,
                label,
                pairing_code,
            }) => {
                self.settle(&pairing_code, TicketState::Spent);
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
                let _ = self.events.send(Event::Member(member));
            }
            Ok(RoomEvent::AdmitRefused {
                pairing_code,
                reason,
            }) => {
                self.settle(&pairing_code, TicketState::Free);
                let _ = self.events.send(Event::Refused {
                    pairing_code,
                    reason,
                });
            }
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
    /// relay answers.
    fn reserve(&self, ticket: &str, pairing_code: &str) -> Result<()> {
        let mut tickets = self
            .tickets
            .lock()
            .map_err(|_| anyhow::anyhow!("tickets poisoned"))?;
        ensure!(
            !tickets
                .iter()
                .any(|entry| entry.state == TicketState::Admitting(pairing_code.to_owned())),
            "this pairing code is already being admitted"
        );
        let entry = tickets
            .iter_mut()
            .find(|entry| entry.secret == ticket)
            .context("this invitation was already used or does not exist")?;
        match entry.state {
            TicketState::Free => entry.state = TicketState::Admitting(pairing_code.to_owned()),
            TicketState::Admitting(_) => bail!("this invitation is already admitting a device"),
            TicketState::Spent => bail!("this invitation was already used or does not exist"),
        }
        drop(tickets);
        Ok(())
    }

    /// Moves the ticket admitting `pairing_code` to `state`.
    fn settle(&self, pairing_code: &str, state: TicketState) {
        let Ok(mut tickets) = self.tickets.lock() else {
            return;
        };
        let admitting = TicketState::Admitting(pairing_code.to_owned());
        match tickets.iter_mut().find(|entry| entry.state == admitting) {
            Some(entry) => entry.state = state,
            None => eprintln!(
                "tilo-room-service: room {}: an answer for a code no ticket is admitting",
                self.id
            ),
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
        let mut events = self.events.subscribe();
        self.reserve(ticket, pairing_code)?;
        let admit = OperatorCommand::Admit {
            pairing_code: pairing_code.to_owned(),
        };
        if let Err(error) = self.tell(&admit).await {
            self.settle(pairing_code, TicketState::Free);
            return Err(error);
        }
        let until = deadline(ADMIT_ANSWER_WAIT);
        loop {
            match tokio::time::timeout_at(until, events.recv()).await {
                Ok(Ok(Event::Member(member))) if member.pairing_code == pairing_code => {
                    return Ok(member);
                }
                Ok(Ok(Event::Refused {
                    pairing_code: refused,
                    reason,
                })) if refused == pairing_code => bail!("{}", reason.message()),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    if let Some(member) = self.certified(pairing_code) {
                        return Ok(member);
                    }
                }
                Ok(Ok(Event::Member(_) | Event::Refused { .. })) => {}
                Ok(Ok(Event::Expired) | Err(broadcast::error::RecvError::Closed)) => {
                    bail!("the room ended")
                }
                Err(_) => bail!("the room did not answer in time"),
            }
        }
    }

    /// The member certified with `pairing_code`, if one was.
    fn certified(&self, pairing_code: &str) -> Option<Member> {
        let history = self.history.lock().ok()?;
        history
            .iter()
            .rev()
            .find(|member| member.pairing_code == pairing_code)
            .cloned()
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// The relay's ends of a room's pipes.
    struct Relay {
        commands: Lines<BufReader<DuplexStream>>,
        events: DuplexStream,
    }

    impl Relay {
        async fn say(&mut self, line: &str) {
            self.events
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }

        async fn report(&mut self, event: &RoomEvent) {
            self.say(&event.to_line()).await;
        }

        async fn command(&mut self) -> OperatorCommand {
            let line = tokio::time::timeout(Duration::from_secs(5), self.commands.next_line())
                .await
                .expect("the service sent no command")
                .unwrap()
                .expect("the service closed stdin");
            OperatorCommand::from_line(&line).unwrap()
        }
    }

    fn started(version: u16, user: &str) -> RoomEvent {
        RoomEvent::Started {
            version,
            origin: "https://127.0.0.1:9700".into(),
            plain_origin: None,
            user_public_key: user.into(),
            certificate_sha256: None,
            allowed_origins: Vec::new(),
        }
    }

    fn placement() -> Placement {
        Placement {
            id: "room".into(),
            port: 9700,
            origin: "https://room.localhost".into(),
            expires_at_ms: u64::MAX,
            directory: PathBuf::from("/nonexistent/tilo-room-test"),
        }
    }

    /// Attaches a room to in-memory pipes after the relay said `first`.
    async fn attach_after(first: &[String]) -> (Result<Arc<Room>>, Relay) {
        let (stdin, relay_stdin) = tokio::io::duplex(64 * 1024);
        let (mut relay_stdout, stdout) = tokio::io::duplex(64 * 1024);
        for line in first {
            relay_stdout
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }
        let room = Room::attach(placement(), Box::new(stdin), stdout).await;
        let relay = Relay {
            commands: BufReader::new(relay_stdin).lines(),
            events: relay_stdout,
        };
        (room, relay)
    }

    async fn running_room() -> (Arc<Room>, Relay) {
        let (room, relay) = attach_after(&[started(room_operator::VERSION, "ab").to_line()]).await;
        (room.unwrap(), relay)
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
