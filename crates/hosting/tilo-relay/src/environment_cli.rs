//! `tilo-relay-environment` — a terminal's pty carried to an environment
//! session on a relay.
//!
//! The device's `ush` supervisor runs this on the slave end of the pty it
//! hands the Terminal, for a session whose target is an environment on a
//! relay:
//!
//! ```text
//! tilo-relay-environment --relay <endpoint id> --workspace <id>
//!     (--record <id> (--spec <environment json> | --records-root <dir>)
//!      | --session <hex>)
//!     [--relay-user <hex>] [--identity-root <dir>] [--status-fd <fd>]
//! ```
//!
//! Its stdin must be a terminal; it is put in raw mode, since the guest on
//! the relay has a terminal of its own. Status lines (the session id for
//! re-attaching, tools built on the relay, the end) go to `--status-fd` as
//! JSON, one per line. A session that cannot be reached is reported there as
//! its end, with the reason.
//!
//! With `--records-root`, the record is read where the supervisor keeps
//! environment records, so the supervisor names a record and never builds
//! the wire form itself.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context;
use clap::Parser;
use tilo_relay::environment_bridge::{BridgeEnd, BridgeStatus, run_bridge};
use tilo_relay_client::{RemoteRelay, attach_remote_environment, open_remote_environment};
use tilo_relay_protocol::environment::{EnvironmentRecordRef, EnvironmentSpec, PtySize};
use tilo_user_identity::{IdentityPaths, RevocationList, persist};

#[derive(Parser)]
#[command(
    name = "tilo-relay-environment",
    about = "Carry this terminal to an environment session on a relay"
)]
struct Args {
    /// The relay's endpoint id (hex).
    #[arg(long)]
    relay: String,
    /// The relay's endpoint address (JSON, as its home-peer registration
    /// keeps it); absent, the relay is found by its id.
    #[arg(long)]
    relay_addr: Option<String>,
    /// The relay's user root (hex); absent, any valid host behind the
    /// endpoint is accepted.
    #[arg(long)]
    relay_user: Option<String>,
    /// The workspace whose membership admits this device.
    #[arg(long)]
    workspace: u64,
    /// The environment record to open a session from.
    #[arg(long, conflicts_with = "session")]
    record: Option<String>,
    /// The record's resolved contents, as JSON.
    #[arg(long, conflicts_with = "records_root")]
    spec: Option<String>,
    /// Where the record is kept (the supervisor's environment records),
    /// instead of `--spec`.
    #[arg(long)]
    records_root: Option<PathBuf>,
    /// A session to re-attach to (hex).
    #[arg(long)]
    session: Option<String>,
    /// Where this device's identity lives.
    #[arg(long, default_value = "/var/lib/tilo")]
    identity_root: PathBuf,
    /// A descriptor for JSON status lines.
    #[arg(long)]
    status_fd: Option<RawFd>,
}

/// Puts a terminal in raw mode and restores it on drop.
struct RawMode {
    fd: RawFd,
    saved: libc::termios,
}

impl RawMode {
    fn enter(fd: RawFd) -> std::io::Result<Self> {
        // SAFETY: an all-zero termios is a valid out-parameter.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tcgetattr writes one termios for a descriptor we hold.
        if unsafe { libc::tcgetattr(fd, &raw mut saved) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = saved;
        // SAFETY: cfmakeraw edits the termios in place.
        unsafe { libc::cfmakeraw(&raw mut raw) };
        // SAFETY: tcsetattr reads one initialised termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the termios read in `enter`.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.saved) };
    }
}

fn terminal_size(fd: RawFd) -> PtySize {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one winsize.
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut size) } == 0;
    if ok && size.ws_col > 0 && size.ws_row > 0 {
        PtySize {
            cols: size.ws_col,
            rows: size.ws_row,
        }
    } else {
        PtySize { cols: 80, rows: 24 }
    }
}

fn hex32(value: &str, what: &str) -> anyhow::Result<[u8; 32]> {
    hex::decode(value)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .with_context(|| format!("{what} is not 32 hex bytes"))
}

/// The relay's address: the registered one when given (it must be the
/// relay's own), else its id alone.
fn relay_address(
    raw: Option<&str>,
    relay_id: iroh_base::PublicKey,
) -> anyhow::Result<iroh_base::EndpointAddr> {
    let Some(raw) = raw else {
        return Ok(iroh_base::EndpointAddr::new(relay_id));
    };
    let address: iroh_base::EndpointAddr = serde_json::from_str(raw).context("--relay-addr")?;
    anyhow::ensure!(
        address.id == relay_id,
        "--relay-addr is another endpoint's address"
    );
    Ok(address)
}

