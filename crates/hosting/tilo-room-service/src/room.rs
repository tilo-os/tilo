//! One room: a `tilo-relay room` process on its own state directory, with
//! its own throwaway user root. Browsers join it by device certification;
//! the relay certifies a browser only once this service admitted its
//! pairing code against a single-use ticket. Nothing outlives the room: the
//! directory is removed when it expires or its relay exits.
use crate::config::Config;
use crate::tls::HostIdentity;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, broadcast};

/// What a room announces to its event subscribers.
#[derive(Clone, Debug)]
pub enum Event {
    Member(serde_json::Value),
    Expired,
}

/// A single-use permission to have one browser device certified.
struct Ticket {
    secret: String,
    used: bool,
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
    stdin: Mutex<ChildStdin>,
    events: broadcast::Sender<Event>,
    history: std::sync::Mutex<Vec<serde_json::Value>>,
    tickets: std::sync::Mutex<Vec<Ticket>>,
    directory: PathBuf,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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

/// A field of a JSON line, or null.
fn field<'a>(value: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    value.get(name).unwrap_or(&serde_json::Value::Null)
}

fn deadline(after: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(after).unwrap_or(now)
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
        let mut lines = BufReader::new(child.stdout.take().context("relay stdout")?).lines();
        // The relay's JSON start line says whose root the room is; log lines
        // may come first.
        let started = deadline(Duration::from_secs(60));
        let user_public_key = loop {
            let line = tokio::time::timeout_at(started, lines.next_line())
                .await
                .context("the room relay did not start in time")??
                .context("the room relay exited before it started")?;
            if let Some(user) = serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|value| field(&value, "userPublicKey").as_str().map(str::to_owned))
            {
                break user;
            }
        };
        let (events, _) = broadcast::channel(64);
        let room = Arc::new(Self {
            origin: config.origin(&id, port),
            id,
            port,
            uuid: uuid_v4(),
            user_public_key,
            expires_at_ms: now_ms()
                .saturating_add(u64::try_from(config.ttl.as_millis()).unwrap_or(u64::MAX)),
            stdin: Mutex::new(stdin),
            events,
            history: std::sync::Mutex::default(),
            tickets: std::sync::Mutex::default(),
            directory,
        });
        let reader = Arc::clone(&room);
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if field(&value, "event") == "member_certified" {
                    let member = serde_json::json!({
                        "label": field(&value, "label"), "device": field(&value, "device"),
                        "pairingCode": field(&value, "pairingCode"), "at": now_ms(),
                    });
                    if let Ok(mut history) = reader.history.lock() {
                        history.push(member.clone());
                        drop(history);
                    }
                    let _ = reader.events.send(Event::Member(member));
                }
            }
        });
        Ok((room, child))
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
            used: false,
        });
        drop(tickets);
        Some(secret)
    }

    /// Spends `ticket` on the browser that displays `pairing_code`: tells
    /// the relay to certify it and waits until it did.
    pub async fn admit(&self, ticket: &str, pairing_code: &str) -> Result<serde_json::Value> {
        ensure!(
            !pairing_code.is_empty()
                && pairing_code.len() <= 64
                && pairing_code
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' '),
            "invalid pairing code"
        );
        {
            let mut tickets = self
                .tickets
                .lock()
                .map_err(|_| anyhow::anyhow!("tickets poisoned"))?;
            let entry = tickets
                .iter_mut()
                .find(|entry| entry.secret == ticket && !entry.used)
                .context("this invitation was already used or does not exist")?;
            entry.used = true;
            drop(tickets);
        }
        let mut events = self.events.subscribe();
        self.tell(&format!("admit {pairing_code}\n")).await?;
        let until = deadline(Duration::from_secs(60));
        loop {
            match tokio::time::timeout_at(until, events.recv()).await {
                Ok(Ok(Event::Member(member))) if field(&member, "pairingCode") == pairing_code => {
                    return Ok(member);
                }
                Ok(Ok(Event::Member(_)) | Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Ok(Event::Expired) | Err(broadcast::error::RecvError::Closed)) => {
                    bail!("the room ended")
                }
                Err(_) => bail!("the room did not certify the device in time"),
            }
        }
    }

    /// Writes one operator line to the relay.
    async fn tell(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        drop(stdin);
        Ok(())
    }

    pub fn subscribe(&self) -> (Vec<serde_json::Value>, broadcast::Receiver<Event>) {
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
        let _ = self.tell("shutdown\n").await;
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
