//! `qos-registry-server` CLI — bind a port, add allowlisted
//! publishers, print the identity, or bootstrap a fresh instance.
//!
//! ```text
//! qos-registry-server serve --port 8443 --identity <path>
//! qos-registry-server add-publisher --pubkey <hex> [--can-add-new] [--note ...]
//! qos-registry-server remove-publisher --pubkey <hex>
//! qos-registry-server list-publishers
//! qos-registry-server init --owner-pubkey <hex> --name <n> --description <d> --base-url <url>
//! ```

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

use registry_server::{
    AppState, PublisherAllowlist, PublisherRow, ServerIdentity, Storage, default_data_dir,
    run_server,
};

#[derive(Debug, Parser)]
#[command(name = "qos-registry-server", about = "Self-hostable qOS app registry.")]
struct Cli {
    /// Location of the on-disk state. Defaults to
    /// `$XDG_STATE_HOME/qos-registry` (see `default_data_dir`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Bind an HTTP listener and serve until ctrl-c. TLS is out
    /// of scope — put nginx or Caddy in front for public deployments.
    Serve {
        /// Port to bind to. Bind is on `0.0.0.0` by default; use
        /// `--bind` to restrict.
        #[arg(long, default_value_t = 8443)]
        port: u16,
        /// IPv4/v6 address to bind. Loopback by default so a
        /// misconfigured server can't accidentally serve the
        /// world before `--identity` is set.
        #[arg(long, default_value = "127.0.0.1")]
        bind: IpAddr,
        /// Path to the ed25519 identity file for the registry
        /// owner. Currently informational — the server pins the
        /// pubkey it loads on start into every handshake — but
        /// exposed so future federation flows can sign index
        /// snapshots from the running server.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Bootstrap a fresh data-dir with a `handshake.toml`.
    /// Idempotent — writing over an existing file is not the
    /// intended flow; use `--overwrite` explicitly.
    Init {
        /// Owner Ed25519 pubkey (32-byte hex). Anchors the trust
        /// chain — clients will pin this and refuse to install
        /// entries the current owner didn't allowlist.
        #[arg(long)]
        owner_pubkey: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// One-line description.
        #[arg(long)]
        description: String,
        /// Public base URL — used by clients to compose full
        /// download URLs.
        #[arg(long)]
        base_url: String,
        /// Overwrite an existing handshake.toml instead of erroring.
        #[arg(long)]
        overwrite: bool,
    },
    /// Add or replace a row in `publishers.toml`.
    AddPublisher {
        /// Publisher's Ed25519 pubkey (32-byte hex).
        #[arg(long)]
        pubkey: String,
        /// Allow this publisher to POST new entries; without
        /// this flag they can only edit their own existing entries.
        #[arg(long)]
        can_add_new: bool,
        /// Free-form note visible to the operator only.
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Remove a publisher from the allowlist. Existing entries
    /// they authored stay in place; the moderator can delete them
    /// separately.
    RemovePublisher {
        /// Publisher's Ed25519 pubkey (32-byte hex).
        #[arg(long)]
        pubkey: String,
    },
    /// Print the current `publishers.toml` to stdout.
    ListPublishers,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    let storage = Storage::open(&data_dir).map_err(|e| anyhow::anyhow!("open storage: {e}"))?;

    match cli.cmd {
        Cmd::Serve { port, bind, identity: _ } => {
            let ident: ServerIdentity = storage
                .load_identity()
                .map_err(|e| anyhow::anyhow!("load identity: {e}"))?;
            if ident.owner_pubkey_hex.is_empty() {
                anyhow::bail!(
                    "no handshake.toml at {} — run `qos-registry-server init …` first",
                    storage.handshake_path().display()
                );
            }
            let allow = storage
                .load_allowlist()
                .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
            info!(
                "loaded {} publisher(s) from {}",
                allow.publishers.len(),
                storage.publishers_path().display()
            );
            let state = AppState::new(storage, ident, allow);
            let addr = SocketAddr::new(bind, port);
            run_server(addr, state).await?;
        }
        Cmd::Init {
            owner_pubkey,
            name,
            description,
            base_url,
            overwrite,
        } => {
            if storage.handshake_path().exists() && !overwrite {
                anyhow::bail!(
                    "{} already exists — pass --overwrite to replace",
                    storage.handshake_path().display()
                );
            }
            let ident = ServerIdentity {
                owner_pubkey_hex: owner_pubkey,
                name,
                description,
                base_url,
            };
            storage
                .save_identity(&ident)
                .map_err(|e| anyhow::anyhow!("save identity: {e}"))?;
            info!(
                "wrote handshake to {} — start the server with `qos-registry-server serve`",
                storage.handshake_path().display()
            );
        }
        Cmd::AddPublisher {
            pubkey,
            can_add_new,
            note,
        } => {
            let mut allow: PublisherAllowlist = storage
                .load_allowlist()
                .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
            let replaced = allow.upsert(PublisherRow {
                pubkey_hex: pubkey.clone(),
                can_add_new,
                note,
            });
            storage
                .save_allowlist(&allow)
                .map_err(|e| anyhow::anyhow!("save allowlist: {e}"))?;
            if replaced {
                info!("replaced publisher {pubkey}");
            } else {
                info!("added publisher {pubkey}");
            }
        }
        Cmd::RemovePublisher { pubkey } => {
            let mut allow: PublisherAllowlist = storage
                .load_allowlist()
                .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
            if allow.remove(&pubkey) {
                storage
                    .save_allowlist(&allow)
                    .map_err(|e| anyhow::anyhow!("save allowlist: {e}"))?;
                info!("removed publisher {pubkey}");
            } else {
                anyhow::bail!("no publisher with pubkey {pubkey}");
            }
        }
        Cmd::ListPublishers => {
            let allow: PublisherAllowlist = storage
                .load_allowlist()
                .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
            let s = allow
                .to_toml()
                .map_err(|e| anyhow::anyhow!("encode allowlist: {e}"))?;
            print!("{s}");
        }
    }
    Ok(())
}
