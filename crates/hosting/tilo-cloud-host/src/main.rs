use anyhow::{Context, Result, bail};
use std::path::PathBuf;
fn main() -> Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them before any
    // runtime threads start.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}
#[tokio::main]
async fn run() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(
        std::env::var("TILO_CLOUD_STATE").unwrap_or_else(|_| "/var/lib/tilo-cloud".into()),
    );
    let machine = std::env::var("TILO_CLOUD_MACHINE_ID")
        .context("trusted TILO_CLOUD_MACHINE_ID configuration required")?;
    if args.first().is_some_and(|arg| arg == "serve") {
        let https = match &args[1..] {
            [] => None,
            [config_flag, config, ready_flag, ready]
                if config_flag == "--https-config" && ready_flag == "--ready-file" =>
            {
                Some(tilo_cloud_host::runtime::HttpsLaunch {
                    config: PathBuf::from(config),
                    ready_file: PathBuf::from(ready),
                })
            }
            _ => bail!("usage: tilo-cloud-host serve [--https-config PATH --ready-file PATH]"),
        };
        return tilo_cloud_host::runtime::serve(&root, &machine, https).await;
    }
    let output = tilo_cloud_host::control::run(&root, &machine, &args)?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
