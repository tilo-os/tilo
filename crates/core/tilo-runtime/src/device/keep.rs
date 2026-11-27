//! What this device keeps beyond its own copies when its machine is a server: the workspaces
//! placed on it, hosted here, with the certification, live origin, kept content, place among
//! its person's servers and rollout that come with keeping them ([`crate::device::keep::server::Server`]),
//! and the packet relay it is reached through.
//!
//! A machine is a server when it has a server configuration ([`CONFIG`], or the file
//! [`CONFIG_ENV`] names). It is signed in as any device is, with a ticket from one of its
//! person's devices (`tilo device add`, then `tilo login <ticket>` or the `tilo.ticket`
//! credential); until then it is no one's and keeps nothing.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "agents")]
pub mod apple_push;
#[cfg(feature = "agents")]
pub mod browser_actions;
pub mod browser_sessions;
pub mod content;
pub mod entry;
#[cfg(feature = "agents")]
pub mod hosted_browser;
pub mod kept_content;
pub mod operations;
mod page_tickets;
pub mod rollout;
pub mod server;
pub mod servers;
pub mod web;
pub mod web_push;

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::device::keep::server::{OnDevice, PacketRelay, Protocols, Server, ServerIdentity};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{EntryMail, EntryPolicy, ListingPolicy, Role};
use tilo_server_protocol::config::{KeptWorkspace, ServerConfig, WebTls};
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::service::{ServiceConfig, WorkspaceService};

/// The server configuration a machine that keeps more has.
pub const CONFIG: &str = "/etc/tilo/server.toml";
/// Names the server configuration in place of [`CONFIG`]: a server in a container, or a test's.
pub const CONFIG_ENV: &str = "TILO_SERVER_CONFIG";

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

/// A workspace the configuration keeps, as its founder founds it: the id every
/// device reaches it by, the name people see it by, and the entry the founder
/// settles once, when it is founded.
#[derive(Debug)]
pub(crate) struct KeptFounding {
    pub(crate) id: tilo_server_protocol::WorkspaceId,
    pub(crate) name: String,
    pub(crate) entry: Option<EntryPolicy>,
}

