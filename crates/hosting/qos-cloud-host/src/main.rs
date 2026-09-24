use anyhow::{Context, Result, bail};
use std::{io::Read, path::PathBuf};
#[tokio::main]
async fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "serve-https") {
        if args.len() != 5 || args[1] != "--config" || args[3] != "--ready-file" {
            bail!("usage: qos-cloud-host serve-https --config PATH --ready-file PATH");
        }
        return qos_cloud_host::runtime::serve_https(
            std::path::Path::new(&args[2]),
            std::path::Path::new(&args[4]),
        )
        .await;
    }
    let root = PathBuf::from(
        std::env::var("QOS_CLOUD_STATE").unwrap_or_else(|_| "/var/lib/tilo-cloud".into()),
    );
    let machine = std::env::var("QOS_CLOUD_MACHINE_ID")
        .context("trusted QOS_CLOUD_MACHINE_ID configuration required")?;
    if args == ["serve"] {
        return qos_cloud_host::runtime::serve(&root, &machine).await;
    }
    if args.first().is_some_and(|arg| arg == "agent") {
        if args.len() != 4 || args[1] != "receipt" || args[2] != "--invocation-id" {
            bail!("usage: qos-cloud-host agent receipt --invocation-id UUID");
        }
        let receipt = qos_cloud_host::operator::agent_receipt(&root, &machine, &args[3])?;
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(());
    }
    let mut store = qos_cloud_host::Store::open(&root, &machine)?;
    let output = if args == ["enrollment", "inspect"] {
        serde_json::to_value(store.inspect(qos_relay::clock::now_ns())?)?
    } else if args == ["mesh", "inspect"] {
        serde_json::to_value(store.mesh_inspect(qos_relay::clock::now_ns())?)?
    } else if args == ["mesh", "status"] {
        store.mesh_status(qos_relay::clock::now_ns())?
    } else if args.len() == 4
        && args[0] == "mesh"
        && args[1] == "apply"
        && args[2] == "--request-file"
    {
        let mut bytes = Vec::new();
        std::fs::File::open(&args[3])?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 {
            bail!("mesh request too large")
        }
        store.mesh_apply(&serde_json::from_slice(&bytes)?, qos_relay::clock::now_ns())?
    } else if args.len() == 4
        && args[0] == "enrollment"
        && args[1] == "apply"
        && args[2] == "--request-file"
    {
        let mut bytes = Vec::new();
        std::fs::File::open(&args[3])?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 {
            bail!("enrollment request too large")
        }
        let request = serde_json::from_slice(&bytes)?;
        store.apply(&request, qos_relay::clock::now_ns())?
    } else {
        bail!(
            "usage: qos-cloud-host serve-https --config PATH --ready-file PATH | agent receipt --invocation-id UUID | serve | enrollment inspect | enrollment apply --request-file PATH | mesh inspect | mesh status | mesh apply --request-file PATH"
        )
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
