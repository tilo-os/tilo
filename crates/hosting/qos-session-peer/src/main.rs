use anyhow::Context;
use qos_session_peer::SessionOwner;
use qos_session_peer::owner::sender_administration::{AdminStatus, CliResult as Result};
use std::io::Write;
use std::path::PathBuf;
/// Diagnostics stay off unless `RUST_LOG` is set: the fixture's stdout carries
/// status JSON, so tracing goes to stderr and never changes the protocol.
fn install_tracing() {
    if std::env::var_os("RUST_LOG").is_none() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}
/// The relay's inbound state path is one deep future chain, so a single poll
/// can exceed Tokio's default worker stack in an unoptimized build.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;
const USAGE: &str = "usage: qos-session-peer init|init-workspace|init-managed|init-managed-workspace|status|run|export|import-reviewed STATE [OUTPUT] [MANIFEST_DIGEST]; sender-storage select|resume|status STATE";

/// Keep enough workers that checkpoint serialization cannot occupy the same
/// two threads that must continue driving the relay transport.
fn worker_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |count| count.get().clamp(4, 8))
}

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads())
        .thread_stack_size(WORKER_STACK_BYTES)
        .thread_name("qos-session-peer")
        .enable_all()
        .build()
        .context("building the session peer runtime")?
        .block_on(run())
}

async fn run() -> Result<()> {
    install_tracing();
    let mut args = std::env::args_os().skip(1);
    let command = args.next().context(USAGE)?;
    if command == "sender-storage" {
        let operation = args
            .next()
            .context("sender-storage requires select|resume|status")?;
        let state = PathBuf::from(args.next().context("state directory is required")?);
        if args.next().is_some() {
            return Err(anyhow::anyhow!("too many sender-storage arguments").into());
        }
        use qos_session_peer::owner::sender_administration as admin;
        let status = match operation.to_str() {
            Some("select") => admin::select(&state)?,
            Some("resume") => admin::resume(&state)?,
            Some("status") => admin::status(&state)?,
            _ => {
                return Err(anyhow::anyhow!("sender-storage requires select|resume|status").into());
            }
        };
        print_admin_status(&status)?;
        return Ok(());
    }
    let state = PathBuf::from(args.next().context("state directory is required")?);
    let output = args.next().map(PathBuf::from);
    let manifest_digest = args.next();
    if args.next().is_some() {
        return Err(anyhow::anyhow!("too many arguments").into());
    }
    match command.to_str() {
        Some("init-managed") if output.is_none() && manifest_digest.is_none() => {
            let status = qos_session_peer::owner::sender_administration::initialize(&state)?;
            print_admin_status(&status)?;
        }
        Some("init-managed-workspace") if manifest_digest.is_none() => {
            let name = output.context("init-managed-workspace requires a workspace name")?;
            let name = name.to_str().context("workspace name must be UTF-8")?;
            let status =
                qos_session_peer::owner::sender_administration::initialize_workspace(&state, name)?;
            print_admin_status(&status)?;
        }
        Some("init") if output.is_none() && manifest_digest.is_none() => {
            let owner = SessionOwner::initialize(&state)?;
            println!("{}", serde_json::to_string(&owner.status())?);
        }
        Some("init-workspace") if manifest_digest.is_none() => {
            let name = output.context("init-workspace requires a workspace name")?;
            let name = name.to_str().context("workspace name must be UTF-8")?;
            let owner = SessionOwner::initialize_workspace(&state, name)?;
            println!("{}", serde_json::to_string(&owner.status())?);
        }
        Some("status") if output.is_none() && manifest_digest.is_none() => {
            let owner = SessionOwner::open(&state)?;
            println!("{}", serde_json::to_string(&owner.status())?);
        }
        Some("run") if manifest_digest.is_none() => {
            let ready = output.context("run requires ready-file path")?;
            qos_session_peer::host::run(SessionOwner::open(&state)?, &ready).await?;
        }
        Some("export") if manifest_digest.is_none() => {
            SessionOwner::open(&state)?.export(&output.context("export requires output path")?)?;
        }
        Some("import-reviewed") => {
            let archive = output.context("import-reviewed requires archive directory")?;
            let digest = manifest_digest
                .context("import-reviewed requires independently reviewed manifest BLAKE3")?;
            let digest = digest
                .to_str()
                .context("manifest digest must be UTF-8 hexadecimal")?;
            let receipt = qos_session_peer::recovery::import_verified_archive(
                &mut SessionOwner::open(&state)?,
                &archive,
                digest,
            )?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        _ => return Err(anyhow::anyhow!(USAGE).into()),
    }
    Ok(())
}

/// Inert administrative output is streamed; no new JSON String is allocated.
fn print_admin_status(status: &AdminStatus) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, status)?;
    output.write_all(b"\n")?;
    Ok(())
}