/// The entry `kept` is founded with, as the deployment names it: whom its entry lets
/// in, at which role, what a directory says of it, and how many requests it keeps.
/// `None` founds a workspace only its members reach: not listed, and joined by
/// invitation.
fn founding_entry(kept: &KeptWorkspace, name: &str) -> Result<Option<EntryPolicy>, String> {
    let text = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let Some(entry) = kept.entry.as_deref().map(str::trim) else {
        if kept.wait_list_max.is_some_and(|kept| kept > 0)
            || text(kept.contact.as_deref()).is_some()
            || text(kept.mail_from.as_deref()).is_some()
            || text(kept.mail_templates.as_deref()).is_some()
        {
            return Err("only an entry keeps a wait list".into());
        }
        return Ok(None);
    };
    let invitation_only = match entry {
        "open" => false,
        "invite" => true,
        other => return Err(format!("its entry is `open` or `invite`, not {other:?}")),
    };
    let default_role = match kept.default_role.as_deref().map(str::trim) {
        None | Some("") => Role::Editor,
        Some(role) => Role::parse(role)
            .filter(|role| matches!(role, Role::Viewer | Role::Commenter | Role::Editor))
            .ok_or_else(|| {
                format!("its default role is `viewer`, `commenter` or `editor`, not {role:?}")
            })?,
    };
    let description = kept
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .unwrap_or(name)
        .to_owned();
    let contact = text(kept.contact.as_deref());
    let mail = text(kept.mail_from.as_deref()).map(|from| EntryMail {
        from,
        templates: text(kept.mail_templates.as_deref()),
    });
    if mail.is_none() && text(kept.mail_templates.as_deref()).is_some() {
        return Err("mail templates need the sender they write for (`mail_from`)".into());
    }
    let policy = EntryPolicy {
        listing: Some(ListingPolicy {
            description,
            default_role,
        }),
        invitation_only,
        wait_list_max: kept.wait_list_max.unwrap_or(0),
        contact,
        mail,
    };
    policy
        .checked()
        .map(Some)
        .map_err(|error| error.to_string())
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
    /// through the packet relay it runs or is given, as a server runs one by default. A
    /// `port` handed to the device takes the address's place.
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
        // A server runs its own packet relay unless its configuration names another (its own
        // at an address, or one in front of it): its pages have no UDP socket, so they reach
        // it only through one, and a machine nobody can dial into needs one the most.
        if self.cfg.packet_relay.is_none() && self.cfg.packet_relay_url.is_none() {
            self.cfg.packet_relay = Some((self.cfg.iroh_listen.ip(), 0).into());
        }
        self.runtime
            .block_on(crate::device::keep::server::bind_keeping(&self.cfg, key))
            .map_err(|error| io::Error::other(format!("this server's endpoint: {error}")))
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

    /// The page this server serves its kept workspaces at, and which workspaces it keeps: what
    /// the apps a person reaches on this device mint their resources' links under. `None` where
    /// this deployment serves no page.
    pub(crate) fn serving(&self) -> Option<(String, Vec<String>)> {
        let web_entry = self.cfg.web_entry.clone()?;
        Some((
            web_entry,
            self.cfg
                .kept
                .iter()
                .map(|kept| kept.id.trim().to_owned())
                .collect(),
        ))
    }

    /// The workspaces this server founds and keeps of its own, as its configuration
    /// lists them: each with the name people see it by and the entry its founder
    /// settles when it is founded.
    ///
    /// # Errors
    /// A listed id or entry setting is not one.
    pub(crate) fn kept(&self) -> io::Result<Vec<KeptFounding>> {
        self.cfg
            .kept
            .iter()
            .map(|kept| {
                let id = tilo_server_protocol::WorkspaceId::parse(kept.id.trim()).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "this server's configuration keeps {:?}, which is not a workspace id",
                            kept.id
                        ),
                    )
                })?;
                let name = if kept.name.trim().is_empty() {
                    kept.id.clone()
                } else {
                    kept.name.clone()
                };
                let entry = founding_entry(kept, &name).map_err(|why| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("this server's configuration keeps {id}: {why}"),
                    )
                })?;
                Ok(KeptFounding { id, name, entry })
            })
            .collect()
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
        crate::device::keep::server::with_persisted_revocations(
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

    /// The certificate a node's page installs, waited for before anything is kept: a machine
    /// the unit points at an install token ([`INSTALL_TOKEN_ENV`]) that holds no certificate
    /// yet creates its device key and serves only `/peer/v1/install` on its web listener until
    /// a root-signed certificate arrives with that token's secret. The session beside it,
    /// whose key the unit names ([`SESSION_DEVICE_ENV`]), is certified as one of the person's
    /// devices in the same install, and the person's user key is written into its identity
    /// tree.
    ///
    /// # Errors
    /// The token or a setting cannot be read, or the listener fails.
    pub(crate) fn enroll(&self) -> io::Result<()> {
        let Some(token_file) = std::env::var_os(INSTALL_TOKEN_ENV) else {
            return Ok(());
        };
        let Some(listen) = self.cfg.web_listen else {
            return Ok(());
        };
        let paths = tilo_device_identity::IdentityPaths::under(&self.cfg.identity_root);
        if paths.device_cert.is_file() {
            return Ok(());
        }
        let token_file = PathBuf::from(token_file);
        let token = std::fs::read_to_string(&token_file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("read {}: {error}", token_file.display()),
            )
        })?;
        // The key this daemon took: the install listener signs with it, and nothing reads it again.
        let device =
            tilo_device_identity::persist::taken_device_key(&paths).map_err(io::Error::other)?;
        println!(
            "{}",
            serde_json::json!({
                "event": "awaiting_certificate",
                "devicePublicKey": hex::encode(device.verifying_key().to_bytes()),
            })
        );
        let session = match std::env::var(SESSION_DEVICE_ENV) {
            Ok(key) => Some(beside_session(&key)?),
            Err(_) => None,
        };
        let config = web_config(&self.cfg, listen);
        self.runtime
            .block_on(crate::device::keep::web::install::await_certificate(
                &config, paths, &token, session,
            ))
            .map_err(|error| {
                io::Error::other(format!("wait for the device certificate: {error}"))
            })?;
        tracing::info!("device certificate installed");
        Ok(())
    }

    /// Keep on `on.service`, kept below `on.dir`: what the server keeps, the protocols the
    /// device's router answers for it, and its own web UI where its configuration places one.
    ///
    /// # Errors
    /// What it keeps does not start, or its web UI does not.
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
            app_serving,
            #[cfg(feature = "agents")]
            browsers,
        } = on;
        // What it keeps is served once its endpoint names where it is reached: a relay it was
        // given that never answered stops it here, named.
        runtime.block_on(tilo_workspace_service::Reaching::ByRelay.awaited(&endpoint));
        relay
            .reached(&endpoint)
            .map_err(|error| io::Error::other(format!("this server's endpoint: {error}")))?;
        let state_root = dir.to_path_buf();
        state_root.clone_into(&mut cfg.state_root);
        #[cfg_attr(
            not(feature = "agents"),
            allow(
                unused_mut,
                reason = "a build that serves Browser wires it into the server"
            )
        )]
        let (mut server, protocols) = runtime
            .block_on(Server::on_device(OnDevice {
                cfg: cfg.clone(),
                identity,
                service,
                keys,
                endpoint,
                joined,
                app_serving,
            }))
            .map_err(|error| io::Error::other(format!("what this server keeps: {error}")))?;
        // The Browser this device places answers its workspaces' devices from here: it was
        // made before their service opened, which answers their app frames with it.
        #[cfg(feature = "agents")]
        if let Some(browsers) = browsers {
            server
                .serve_browsers(browsers, runtime.handle().clone())
                .map_err(|error| io::Error::other(format!("this host's Browser: {error}")))?;
        }
        // A server serves its own web UI where its configuration places one, unless its
        // person turned it off: that choice is its entry's, which the follow applies.
        let web = match cfg.web_listen {
            Some(listen) => runtime
                .block_on(server.serve_web_if_wanted(web_config(&cfg, listen)))
                .map_err(|error| io::Error::other(format!("this server's web UI: {error}")))?,
            None => None,
        };
        // The session beside a node learns where this server's endpoint is from its environment
        // file, so nobody copies the address: the line systemd hands it as TILO_COLLAB_HOST.
        if let Some(path) = std::env::var_os(SESSION_ENV_OUT_ENV) {
            let endpoint = serde_json::to_string(&server.node_addr()).map_err(|error| {
                io::Error::other(format!("encode this server's endpoint: {error}"))
            })?;
            let path = PathBuf::from(path);
            let line = format!("TILO_COLLAB_HOST='{endpoint}'\n");
            tilo_files::native::atomic::write_with_mode(&path, line.as_bytes(), 0o644).map_err(
                |error| io::Error::new(error.kind(), format!("write {}: {error}", path.display())),
            )?;
        }
        tracing::info!("this device keeps the workspaces placed on it, as a server");
        Ok((
            Kept {
                server: Some(server),
                relay: Some(relay),
                web,
                runtime: Some(runtime),
                state_root,
            },
            protocols,
        ))
    }
}

