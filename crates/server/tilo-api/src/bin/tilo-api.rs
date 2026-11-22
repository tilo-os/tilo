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
//! - `TILO_API_HOST`: the Tilo hosts the person's workspaces may live on,
//!   each as the host writes it (`web-host.json`): its certificate, where it
//!   is dialled, and its HTTPS origin, which names it. One object, or a JSON
//!   array of them. The server forwards each call to the host that serves it best and
//!   keeps no workspace of its own ([`tilo_api::hosts`]); it never asks a
//!   host's web origin who it is.

use std::path::PathBuf;
use tilo_api::{Config, Server, store::Store};
use tilo_server_protocol::web::ServerInfo;

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

/// The hosts `name` names ([`tilo_api::hosts::read`]).
fn hosts(name: &str) -> Result<Vec<ServerInfo>, String> {
    tilo_api::hosts::read(&var(name)?).map_err(|error| format!("{name}: {error}"))
}

fn main() -> Result<(), String> {
    // Automerge isolation workers re-enter this binary; serve them first. The
    // sessions this server holds for the person's keys admit their
    // workspaces' governance, which is decided there.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    // With no filter of its own a site keeps warnings, as the server beside it
    // does: among them why an approval was refused.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let listen: std::net::SocketAddr = var("TILO_API_LISTEN")?
        .parse()
        .map_err(|error| format!("TILO_API_LISTEN: {error}"))?;
    let config = Config {
        issuer: origin("TILO_API_ORIGIN")?,
        web_entry: origin("TILO_API_WEB_ENTRY")?,
        hosts: hosts("TILO_API_HOST")?,
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
        tilo_api::serve(server, listener).await;
        Ok(())
    })
}
