//! Standalone loopback HTTP adapter; terminate public TLS at the host proxy.
#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
#[cfg(unix)]
use tilo_model_gateway::{http_service::HttpService, session_directory::SessionDirectory};
#[cfg(unix)]
#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: SocketAddr,
    /// Owner-only directory containing host-issued, expiring session grants.
    #[arg(long)]
    session_directory: PathBuf,
}
#[cfg(unix)]
fn main() {
    let cli = Cli::parse();
    let result = tilo_rt::daemon("model-http").run(async move {
        if !cli.listen.ip().is_loopback() {
            return Err("HTTP must bind loopback");
        }
        let auth = SessionDirectory::new(cli.session_directory)
            .map_err(|_| "session directory unavailable")?;
        let service =
            HttpService::new(Arc::new(auth), 16).map_err(|_| "model service unavailable")?;
        let listener = tokio::net::TcpListener::bind(cli.listen)
            .await
            .map_err(|_| "listener unavailable")?;
        service
            .serve_loopback(listener, std::future::pending())
            .await
            .map_err(|_| "model service failed")
    });
    if !matches!(result, Ok(Ok(()))) {
        eprintln!("tilo-model-http: service unavailable");
        std::process::exit(1);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("tilo-model-http: Unix session directory authentication is required");
    std::process::exit(1);
}
