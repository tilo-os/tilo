//! The Tilo API server.
//!
//! Configured by the environment, so a deployment says everything in one
//! place and the binary holds nothing of it:
//!
//! - `TILO_API_LISTEN`: where to listen, `HOST:PORT` (a TLS-terminating
//!   proxy fronts it).
//! - `TILO_API_ORIGIN`: the server's public origin, as agents reach it.
//! - `TILO_API_WEB_ENTRY`: the person's Tilo on the web, which signs them in.
//! - `TILO_API_STATE`: the file the server keeps clients and grants in.
//! - `TILO_API_HOST`: the public origin of the Tilo host the person's
//!   workspaces live on, which the server enters as each key
//!   ([`tilo_api::relay`]).

use std::path::PathBuf;
use tilo_api::{Config, Server, store::Store};

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn origin(name: &str) -> Result<String, String> {
    let value = var(name)?;
    let parsed = url::Url::parse(&value).map_err(|error| format!("{name}: {error}"))?;
    if !tilo_net::secure_origin(&parsed) {
        return Err(format!("{name} must be https, or http on this machine"));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let listen: std::net::SocketAddr = var("TILO_API_LISTEN")?
        .parse()
        .map_err(|error| format!("TILO_API_LISTEN: {error}"))?;
    let config = Config {
        issuer: origin("TILO_API_ORIGIN")?,
        web_entry: origin("TILO_API_WEB_ENTRY")?,
        relay: origin("TILO_API_HOST")?,
    };
    let store = Store::open(PathBuf::from(var("TILO_API_STATE")?))
        .map_err(|error| format!("TILO_API_STATE: {error}"))?;
    let server = Server::new(&config, store).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|error| format!("{listen}: {error}"))?;
        tracing::info!(%listen, "API server listening");
        // Said once at start, in the log, when the site serves no sign-in page.
        let _ = server.sign_in_page().await;
        tilo_api::serve(server, listener).await;
        Ok(())
    })
}
