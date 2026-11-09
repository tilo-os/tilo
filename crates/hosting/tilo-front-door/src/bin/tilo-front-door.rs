//! Serve the front door.
//!
//! Configured by the environment, so a deployment says everything in one
//! place and the binary holds nothing of it:
//!
//! - `TILO_DOOR_LISTEN`: where to listen, `HOST:PORT` (a TLS-terminating
//!   proxy fronts it).
//! - `TILO_DOOR_ISSUER`: the door's public origin, as agents reach it.
//! - `TILO_DOOR_WEB_ENTRY`: the person's Tilo on the web, which signs them in.
//! - `TILO_DOOR_STATE`: the file the door keeps clients and grants in.
//! - `TILO_DOOR_RELAY`: the public origin of the relay the person's
//!   workspaces live on, which the door enters as each grant
//!   ([`tilo_front_door::relay`]).

use std::path::PathBuf;
use tilo_front_door::{Config, Door, store::Store};

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn origin(name: &str) -> Result<String, String> {
    let value = var(name)?;
    let parsed = url::Url::parse(&value).map_err(|error| format!("{name}: {error}"))?;
    if parsed.scheme() != "https" && parsed.host_str() != Some("localhost") {
        return Err(format!("{name} must be https"));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let listen: std::net::SocketAddr = var("TILO_DOOR_LISTEN")?
        .parse()
        .map_err(|error| format!("TILO_DOOR_LISTEN: {error}"))?;
    let config = Config {
        issuer: origin("TILO_DOOR_ISSUER")?,
        web_entry: origin("TILO_DOOR_WEB_ENTRY")?,
        relay: origin("TILO_DOOR_RELAY")?,
    };
    let store = Store::open(PathBuf::from(var("TILO_DOOR_STATE")?))
        .map_err(|error| format!("TILO_DOOR_STATE: {error}"))?;
    let door = Door::new(&config, store).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|error| format!("{listen}: {error}"))?;
        tracing::info!(%listen, "front door listening");
        tilo_front_door::serve(door, listener).await;
        Ok(())
    })
}
