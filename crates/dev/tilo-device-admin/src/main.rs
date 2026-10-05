use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tilo_device_admin::identity::{self, Identity};
use tilo_device_admin::net::Relays;
use tilo_device_admin::{dial, serve};

/// The device-admin route: tilo-vmd's request protocol between a person's
/// devices over the peer network. Humans and agents use `tilo-vm`, which
/// runs this; `tilo-vm guide devices` explains the route.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the route on this device (run by tilo-device-admin.service).
    Serve {
        #[arg(long, default_value = identity::SYSTEM_ROOT)]
        identity_root: PathBuf,
        /// A relay of your own instead of the public ones (else
        /// `TILO_DEVICE_ADMIN_RELAY`).
        #[arg(long)]
        relay: Option<String>,
        /// The daemon each admitted session talks to.
        #[arg(long, default_value = "tilo-vmd rpc")]
        daemon: String,
        #[arg(long, default_value = tilo_device_admin::SERVICE_STATE)]
        state_file: PathBuf,
    },
    /// Print, as one JSON line, this device's id, its admin endpoint and the
    /// relay to find it on (what `tilo-vm` records for the device).
    Id {
        #[arg(long, default_value = identity::SYSTEM_ROOT)]
        identity_root: PathBuf,
        #[arg(long)]
        relay: Option<String>,
        #[arg(long, default_value = tilo_device_admin::SERVICE_STATE)]
        state_file: PathBuf,
    },
    /// Bridge stdin and stdout to a device's tilo-vmd over the route (what
    /// `tilo-vm` runs when the device is not on this network).
    Dial {
        /// This machine's identity, enrolled for administration.
        #[arg(long)]
        identity_root: PathBuf,
        /// The device's admin endpoint id.
        #[arg(long)]
        endpoint: String,
        /// The device id the answer must prove.
        #[arg(long)]
        device: Option<String>,
        /// The device's own relay, when it uses one (else
        /// `TILO_DEVICE_ADMIN_RELAY`, else the public relays).
        #[arg(long)]
        relay: Option<String>,
        /// The public relay the device was last seen on.
        #[arg(long)]
        home_relay: Option<String>,
        /// Seconds to keep dialing a device that does not answer.
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
    /// Make this machine's device key if it has none, and print it with its
    /// enrolment, as one JSON line.
    Key {
        #[arg(long)]
        identity_root: PathBuf,
    },
    /// Run a relay of your own for the route (plain HTTP: put TLS in front of
    /// it beyond a trusted network), and point both sides at it with
    /// `TILO_DEVICE_ADMIN_RELAY=http://<host>:<port>`.
    Relay {
        #[arg(long, default_value = "0.0.0.0:3340")]
        listen: std::net::SocketAddr,
    },
    /// Install the certificate the person's root issued for this machine's
    /// key (`tilo-enroll certify --capability admin`).
    InstallCert {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        cert: PathBuf,
    },
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("start the async runtime")
}

fn parse_key(text: &str, what: &str) -> anyhow::Result<[u8; 32]> {
    hex::decode(text.trim())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .with_context(|| format!("{what} must be 64 hex digits, got {text:?}"))
}

fn key_json(root: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    let device = identity::ensure_key(root)?;
    let identity = Identity::load(root)?;
    let cert = identity.cert.as_ref();
    Ok(serde_json::json!({
        "device": hex::encode(device),
        "identity": cert.map(|cert| hex::encode(cert.user_public_key)),
        "label": cert.map(|cert| cert.device_label.clone()),
        "administrator": cert.is_some_and(|cert| cert.capabilities.can_administer_devices),
    }))
}

