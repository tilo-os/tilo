//! The server under measurement, in a process of its own.
//!
//! The parent starts this binary again with `--serve` and talks to it over
//! its standard streams, one JSON line each way: the [`Setup`] first (the
//! server's identity travels through the pipe, never through a file), then a
//! `sample` command per measurement, answered with a [`Sample`]. A closed
//! pipe or `stop` shuts the server down, so it never outlives the bench.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use tilo_runtime::device::keep::server::{Server, ServerIdentity};
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::workload::Fleet;

/// Bounds a server process that does not answer: only a hang reaches it.
const ANSWER: Duration = Duration::from_secs(120);

/// A member of a workspace: its certificate and its device's secret key, for a member lists
/// itself, signing its own row.
pub type Member = (DeviceCert, [u8; 32]);

/// What the server process needs to start: its identity and its workspaces.
#[derive(Serialize, Deserialize)]
pub struct Setup {
    pub state_root: PathBuf,
    pub user_public_key: [u8; 32],
    pub server_secret_key: [u8; 32],
    pub server_cert: DeviceCert,
    /// Each workspace's id and its members.
    pub workspaces: Vec<(collab_types::WorkspaceId, Vec<Member>)>,
}

#[derive(Serialize, Deserialize)]
struct Ready {
    addr: EndpointAddr,
}

/// The server process's counters at one moment.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Sample {
    /// User plus system CPU time of the whole server process.
    pub cpu_ns: u64,
    /// Resident memory now (Linux); the peak elsewhere.
    pub rss_bytes: u64,
    /// Bytes written to peer sessions ([`tilo_workspace_service::metrics::MetricsSnapshot::bytes_sent`]).
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

/// The parent's handle on the server process.
pub struct ServerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    addr: EndpointAddr,
    device: [u8; 32],
}

impl ServerProcess {
    /// Start the server process for `fleet`, with its state under `state`.
    /// With `cpus`, the server process runs on those CPUs alone.
    pub async fn spawn(
        fleet: &Fleet,
        state: &Path,
        cpus: Option<&[usize]>,
    ) -> anyhow::Result<Self> {
        let setup = fleet.server_setup(state);
        let device = setup.server_cert.device_public_key;
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("--serve");
        if let Some(cpus) = cpus {
            let list: Vec<String> = cpus.iter().map(ToString::to_string).collect();
            command.arg("--cpus").arg(list.join(","));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("start the server process")?;
        let mut stdin = child.stdin.take().context("server stdin")?;
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

    /// The server's counters now.
    pub async fn sample(&mut self) -> anyhow::Result<Sample> {
        self.stdin.write_all(b"sample\n").await?;
        self.stdin.flush().await?;
        Ok(serde_json::from_str(&answer(&mut self.stdout).await?)?)
    }

    /// Shut the server down, and kill it if it does not go.
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
        .context("the server process did not answer")??
        .context("the server process exited")
}

/// The `--serve` side: run the server on the daemon's own runtime profile,
/// on `cpus` alone when the bench names any. The daemon sizes its runtime
/// from the CPUs this process may use, so it starts as on a VM with that many.
pub fn serve(cpus: &[usize]) -> anyhow::Result<()> {
    if !cpus.is_empty() {
        affinity::set(cpus)?;
    }
    tilo_rt::daemon("tilo-server").run(serve_server())?
}

/// Give the server the first `count` CPUs this process may use and keep the
/// rest for this process, which starts its own runtime on them afterwards.
/// Returns the server's CPUs.
pub fn split_cpus(count: usize) -> anyhow::Result<Vec<usize>> {
    let mut cpus = affinity::get()?;
    anyhow::ensure!(
        count >= 1 && count < cpus.len(),
        "--server-cpus {count} must leave the members at least one of the {} CPUs",
        cpus.len()
    );
    let members = cpus.split_off(count);
    affinity::set(&members)?;
    Ok(cpus)
}

/// The calling thread's CPU affinity, which the threads and processes it
/// starts afterwards inherit.
#[cfg(target_os = "linux")]
mod affinity {
    const SET_BYTES: usize = std::mem::size_of::<libc::cpu_set_t>();

    pub fn get() -> anyhow::Result<Vec<usize>> {
        // SAFETY: `cpu_set_t` is a plain bit set, for which all-zero is the empty set.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: writes at most `SET_BYTES` into the exclusive reference.
        if unsafe { libc::sched_getaffinity(0, SET_BYTES, &raw mut set) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let limit = usize::try_from(libc::CPU_SETSIZE)?;
        Ok((0..limit)
            // SAFETY: every index is below `CPU_SETSIZE`, inside the set.
            .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
            .collect())
    }

    pub fn set(cpus: &[usize]) -> anyhow::Result<()> {
        let limit = usize::try_from(libc::CPU_SETSIZE)?;
        // SAFETY: `cpu_set_t` is a plain bit set, for which all-zero is the empty set.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for &cpu in cpus {
            anyhow::ensure!(cpu < limit, "CPU {cpu} is beyond the affinity set");
            // SAFETY: `cpu` is below `CPU_SETSIZE`, inside the set.
            unsafe { libc::CPU_SET(cpu, &mut set) };
        }
        // SAFETY: reads `SET_BYTES` from the shared reference.
        if unsafe { libc::sched_setaffinity(0, SET_BYTES, &raw const set) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod affinity {
    pub fn get() -> anyhow::Result<Vec<usize>> {
        anyhow::bail!("--server-cpus needs Linux's sched_setaffinity")
    }

    pub fn set(_cpus: &[usize]) -> anyhow::Result<()> {
        anyhow::bail!("--server-cpus needs Linux's sched_setaffinity")
    }
}

async fn serve_server() -> anyhow::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let setup: Setup = serde_json::from_str(&stdin.next_line().await?.context("no setup")?)?;
    let cfg = ServerConfig {
        state_root: setup.state_root.clone(),
        ..ServerConfig::default()
    };
    let identity = ServerIdentity {
        user_public_key: setup.user_public_key,
        device_signing_key: ed25519_dalek::SigningKey::from_bytes(&setup.server_secret_key),
        device_cert: setup.server_cert,
        revocations: RevocationList::new(),
    };
    // Loopback only: no home-relay connection, whose keep-alives would be
    // counted as the server's egress.
    let server = Server::start_with_app_serving_and_relay_mode(
        cfg,
        identity,
        None,
        iroh::RelayMode::Disabled,
    )
    .await?;
    // The workspaces are the server owner's: their server makes each, and the
    // load's devices enter it.
    let workspaces = server
        .workspaces()
        .context("the server holds its workspaces")?;
    for (id, members) in setup.workspaces {
        let workspace = id.to_string();
        workspaces
            .create_workspace("Load", Some(&workspace))
            .await
            .map_err(anyhow::Error::msg)?;
        for (member, secret) in &members {
            let key = ed25519_dalek::SigningKey::from_bytes(secret);
            workspaces
                .enter(&workspace, (member, &key))
                .await
                .map_err(anyhow::Error::msg)?;
        }
    }
    say(&Ready {
        addr: server.node_addr(),
    })?;
    while let Some(command) = stdin.next_line().await? {
        if command != "sample" {
            break;
        }
        say(&sample(&server))?;
    }
    server.shutdown().await;
    Ok(())
}

fn say(value: &impl Serialize) -> anyhow::Result<()> {
    // Standard output is line-buffered: each answer leaves whole.
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn sample(server: &Server) -> Sample {
    let metrics = server.carried();
    let socket = &server.endpoint.metrics().socket;
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
