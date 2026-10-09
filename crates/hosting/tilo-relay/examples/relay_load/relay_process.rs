//! The relay under measurement, in a process of its own.
//!
//! The parent starts this binary again with `--serve` and talks to it over
//! its standard streams, one JSON line each way: the [`Setup`] first (the
//! relay's identity travels through the pipe, never through a file), then a
//! `sample` command per measurement, answered with a [`Sample`]. A closed
//! pipe or `stop` shuts the relay down, so it never outlives the bench.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::{CheckpointPrefixRecovery, RelayConfig, ReplicationIntent, WorkspaceBinding};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::workload::Fleet;

/// Bounds a relay process that does not answer: only a hang reaches it.
const ANSWER: Duration = Duration::from_secs(120);

/// What the relay process needs to start: its identity and its rooms.
#[derive(Serialize, Deserialize)]
pub struct Setup {
    pub state_root: PathBuf,
    pub user_public_key: [u8; 32],
    pub relay_secret_key: [u8; 32],
    pub relay_cert: DeviceCert,
    /// Each room's workspace id and the device keys of its members.
    pub rooms: Vec<(u64, Vec<[u8; 32]>)>,
}

#[derive(Serialize, Deserialize)]
struct Ready {
    addr: EndpointAddr,
}

/// The relay process's counters at one moment.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Sample {
    /// User plus system CPU time of the whole relay process.
    pub cpu_ns: u64,
    /// Resident memory now (Linux); the peak elsewhere.
    pub rss_bytes: u64,
    /// Bytes written to peer sessions ([`tilo_relay::metrics::MetricsSnapshot::bytes_sent`]).
    pub bytes_sent: u64,
    /// Live fan-out part of `bytes_sent`.
    pub bytes_sent_live: u64,
    /// UDP payload bytes the relay's iroh endpoint sent: the frames above
    /// plus QUIC's headers, encryption, acknowledgements and keep-alives.
    pub transport_bytes_sent: u64,
    pub deltas_received: u64,
    pub deltas_fanned_out: u64,
    pub subscriber_dropped: u64,
}

/// The parent's handle on the relay process.
pub struct RelayProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    addr: EndpointAddr,
    device: [u8; 32],
}

impl RelayProcess {
    /// Start the relay process for `fleet`, with its state under `state`.
    pub async fn spawn(fleet: &Fleet, state: &Path) -> anyhow::Result<Self> {
        let setup = fleet.relay_setup(state);
        let device = setup.relay_cert.device_public_key;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("start the relay process")?;
        let mut stdin = child.stdin.take().context("relay stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().context("relay stdout")?).lines();
        let mut line = serde_json::to_vec(&setup)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        let ready: Ready = serde_json::from_str(&answer(&mut stdout).await?)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            addr: ready.addr,
            device,
        })
    }

    pub const fn addr(&self) -> &EndpointAddr {
        &self.addr
    }

    pub const fn device(&self) -> [u8; 32] {
        self.device
    }

    /// The relay's counters now.
    pub async fn sample(&mut self) -> anyhow::Result<Sample> {
        self.stdin.write_all(b"sample\n").await?;
        self.stdin.flush().await?;
        Ok(serde_json::from_str(&answer(&mut self.stdout).await?)?)
    }

    /// Shut the relay down, and kill it if it does not go.
    pub async fn stop(&mut self) {
        let _ = self.stdin.write_all(b"stop\n").await;
        let _ = self.stdin.flush().await;
        if tokio::time::timeout(ANSWER, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

async fn answer(stdout: &mut Lines<BufReader<ChildStdout>>) -> anyhow::Result<String> {
    tokio::time::timeout(ANSWER, stdout.next_line())
        .await
        .context("the relay process did not answer")??
        .context("the relay process exited")
}

/// The `--serve` side: run the relay on the daemon's own runtime profile.
pub fn serve() -> anyhow::Result<()> {
    tilo_rt::daemon("tilo-relay").run(serve_relay())?
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay must keep serving until shutdown().await consumes it"
)]
async fn serve_relay() -> anyhow::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let setup: Setup = serde_json::from_str(&stdin.next_line().await?.context("no setup")?)?;
    let mut cfg = RelayConfig {
        state_root: setup.state_root.clone(),
        ..RelayConfig::default()
    };
    for (id, _) in &setup.rooms {
        cfg.workspaces.push(WorkspaceBinding {
            id: *id,
            pin: true,
            replication: ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: CheckpointPrefixRecovery::Disabled,
        });
    }
    let identity = RelayIdentity {
        user_public_key: setup.user_public_key,
        device_signing_key: ed25519_dalek::SigningKey::from_bytes(&setup.relay_secret_key),
        device_cert: setup.relay_cert,
        revocations: RevocationList::new(),
    };
    // Loopback only: no home-relay connection, whose keep-alives would be
    // counted as the relay's egress.
    let relay = Relay::start_with_app_serving_and_relay_mode(
        cfg,
        identity,
        None,
        iroh::RelayMode::Disabled,
    )
    .await?;
    for (id, members) in setup.rooms {
        relay
            .set_workspace_membership(id, true, members, setup.state_root.clone())
            .await?;
    }
    say(&Ready {
        addr: relay.node_addr(),
    })?;
    while let Some(command) = stdin.next_line().await? {
        if command != "sample" {
            break;
        }
        say(&sample(&relay))?;
    }
    relay.shutdown().await;
    Ok(())
}

fn say(value: &impl Serialize) -> anyhow::Result<()> {
    // Standard output is line-buffered: each answer leaves whole.
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn sample(relay: &Relay) -> Sample {
    let metrics = relay.handle().metrics.snapshot();
    let socket = &relay.endpoint.metrics().socket;
    let (cpu_ns, rss_bytes) = usage();
    Sample {
        cpu_ns,
        rss_bytes,
        bytes_sent: metrics.bytes_sent,
        bytes_sent_live: metrics.bytes_sent_live,
        transport_bytes_sent: socket
            .send_ipv4
            .get()
            .saturating_add(socket.send_ipv6.get())
            .saturating_add(socket.send_relay.get()),
        deltas_received: metrics.deltas_received,
        deltas_fanned_out: metrics.deltas_fanned_out,
        subscriber_dropped: metrics.subscriber_dropped,
    }
}

/// This process's CPU time (ns) and resident memory (bytes).
fn usage() -> (u64, u64) {
    // SAFETY: `rusage` is plain integers, for which all-zero is a value.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `getrusage` writes one `rusage` into the exclusive reference.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } != 0 {
        return (0, 0);
    }
    let nanos = |time: libc::timeval| {
        u64::try_from(time.tv_sec)
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(
                u64::try_from(time.tv_usec)
                    .unwrap_or(0)
                    .saturating_mul(1_000),
            )
    };
    let cpu = nanos(usage.ru_utime).saturating_add(nanos(usage.ru_stime));
    (cpu, resident_bytes().unwrap_or_else(|| peak_bytes(&usage)))
}

/// `VmRSS` from `/proc/self/status`: memory resident now.
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib.saturating_mul(1_024))
}

/// The peak resident set, where the current one cannot be read.
fn peak_bytes(usage: &libc::rusage) -> u64 {
    let peak = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    // macOS reports bytes, Linux KiB.
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak.saturating_mul(1_024)
    }
}