fn id_json(
    root: &std::path::Path,
    relay: Option<&str>,
    state_file: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let identity = Identity::load(root)?;
    let endpoint = identity::endpoint_id(&identity.key);
    // What the running service reports; a stale file of another key is not it.
    let state: Option<serve::State> = std::fs::read(state_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serve::State>(&bytes).ok())
        .filter(|state| state.endpoint == endpoint.to_string());
    let home_relay = state
        .as_ref()
        .and_then(|state| state.addr.relay_urls().next().map(ToString::to_string));
    // The service reads its relay from its unit's environment, which this
    // command does not see: its own report names it.
    let relay = match relay {
        Some(_) => Relays::choose(relay)?.own().map(ToString::to_string),
        None => match state.as_ref() {
            Some(state) => state.relay.clone(),
            None => Relays::choose(None)?.own().map(ToString::to_string),
        },
    };
    let cert = identity.cert.as_ref();
    Ok(serde_json::json!({
        "device": hex::encode(identity.device_id()),
        "endpoint": endpoint.to_string(),
        "label": cert.map(|cert| cert.device_label.clone()),
        "identity": cert.map(|cert| hex::encode(cert.user_public_key)),
        "relay": relay,
        "home_relay": home_relay,
        "serving": state.is_some(),
    }))
}

async fn relay(listen: std::net::SocketAddr) -> anyhow::Result<()> {
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::RelayConfig::new(listen));
    let server = iroh_relay::server::Server::spawn(config)
        .await
        .with_context(|| format!("start the relay on {listen}"))?;
    eprintln!(
        "relaying on http://{}",
        server
            .http_addr()
            .map_or_else(|| listen.to_string(), |addr| addr.to_string())
    );
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    server.shutdown().await.context("stop the relay")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve {
            identity_root,
            relay,
            daemon,
            state_file,
        } => Relays::choose(relay.as_deref()).and_then(|relays| {
            runtime()?.block_on(serve::run(serve::Options {
                identity_root,
                relays,
                daemon: daemon.split_whitespace().map(str::to_owned).collect(),
                state_file: Some(state_file),
            }))
        }),
        Command::Id {
            identity_root,
            relay,
            state_file,
        } => {
            id_json(&identity_root, relay.as_deref(), &state_file).map(|value| println!("{value}"))
        }
        Command::Key { identity_root } => key_json(&identity_root).map(|value| println!("{value}")),
        Command::Relay { listen } => runtime().and_then(|runtime| runtime.block_on(relay(listen))),
        Command::InstallCert {
            identity_root,
            cert,
        } => identity::install_cert(&identity_root, &cert)
            .and_then(|_| key_json(&identity_root).map(|value| println!("{value}"))),
        Command::Dial {
            identity_root,
            endpoint,
            device,
            relay,
            home_relay,
            wait,
        } => {
            return dial_main(
                identity_root,
                &endpoint,
                device.as_deref(),
                relay.as_deref(),
                home_relay.as_deref(),
                wait,
            );
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-device-admin: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn dial_main(
    identity_root: PathBuf,
    endpoint: &str,
    device: Option<&str>,
    relay: Option<&str>,
    home_relay: Option<&str>,
    wait: u64,
) -> ExitCode {
    let options = (|| -> anyhow::Result<dial::Options> {
        Ok(dial::Options {
            identity_root,
            endpoint: endpoint
                .trim()
                .parse()
                .with_context(|| format!("--endpoint {endpoint:?} is not an endpoint id"))?,
            device: device
                .map(|device| parse_key(device, "--device"))
                .transpose()?,
            relays: Relays::choose(relay)?,
            home_relay: home_relay
                .map(|url| {
                    url.parse()
                        .with_context(|| format!("--home-relay {url:?} is not a URL"))
                })
                .transpose()?,
            wait: Duration::from_secs(wait),
        })
    })();
    let outcome = options.map_err(dial::Failure::Other).and_then(|options| {
        runtime()
            .map_err(dial::Failure::Other)?
            .block_on(dial::run(options))
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("tilo-device-admin: {failure}");
            ExitCode::from(failure.exit_code())
        }
    }
}
