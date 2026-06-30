//! Binary entry-point for `qos-relay`.
//!
//! Parses CLI flags, merges with TOML config + env vars, builds the
//! [`Relay`](qos_relay::Relay), and runs the event loop until
//! SIGTERM/SIGINT.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use libp2p::Multiaddr;
use qos_relay::{Relay, RelayConfig};

/// Standalone libp2p gossipsub relay for the qOS collaboration mesh.
///
/// Forwards gossipsub messages on the configured topics (default
/// `qos.*`) and provides Kademlia DHT bootstrap so peers behind NAT
/// can find each other. Trust model: the relay is a forwarding
/// intermediary, not a channel member. Once MLS encryption ships
/// (Phase 7 of the collab spec) it will see only ciphertext +
/// metadata.
#[derive(Debug, Parser)]
#[command(name = "qos-relay", version, about, long_about = None)]
struct Cli {
    /// Path to a TOML config file. If unset, falls back to
    /// `$QOS_RELAY_CONFIG` and finally to the in-binary defaults.
    #[arg(long, short)]
    config: Option<PathBuf>,

    /// Override `listen` from the config. May be passed multiple
    /// times. Each value is a libp2p multiaddr e.g.
    /// `/ip4/0.0.0.0/tcp/4001` or `/ip4/0.0.0.0/udp/4001/quic-v1`.
    #[arg(long)]
    listen: Vec<Multiaddr>,

    /// Override `bootstrap` from the config. May be passed multiple
    /// times. Each value must include a `/p2p/<peerid>` suffix so
    /// the dialled peer can be authenticated.
    #[arg(long)]
    bootstrap: Vec<Multiaddr>,

    /// Override `identity_path`.
    #[arg(long)]
    identity: Option<PathBuf>,

    /// Override `metrics_listen`. Pass e.g. `127.0.0.1:9090` to
    /// expose Prometheus metrics on that address.
    #[arg(long)]
    metrics: Option<SocketAddr>,

    /// Override `log_level`. Accepts `tracing_subscriber::EnvFilter`
    /// directives e.g. `qos_relay=debug,libp2p=info`.
    #[arg(long)]
    log: Option<String>,

    /// Print the effective config (after CLI/env merge) to stdout
    /// and exit without starting the relay. Useful for `diff`-ing
    /// what each layer contributed.
    #[arg(long)]
    print_config: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut config = load_initial_config(cli.config.as_deref())?;
    apply_cli_overrides(&mut config, &cli);

    qos_relay::log::init(&config.log_level);

    if cli.print_config {
        println!("{}", config.to_toml()?);
        return Ok(());
    }

    let relay = Relay::build(config).await?;
    tracing::info!(local_peer_id = %relay.local_peer_id(), "relay built");
    relay.run().await
}

/// Resolve the starting config: file path > env var > defaults.
fn load_initial_config(path: Option<&std::path::Path>) -> anyhow::Result<RelayConfig> {
    if let Some(p) = path {
        return RelayConfig::load(p);
    }
    if let Ok(env_path) = std::env::var("QOS_RELAY_CONFIG") {
        let p = PathBuf::from(env_path);
        return RelayConfig::load(&p);
    }
    Ok(RelayConfig::default())
}

/// Apply CLI overrides on top of the loaded config. CLI always wins.
fn apply_cli_overrides(cfg: &mut RelayConfig, cli: &Cli) {
    if !cli.listen.is_empty() {
        cfg.listen = cli.listen.clone();
    }
    if !cli.bootstrap.is_empty() {
        cfg.bootstrap = cli.bootstrap.clone();
    }
    if let Some(p) = &cli.identity {
        cfg.identity_path = p.clone();
    }
    if let Some(addr) = cli.metrics {
        cfg.metrics_listen = Some(addr);
    }
    if let Some(l) = &cli.log {
        cfg.log_level = l.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_basic_flags() {
        let cli = Cli::parse_from([
            "qos-relay",
            "--listen",
            "/ip4/127.0.0.1/tcp/4001",
            "--log",
            "debug",
        ]);
        assert_eq!(cli.listen.len(), 1);
        assert_eq!(cli.log.as_deref(), Some("debug"));
    }

    #[test]
    fn cli_overrides_apply_in_order() {
        let mut cfg = RelayConfig::default();
        let cli = Cli::parse_from([
            "qos-relay",
            "--listen",
            "/ip4/127.0.0.1/tcp/0",
            "--log",
            "trace",
        ]);
        apply_cli_overrides(&mut cfg, &cli);
        assert_eq!(cfg.listen.len(), 1);
        assert_eq!(cfg.log_level, "trace");
    }

    #[test]
    fn cli_help_renders() {
        // Confirms the clap derive is internally consistent.
        let mut cmd = Cli::command();
        let _ = cmd.render_help();
    }
}
