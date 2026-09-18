use anyhow::{Context, Result, bail};
use qos_session_peer::SessionOwner;
use std::path::PathBuf;
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let command = args
        .next()
        .context("usage: qos-session-peer init|status|run|export STATE [OUTPUT]")?;
    let state = PathBuf::from(args.next().context("state directory is required")?);
    let output = args.next().map(PathBuf::from);
    if args.next().is_some() {
        bail!("too many arguments");
    }
    match command.to_str() {
        Some("init") if output.is_none() => {
            let owner = SessionOwner::initialize(&state)?;
            println!("{}", serde_json::to_string(&owner.status())?);
        }
        Some("status") if output.is_none() => {
            let owner = SessionOwner::open(&state)?;
            println!("{}", serde_json::to_string(&owner.status())?);
        }
        Some("run") => {
            let ready = output.context("run requires ready-file path")?;
            qos_session_peer::host::run(SessionOwner::open(&state)?, &ready).await?;
        }
        Some("export") => {
            SessionOwner::open(&state)?.export(&output.context("export requires output path")?)?;
        }
        _ => bail!("usage: qos-session-peer init|status|run|export STATE [OUTPUT]"),
    }
    Ok(())
}
