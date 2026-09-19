use anyhow::{Context, Result, bail};
use qos_session_peer::SessionOwner;
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
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    install_tracing();
    let mut args = std::env::args_os().skip(1);
    let command = args
        .next()
        .context("usage: qos-session-peer init|status|run|export|import-reviewed STATE [OUTPUT] [MANIFEST_DIGEST]")?;
    let state = PathBuf::from(args.next().context("state directory is required")?);
    let output = args.next().map(PathBuf::from);
    let manifest_digest = args.next();
    if args.next().is_some() {
        bail!("too many arguments");
    }
    match command.to_str() {
        Some("init") if output.is_none() && manifest_digest.is_none() => {
            let owner = SessionOwner::initialize(&state)?;
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
        _ => bail!(
            "usage: qos-session-peer init|status|run|export|import-reviewed STATE [OUTPUT] [MANIFEST_DIGEST]"
        ),
    }
    Ok(())
}
