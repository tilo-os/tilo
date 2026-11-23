//! What this device keeps beyond its own copies when its machine is a server: the workspaces
//! placed on it, governed here, with the certification, live origin, kept content, place among
//! its person's servers and rollout that come with keeping them ([`tilo_server::daemon::Server`]),
//! and the packet relay it is reached through.
//!
//! A machine is a server when it has a server configuration ([`CONFIG`], or the file
//! [`CONFIG_ENV`] names). It is signed in as any device is, with a ticket from one of its
//! person's devices (`tilo device add`, then `tilo login <ticket>` or the `tilo.ticket`
//! credential); until then it is no one's and keeps nothing.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_server::daemon::{OnDevice, PacketRelay, Protocols, Server, ServerIdentity};
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::service::{ServiceConfig, WorkspaceService};

/// The server configuration a machine that keeps more has.
pub const CONFIG: &str = "/etc/tilo/server.toml";
/// Names the server configuration in place of [`CONFIG`]: a server in a container, or a test's.
pub const CONFIG_ENV: &str = "TILO_SERVER_CONFIG";
/// What a server that no ticket signed in says.
pub const SIGN_IN_FIRST: &str = "This server is not signed in: run `tilo device add` on one of \
     your devices and sign it in with the ticket it prints (`tilo login <ticket>`).";

/// Where this machine's server configuration is, when it is a server.
pub(crate) fn config() -> Option<PathBuf> {
    std::env::var_os(CONFIG_ENV)
        .map(PathBuf::from)
        .or_else(|| Path::new(CONFIG).exists().then(|| PathBuf::from(CONFIG)))
}

/// A machine's server configuration, and the runtime what it keeps runs on: the many
/// connections a server answers are not left to the one thread the device's apps share.
pub(crate) struct Keeping {
    cfg: ServerConfig,
    runtime: tokio::runtime::Runtime,
}

impl Keeping {
    /// The server configuration this machine has, if it has one.
    ///
    /// # Errors
    /// The configuration cannot be read, or its runtime does not start.
    pub(crate) fn configured() -> io::Result<Option<Self>> {
        let Some(path) = config() else {
            return Ok(None);
        };
        let cfg = ServerConfig::load(&path).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("this server's configuration {}: {error}", path.display()),
            )
        })?;
        let runtime = tilo_rt::daemon("tilo-keep").build()?;
        Ok(Some(Self { cfg, runtime }))
    }

    /// This device's one endpoint, as the configuration places it: on its address, reached
    /// through the packet relay it runs or is given. A `port` handed to the device takes the
    /// address's place.
    ///
    /// # Errors
    /// The relay does not start, or the endpoint does not bind.
    pub(crate) fn bind(
        &mut self,
        key: &SigningKey,
        port: Option<u16>,
    ) -> io::Result<(iroh::Endpoint, PacketRelay)> {
        if let Some(port) = port {
            self.cfg.iroh_listen.set_port(port);
        }
        self.runtime
            .block_on(tilo_server::daemon::bind_keeping(&self.cfg, key))
            .map_err(|error| io::Error::other(format!("this server's endpoint: {error}")))
    }

    /// Whether this server governs the workspaces placed on it, as the device `cert` names.
    pub(crate) fn governs(&self, cert: &DeviceCert) -> bool {
        Server::governs(&self.cfg, cert)
    }

    /// The device's workspace service configured to keep the workspaces placed on it, as the
    /// work this server takes says.
    pub(crate) fn configuring(&self) -> impl FnOnce(ServiceConfig) -> ServiceConfig + Send + use<> {
        let cfg = self.cfg.clone();
        move |config| {
            let apps = config.apps.clone();
            Server::keeping(&cfg, config, apps)
        }
    }

    /// The device `key` its person certified with `cert`, as its workspace service acts on a
    /// server: as the device itself, which its people's devices address, with the revocations
    /// it keeps below the device's directory `dir`.
    ///
    /// # Errors
    /// The server's revocations cannot be read.
    pub(crate) fn itself(
        key: &SigningKey,
        cert: &DeviceCert,
        dir: &Path,
    ) -> io::Result<ServerIdentity> {
        tilo_server::daemon::with_persisted_revocations(
            ServerIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: RevocationList::new(),
            },
            dir,
        )
        .map_err(|error| io::Error::other(format!("this server's revocations: {error:#}")))
    }

    /// Keep on `on.service`, kept below `on.dir`: what the server keeps, and the protocols the
    /// device's router answers for it.
    ///
    /// # Errors
    /// What it keeps does not start.
    pub(crate) fn keep(self, on: On<'_>) -> io::Result<(Kept, Protocols)> {
        let Self { mut cfg, runtime } = self;
        let On {
            dir,
            joined,
            identity,
            service,
            keys,
            endpoint,
            relay,
        } = on;
        cfg.state_root = dir.to_path_buf();
        let (server, protocols) = runtime
            .block_on(Server::on_device(OnDevice {
                cfg,
                identity,
                service,
                keys,
                endpoint,
                joined,
                app_serving: None,
            }))
            .map_err(|error| io::Error::other(format!("what this server keeps: {error}")))?;
        tracing::info!("this device keeps the workspaces placed on it, as a server");
        Ok((
            Kept {
                server: Some(server),
                relay: Some(relay),
                runtime: Some(runtime),
            },
            protocols,
        ))
    }
}

/// What a server keeps on: the device and its workspace service.
pub(crate) struct On<'a> {
    /// The device's directory, where its workspace service is kept.
    pub(crate) dir: &'a Path,
    /// Whether a ticket signed the device in.
    pub(crate) joined: bool,
    /// The device, as [`Keeping::itself`] names it.
    pub(crate) identity: ServerIdentity,
    pub(crate) service: Arc<WorkspaceService>,
    pub(crate) keys: tilo_workspace_keys::KeyProvider,
    /// The device's one endpoint, which [`Keeping::bind`] bound.
    pub(crate) endpoint: iroh::Endpoint,
    /// The packet relay the device's endpoint is reached through.
    pub(crate) relay: PacketRelay,
}

/// What this server keeps, for as long as it runs.
pub(crate) struct Kept {
    server: Option<Server>,
    relay: Option<PacketRelay>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for Kept {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        // Dropped on a runtime's thread, nothing here may wait: what it keeps stops with its
        // runtime.
        if tokio::runtime::Handle::try_current().is_ok() {
            runtime.shutdown_background();
            return;
        }
        if let Some(server) = self.server.take() {
            runtime.block_on(server.shutdown());
        }
        if let Some(relay) = self.relay.take() {
            runtime.block_on(relay.shutdown());
        }
        runtime.shutdown_background();
    }
}