/// Names the token a node's page installs its certificate with: a machine with this set, and
/// no certificate yet, serves only the install route until one arrives.
const INSTALL_TOKEN_ENV: &str = "TILO_SERVER_INSTALL_TOKEN_FILE";
/// Names the session device key a node certifies beside itself, as its unit's session
/// environment file (`TILO_NODE_SESSION_DEVICE`) does.
const SESSION_DEVICE_ENV: &str = "TILO_NODE_SESSION_DEVICE";
/// Names the session's identity tree, where the install writes its certificate and the
/// person's user key.
const SESSION_IDENTITY_ROOT_ENV: &str = "TILO_SERVER_SESSION_IDENTITY_ROOT";
/// Names the environment file the session beside this node reads the server's endpoint from.
const SESSION_ENV_OUT_ENV: &str = "TILO_SERVER_SESSION_ENV_OUT";

/// The session beside a node, as its install certifies it: the device key `key` (hex), and
/// where its certificate and the person's user key are written.
fn beside_session(key: &str) -> io::Result<crate::device::keep::web::install::SessionDevice> {
    let public_key: [u8; 32] = hex::decode(key.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{SESSION_DEVICE_ENV} is not a device key: {key}"),
            )
        })?;
    let root = std::env::var_os(SESSION_IDENTITY_ROOT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{SESSION_IDENTITY_ROOT_ENV} names no identity tree for the session beside \
                     this node"
                ),
            )
        })?;
    let paths = tilo_device_identity::IdentityPaths::under(&root);
    Ok(crate::device::keep::web::install::SessionDevice {
        public_key,
        cert_out: paths.device_cert,
        user_key_out: Some(paths.user_pubkey),
    })
}