/// The record's wire form, read where `records_root` keeps it.
#[cfg(target_os = "linux")]
fn spec_from_records(root: &std::path::Path, record: &str) -> anyhow::Result<EnvironmentSpec> {
    let environment = ush::records::Records::at(root)
        .load(record)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_context(|| format!("there is no environment {record:?}"))?;
    Ok(tilo_relay::environment_bridge::environment_spec(
        &environment,
    ))
}

/// Records are kept by the Linux supervisor; elsewhere the spec is given.
#[cfg(not(target_os = "linux"))]
fn spec_from_records(_root: &std::path::Path, _record: &str) -> anyhow::Result<EnvironmentSpec> {
    anyhow::bail!("--records-root needs the Linux shell runtime; give --spec")
}

/// Open the session the arguments name, or re-attach to it.
async fn reach(
    args: &Args,
    relay: &RemoteRelay,
    size: PtySize,
) -> anyhow::Result<tilo_relay_client::PtyStream> {
    let stream = match (&args.record, &args.spec, &args.records_root, &args.session) {
        (Some(record), spec, root, None) => {
            let spec: EnvironmentSpec = match (spec, root) {
                (Some(spec), None) => serde_json::from_str(spec).context("--spec")?,
                (None, Some(root)) => spec_from_records(root, record)?,
                _ => anyhow::bail!("give --record with --spec or --records-root"),
            };
            let record = EnvironmentRecordRef {
                workspace_id: args.workspace,
                record_id: record.clone(),
            };
            open_remote_environment(relay, &record, &spec, size, std::env::var("TERM").ok()).await
        }
        (None, None, None, Some(session)) => {
            let session = hex::decode(session)
                .ok()
                .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                .context("--session is not 16 hex bytes")?;
            attach_remote_environment(relay, args.workspace, session, size).await
        }
        _ => anyhow::bail!("give --record with --spec or --records-root, or --session"),
    };
    stream.context("reach the environment")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let paths = IdentityPaths::under(&args.identity_root);
    let signing_key = persist::load_device_key(&paths).context("read this device's key")?;
    let cert =
        persist::read_device_cert(&paths.device_cert).context("read this device's certificate")?;
    let relay_id = iroh_base::PublicKey::from_str(&args.relay).context("relay endpoint id")?;
    let relay = RemoteRelay {
        endpoint: iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .context("bind an endpoint")?,
        address: relay_address(args.relay_addr.as_deref(), relay_id)?,
        relay_user: args
            .relay_user
            .as_deref()
            .map(|user| hex32(user, "--relay-user"))
            .transpose()?,
        cert,
        signing_key,
        revocations: RevocationList::new(),
    };

    let mut status_file = args.status_fd.map(|fd| {
        // SAFETY: the caller hands this process the descriptor to own.
        unsafe { std::fs::File::from_raw_fd(fd) }
    });
    let mut status = move |line: BridgeStatus| {
        if let Some(file) = status_file.as_mut()
            && let Ok(mut json) = serde_json::to_vec(&line)
        {
            json.push(b'\n');
            let _ = file.write_all(&json);
        }
    };

    let stdin_fd = std::io::stdin().as_raw_fd();
    let size = terminal_size(stdin_fd);
    let stream = match reach(&args, &relay, size).await {
        Ok(stream) => stream,
        Err(error) => {
            status(BridgeStatus::Ended {
                code: None,
                reason: Some(format!("{error:#}")),
                superseded: false,
            });
            return Err(error);
        }
    };

    let raw = RawMode::enter(stdin_fd).ok();
    let (size_tx, mut sizes) = tokio::sync::mpsc::channel(4);
    let mut window_change =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .context("watch the terminal size")?;
    tokio::spawn(async move {
        while window_change.recv().await.is_some() {
            if size_tx.send(terminal_size(stdin_fd)).await.is_err() {
                return;
            }
        }
    });
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    let reattach_relay = relay.clone();
    let workspace = args.workspace;
    let end = run_bridge(
        stream,
        &mut input,
        &mut output,
        &mut sizes,
        &mut status,
        move |session| {
            let relay = reattach_relay.clone();
            async move {
                attach_remote_environment(&relay, workspace, session, terminal_size(stdin_fd)).await
            }
        },
    )
    .await?;
    drop(raw);
    let (code, reason) = match &end {
        BridgeEnd::Exited(code) => (*code, None),
        BridgeEnd::Superseded => (None, Some("attached on another terminal".to_owned())),
        BridgeEnd::Detached => (None, Some("detached".to_owned())),
        BridgeEnd::Lost(reason) => (None, Some(reason.clone())),
    };
    if let Some(reason) = &reason {
        eprintln!("\r\n[{reason}]\r");
    }
    status(BridgeStatus::Ended {
        code,
        reason,
        superseded: matches!(end, BridgeEnd::Superseded),
    });
    if let BridgeEnd::Exited(Some(code)) = end
        && code != 0
    {
        anyhow::bail!("the environment's shell exited with {code}");
    }
    Ok(())
}