/// The browser listener a server serves its own web UI at `listen` with: the certificate
/// its configuration names, or one its person's local CA issues, kept below its state.
fn web_config(cfg: &ServerConfig, listen: SocketAddr) -> crate::device::keep::web::WebConfig {
    use crate::device::keep::web::{TlsSource, WebConfig};
    let tls = match &cfg.web_tls {
        WebTls::Local => TlsSource::Local {
            hosts: local_hosts(listen),
            store: Some(cfg.state_root.join("web-certificate")),
        },
        WebTls::Files { chain, key, host } => TlsSource::Files {
            certificate_chain: chain.clone(),
            private_key: key.clone(),
            host: host.clone(),
        },
    };
    WebConfig {
        listen,
        tls,
        allowed_origins: allowed_origins(
            cfg.web_entry.as_deref(),
            std::env::var(ALLOWED_ORIGINS_ENV).ok().as_deref(),
        ),
        plain_listen: None,
        public_origin: None,
        fetch_proxy: None,
        site: cfg.web_entry.clone(),
    }
}

/// The further browser origins a deployment's pages come from, comma-separated.
const ALLOWED_ORIGINS_ENV: &str = "TILO_SERVER_ALLOWED_ORIGINS";

/// The origins whose pages call this server cross-origin: the site its web entry is served
/// from, whose pages sign in and set up through it, and whatever else the deployment names
/// (a second domain of the same site).
fn allowed_origins(web_entry: Option<&str>, named: Option<&str>) -> Vec<String> {
    let entry = web_entry.and_then(|entry| {
        let (scheme, rest) = entry.split_once("://")?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        (!host.is_empty()).then(|| format!("{scheme}://{host}"))
    });
    let mut origins: Vec<String> = entry.into_iter().collect();
    for origin in named
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
    {
        if !origins.iter().any(|known| known == origin) {
            origins.push(origin.to_owned());
        }
    }
    origins
}

/// The names a server's local certificate answers to: this machine's own name and loopback,
/// and the address it listens on unless that is every interface.
fn local_hosts(listen: SocketAddr) -> Vec<String> {
    let mut hosts = vec!["localhost".to_owned()];
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.is_empty()
        && !hosts.iter().any(|host| host.eq_ignore_ascii_case(&name))
    {
        hosts.push(name);
    }
    if !listen.ip().is_unspecified() {
        let address = listen.ip().to_string();
        if !hosts.contains(&address) {
            hosts.push(address);
        }
    }
    hosts
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
    /// What answers the apps' actions and serves their pages as a browser opens them: this
    /// device's keys' sessions, and the Browser it places for its workspaces' machines
    /// (`crate::device::services`, `crate::device::keep::hosted_browser::app_serving`).
    pub(crate) app_serving: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
    /// The Browser the device places for its workspaces' pages, when it pays for their machines:
    /// made before its workspace service opened, which answers their devices' app frames with
    /// it, and wired by the host it keeps.
    #[cfg(feature = "agents")]
    pub(crate) browsers: Option<Arc<crate::device::keep::hosted_browser::HostedBrowsers>>,
}

impl Kept {
    /// The live session this server keeps: the one the device's live session runs on, so the
    /// relay and its protocol are answered once.
    pub(crate) fn live_session(&self) -> Option<crate::device::live_relay::LiveSession> {
        self.server.as_ref().and_then(Server::live_session)
    }
}

/// What this server keeps, for as long as it runs.
pub(crate) struct Kept {
    server: Option<Server>,
    relay: Option<PacketRelay>,
    /// Its own web UI, when it serves one: dropped, it stops serving.
    web: Option<crate::device::keep::web::WebHandle>,
    runtime: Option<tokio::runtime::Runtime>,
    /// Where what it keeps has its state, as its update turn's leases and the
    /// image it restarted into are kept there.
    state_root: PathBuf,
}

impl Kept {
    /// This server's part in its person's update rollout: the workspaces it
    /// hosts, and where it keeps the image it restarted into. The daemon's
    /// update action takes its turn through it.
    pub(crate) fn machine(&self) -> Option<crate::device::keep::rollout::ServerMachine> {
        let server = self.server.as_ref()?;
        Some(crate::device::keep::rollout::ServerMachine::new(
            server.workspaces()?.clone(),
            self.state_root.clone(),
        ))
    }

    /// This machine's controls ([`crate::device::keep::operations::Controls`]): the workspaces it
    /// hosts, whose administrators may control it, and where its root helper reads what it is
    /// asked and answers.
    pub(crate) fn controls(&self) -> Option<crate::device::keep::operations::Controls> {
        let server = self.server.as_ref()?;
        Some(crate::device::keep::operations::Controls::new(
            server.workspaces()?.clone(),
            self.state_root.clone(),
        ))
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        // The listener first: its tasks run on the runtime that is about to stop.
        drop(self.web.take());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The site a server's web entry names calls it from its pages: its sign-in and setup
    /// screens ask this server for their codes, cross-origin, with nothing else configured.
    #[test]
    fn the_web_entrys_site_calls_its_server_from_the_browser() {
        let cfg = ServerConfig {
            web_entry: Some("http://tilo.localhost:23420/".into()),
            ..ServerConfig::default()
        };
        let web = web_config(&cfg, "127.0.0.1:0".parse().unwrap());
        assert!(
            web.allowed_origins
                .iter()
                .any(|origin| origin == "http://tilo.localhost:23420"),
            "the entry's own site is allowed: {:?}",
            web.allowed_origins
        );
        assert_eq!(
            allowed_origins(
                Some("https://tilo.sh/app/"),
                Some(" https://tilo.so , https://tilo.sh,")
            ),
            ["https://tilo.sh", "https://tilo.so"],
            "a deployment's second domain joins its entry's site, once each"
        );
        assert!(allowed_origins(None, None).is_empty());
    }

    /// The entry a deployment names is the one its founder settles: who the
    /// workspace's entry lets in, at which role, and what a directory says of it —
    /// and a value that is no entry stops the start instead of founding the wrong one.
    #[test]
    fn a_kept_workspaces_entry_is_what_the_deployment_names() {
        let keeping = |kept: KeptWorkspace| Keeping {
            cfg: ServerConfig {
                kept: vec![kept],
                ..ServerConfig::default()
            },
            runtime: tilo_rt::daemon("tilo-keep-test").build().unwrap(),
        };
        let open = keeping(KeptWorkspace {
            id: "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11".into(),
            name: "Lounge".into(),
            entry: Some("open".into()),
            default_role: Some("commenter".into()),
            description: Some("Coffee and tea".into()),
            wait_list_max: None,
            contact: None,
            mail_from: None,
            mail_templates: None,
        })
        .kept()
        .expect("the kept workspace reads");
        assert_open_entry(&open);

        let invite = keeping(KeptWorkspace {
            id: "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11".into(),
            name: "Lounge".into(),
            entry: Some("invite".into()),
            default_role: None,
            description: None,
            wait_list_max: Some(500),
            contact: Some("hello@lounge.example".into()),
            mail_from: Some("invitations@lounge.example".into()),
            mail_templates: None,
        })
        .kept()
        .expect("the kept workspace reads");
        assert_invite_entry(&invite);

        let unnamed = keeping(KeptWorkspace {
            id: "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11".into(),
            name: "Lounge".into(),
            entry: None,
            default_role: None,
            description: None,
            wait_list_max: None,
            contact: None,
            mail_from: None,
            mail_templates: None,
        })
        .kept()
        .expect("the kept workspace reads");
        assert!(
            unnamed[0].entry.is_none(),
            "a deployment that names no entry founds a workspace only its members reach"
        );

        for (entry, told) in [(Some("closed"), "entry"), (Some("open"), "role")] {
            let why = keeping(KeptWorkspace {
                id: "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11".into(),
                name: "Lounge".into(),
                entry: entry.map(str::to_owned),
                default_role: (told == "role").then(|| "owner".to_owned()),
                description: None,
                wait_list_max: None,
                contact: None,
                mail_from: None,
                mail_templates: None,
            })
            .kept()
            .expect_err("a value that is no entry stops the start");
            assert!(
                why.to_string().contains(told),
                "the refusal says which setting: {why}"
            );
        }
    }

    /// An open entry is listed as its deployment names it, and admits anyone who asks.
    fn assert_open_entry(founded: &[KeptFounding]) {
        let entry = founded[0].entry.clone().expect("its entry is founded");
        assert_eq!(
            entry.listing,
            Some(ListingPolicy {
                description: "Coffee and tea".into(),
                default_role: Role::Commenter,
            })
        );
        assert!(
            !entry.invitation_only,
            "an open entry admits anyone who asks"
        );
    }

    /// An invited entry is only for those it invited, with what the deployment names and an
    /// editor's part by default.
    fn assert_invite_entry(founded: &[KeptFounding]) {
        let entry = founded[0].entry.as_ref().expect("its entry is founded");
        assert!(entry.invitation_only, "only people it invited join");
        assert_eq!(entry.wait_list_max, 500);
        assert_eq!(entry.contact.as_deref(), Some("hello@lounge.example"));
        assert_eq!(
            entry.mail,
            Some(EntryMail {
                from: "invitations@lounge.example".into(),
                templates: None,
            })
        );
        assert_eq!(
            entry.listing.as_ref().map(|listing| listing.default_role),
            Some(Role::Editor),
            "a deployment that names no role hands out an editor's part"
        );
        assert_eq!(
            entry
                .listing
                .as_ref()
                .map(|listing| listing.description.as_str()),
            Some("Lounge"),
            "a deployment that names no description is listed by its name"
        );
    }

    /// A machine that is a server runs the packet relay its pages are reached through
    /// without its configuration naming one, and its endpoint names it: a page has no UDP
    /// socket, so a server nobody configured one for would serve its web UI to nobody.
    #[test]
    fn a_server_runs_its_packet_relay_by_default() {
        let state = tempfile::tempdir().unwrap();
        let mut keeping = Keeping {
            cfg: ServerConfig {
                state_root: state.path().to_path_buf(),
                iroh_listen: "127.0.0.1:0".parse().unwrap(),
                ..ServerConfig::default()
            },
            runtime: tilo_rt::daemon("tilo-keep-test").build().unwrap(),
        };
        let key = SigningKey::from_bytes(&[7; 32]);
        let (endpoint, relay) = keeping.bind(&key, None).unwrap();
        (keeping.runtime).block_on(tilo_workspace_service::Reaching::ByRelay.awaited(&endpoint));
        let named: Vec<String> = endpoint
            .addr()
            .relay_urls()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            named.len(),
            1,
            "the endpoint names its packet relay: {named:?}"
        );
        let address = named[0]
            .strip_prefix("http://")
            .expect("a plain packet relay URL")
            .trim_end_matches('/')
            .parse::<SocketAddr>()
            .expect("the relay URL's address");
        let answered = keeping.runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::TcpStream::connect(address),
            )
            .await
        });
        assert!(
            matches!(answered, Ok(Ok(_))),
            "the packet relay answers on {}: {answered:?}",
            named[0]
        );
        keeping.runtime.block_on(endpoint.close());
        keeping.runtime.block_on(relay.shutdown());
    }
}
