//! On-disk config schema for `tilo-server`.
//!
//! The daemon reads a TOML file — path is passed via `--config` and
//! defaults to `/etc/tilo/server.toml`. The file contains
//! filesystem paths + a listen address + policy. Which workspaces a server
//! holds is decided by the workspaces themselves (their governance), never by
//! this file. Any secrets stay on disk under those paths and are never
//! inlined here.
//!
//! Example:
//!
//! ```toml
//! # /etc/tilo/server.toml
//!
//! # Where the user root public key + this device's keypair + cert live.
//! # Matches `tilo_device_identity::IdentityPaths::default_system()`.
//! identity_root = "/var/lib/tilo"
//!
//! # Where server state (workspace pins, pending deltas) lives.
//! state_root = "/var/lib/tilo-server"
//!
//! # Iroh endpoint bind hint. `"0.0.0.0:0"` lets iroh pick a port
//! # (default) — a fixed port makes firewall rules easier.
//! iroh_listen = "0.0.0.0:0"
//!
//! # Trust policy. See `TrustPolicy` for values.
//! trust_policy = "allow_matching_user"
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::TrustPolicy;
use crate::work::WorkSetting;

/// Top-level server configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Root under which the user + device identity files live.
    /// Passed straight into
    /// [`tilo_device_identity::IdentityPaths::under`].
    #[serde(default = "default_identity_root")]
    pub identity_root: PathBuf,

    /// Root under which the daemon persists workspace state (pinned
    /// automerge documents, per-peer pending-delta queues, metrics
    /// snapshots).
    #[serde(default = "default_state_root")]
    pub state_root: PathBuf,

    /// Iroh endpoint bind hint. `0.0.0.0:0` = OS-chosen port.
    #[serde(default = "default_iroh_listen")]
    pub iroh_listen: SocketAddr,

    /// Where this host runs its own iroh packet relay. A page has no UDP
    /// socket, so it reaches a host only through a relay: the host advertises
    /// this one in the address it signs, and its own transport uses it too.
    /// `None` leaves the host on the public relays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_relay: Option<SocketAddr>,

    /// The origin pages dial the packet relay at when something terminates TLS
    /// in front of it (a deployment's ingress publishing `wss://`): the URL the
    /// host advertises. Without it that is the bind address's own `http://`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_relay_url: Option<String>,

    /// Workspaces this server founds and keeps of its own: a deployment that
    /// serves one names it here, and its server makes it on its first start and
    /// keeps it from then on. A workspace the server already holds is not made
    /// again, and the person's devices reach it as any workspace of theirs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<KeptWorkspace>,

    /// Which peers this relay accepts. Defaults to the matching-
    /// user policy — only peers whose cert chains to the same user
    /// identity as this server's cert.
    #[serde(default)]
    pub trust_policy: TrustPolicy,

    /// The work this device takes from other devices. A host serves a workspace placed on it only
    /// when the setting takes the placing person's work.
    #[serde(default = "default_work")]
    pub work: WorkSetting,

    /// Whether the machine this keeper works on is rented from a cloud provider, whose operator
    /// can read everything on it. A keeper declares it in the address it signs for each
    /// workspace, so every member reads that the provider can read theirs too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rented: bool,

    /// Whether this device answers its workspaces' apps' actions for the devices that ask it to
    /// run them. A device that keeps a workspace serves its apps, and one told not to answers none
    /// of theirs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub app_serving: bool,

    /// Web entry base (ending in `/`) of the operator's versioned browser bundle,
    /// announced in catalogue locators so links are minted under it.
    /// This is a navigation destination, not an admission or content-access grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_entry: Option<String>,
    /// Where this server serves its own web UI over HTTPS: on by default. Its person turns
    /// it off with the `web.serve` action on its entry among their servers, and `None`
    /// serves none in this process, as a deployment that publishes another's pages does.
    #[serde(
        default = "default_web_listen",
        skip_serializing_if = "Option::is_none"
    )]
    pub web_listen: Option<SocketAddr>,
    /// The certificate its web UI is served with: one its person's local CA issues
    /// ([`WebTls::Local`], the default), or a PEM chain and key ([`WebTls::Files`]).
    #[serde(default)]
    pub web_tls: WebTls,
    /// Pre-admissions a workspace keeps, spent ones included. A
    /// public workspace that pre-admits every visitor raises it. `None` keeps
    /// 1024. It keeps the key `max_workspace_grants` in `server.toml`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "max_workspace_grants"
    )]
    pub max_workspace_grants: Option<usize>,

    /// The packet relay this machine enrolls through on its first boot, and
    /// what a device of the person dials it at when it shows a code: a
    /// data-centre server behind a network address translation is reached
    /// through it. `None` leaves it on the public relays every Tilo device
    /// uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,

    /// The key this server signs in to Apple's push service with, and the apps it pushes for:
    /// what tells a person's closed apps on Apple's systems that something waits for them.
    /// `None` tells a closed app nothing, and the server says so once at its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_push: Option<ApplePushKey>,
}

/// The key Apple gave whoever publishes the app, for its push service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplePushKey {
    /// The file the key is in, as Apple hands it out (a `.p8` file). Its owner alone may read
    /// it: the server reads it and never writes, prints or logs it.
    pub key_file: PathBuf,
    /// The key's id, as Apple's developer account lists it.
    pub key_id: String,
    /// The id of the team the key and the apps belong to.
    pub team_id: String,
    /// The apps a push is sent for, by their bundle identifiers.
    pub topics: Vec<String>,
}

/// A workspace a server founds and keeps of its own: the founder's inputs, which
/// the workspace's owner acts on once, when it is founded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptWorkspace {
    /// Its id, which the deployment names so every device reaches the same one.
    pub id: String,
    /// The name people see it by.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Who its entry lets in: `open` admits anyone who asks, at
    /// [`Self::default_role`]; `invite` admits only people the workspace
    /// invited. Absent, the entry admits only its members, and the workspace is
    /// not listed anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The role everyone who joins through the entry holds to begin with:
    /// `viewer`, `commenter` or `editor`. `editor` when the entry gives none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_role: Option<String>,
    /// What the workspace is, in a sentence or two, as the directory shows it.
    /// `name` when the deployment names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How many addresses a `invite` entry keeps for whoever may invite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_list_max: Option<usize>,
    /// Where someone asks to be taken off the wait list, as the entry shows
    /// it (`TILO_WORKSPACES_CONTACT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    /// The sender the wait list mails an address from
    /// (`TILO_WORKSPACES_MAIL_FROM`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail_from: Option<String>,
    /// The directory the wait list's mails' words are read from
    /// (`TILO_WORKSPACES_MAIL_TEMPLATES`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail_templates: Option<String>,
}

fn default_work() -> WorkSetting {
    WorkSetting::own_person()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde names an optional field's default by function, and a deployment serves no web UI by naming none"
)]
fn default_web_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([0, 0, 0, 0], 8443)))
}

fn default_identity_root() -> PathBuf {
    PathBuf::from("/var/lib/tilo")
}

fn default_state_root() -> PathBuf {
    PathBuf::from("/var/lib/tilo-server")
}

fn default_iroh_listen() -> SocketAddr {
    // Constructed directly rather than parsed — `0.0.0.0:0` is
    // infallible either way, but this has no panic path to document.
    SocketAddr::from(([0, 0, 0, 0], 0))
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            identity_root: default_identity_root(),
            state_root: default_state_root(),
            iroh_listen: default_iroh_listen(),
            packet_relay: None,
            packet_relay_url: None,
            trust_policy: TrustPolicy::default(),
            work: default_work(),
            rented: false,
            app_serving: false,
            web_entry: None,
            kept: Vec::new(),
            web_listen: default_web_listen(),
            web_tls: WebTls::default(),
            max_workspace_grants: None,
            relay: None,
            apple_push: None,
        }
    }
}

/// The certificate a server serves its own web UI with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebTls {
    /// The PEM certificate chain and private key a deployment serves, and the name
    /// browsers reach it by.
    Files {
        chain: PathBuf,
        key: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    /// A certificate the person's local CA on this server issues, kept below its state:
    /// what a server with no domain name serves. The person's own devices trust it;
    /// a browser elsewhere does not.
    #[default]
    Local,
}

/// Failure modes for config load/store.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The file was read but TOML parsing failed.
    #[error("toml parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// TOML serialisation failed (shouldn't happen for our own shape).
    #[error("toml encode: {0}")]
    Encode(#[from] toml::ser::Error),
    /// The file names a setting this server no longer has.
    #[error("`{setting}` is no longer a server setting: {why}; remove `{setting}` from the file")]
    Retired {
        /// The setting the file names.
        setting: String,
        /// Why this server has no such setting.
        why: &'static str,
    },
}

/// Settings an older server read, each with why it is gone: a file naming one fails to load and
/// says so.
const RETIRED: [(&str, &str); 3] = [
    (
        "full_replica_status",
        "this server keeps no full copy of a workspace's files, which sync through the workspace",
    ),
    (
        "home_peer_bundle",
        "this server keeps no full copy of a workspace's files, which sync through the workspace",
    ),
    (
        "services",
        "which of a workspace's work this device takes is the work setting",
    ),
];

impl ServerConfig {
    /// A host keeping its state at `state_root`, on its computer's loopback: what a test starts.
    pub fn loopback(state_root: PathBuf) -> Self {
        Self {
            state_root,
            ..Self::default()
        }
        .on_loopback()
    }

    /// This host on its computer's loopback, at the port it asked for: what a test starts.
    #[must_use]
    pub fn on_loopback(self) -> Self {
        let iroh_listen =
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, self.iroh_listen.port()));
        Self {
            iroh_listen,
            ..self
        }
    }

    /// Whether the host asks its router for a port (`UPnP`, PCP, NAT-PMP), which is how
    /// devices reach a host at home without the relay. A host on loopback is reached from no
    /// router, so it asks none, and sends no probe for one.
    pub fn asks_router_for_port(&self) -> bool {
        !self.iroh_listen.ip().is_loopback()
    }

    /// Read a config file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` can't be read, or its contents
    /// don't parse as valid TOML for this schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read_to_string(path)?;
        let table: toml::Table = toml::from_str(&bytes)?;
        if let Some((setting, why)) = RETIRED.iter().find(|(key, _)| table.contains_key(*key)) {
            return Err(ConfigError::Retired {
                setting: (*setting).to_owned(),
                why,
            });
        }
        Ok(table.try_into()?)
    }

    /// Serialise + write the config to disk. Preserves the exact
    /// key ordering the type produces.
    ///
    /// # Errors
    ///
    /// Returns an error if `path`'s parent directory can't be
    /// created, `self` fails to serialise to TOML (shouldn't happen
    /// for this shape), or the write fails.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two paths every hand-written config starts with.
    const PRELUDE: &str =
        "identity_root = \"/var/lib/tilo\"\nstate_root = \"/var/lib/tilo-server\"\n";

    tilo_test_env::scenario! {
        fn server_config_defaults_decodes_and_persists;

        phase server_config_defaults_and_toml_decoding;
        {
            let cfg = ServerConfig::default();
            assert_eq!(cfg.identity_root, PathBuf::from("/var/lib/tilo"));
            assert_eq!(cfg.state_root, PathBuf::from("/var/lib/tilo-server"));
            assert_eq!(cfg.iroh_listen.port(), 0);
            assert!(cfg.asks_router_for_port());
            assert!(!cfg.clone().on_loopback().asks_router_for_port());
            assert_eq!(cfg.work, WorkSetting::own_person());
            assert!(!cfg.app_serving);

            // A minimal file names only the two roots.
            let _: ServerConfig = toml::from_str(PRELUDE).unwrap();

            // An unknown field is rejected rather than ignored.
            assert!(toml::from_str::<ServerConfig>(&format!("{PRELUDE}surprise = 3\n")).is_err());
        }

        phase server_config_edits_are_idempotent_and_persist;
        {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("server.toml");
            let cfg = ServerConfig::default();
            cfg.save(&path).unwrap();
            assert_eq!(cfg, ServerConfig::load(&path).unwrap());

            // A file naming a setting an older server had fails to load, and names it.
            for (retired, line) in [
                ("full_replica_status", "full_replica_status = \"/var/lib/tilo/full-replica/status.json\"\n"),
                ("home_peer_bundle", "home_peer_bundle = \"/etc/tilo/home-peer.toml\"\n"),
            ] {
                std::fs::write(&path, format!("{line}{}", toml::to_string_pretty(&cfg).unwrap()))
                    .unwrap();
                let error = ServerConfig::load(&path).unwrap_err().to_string();
                assert!(error.contains(retired) && error.contains("remove"), "{error}");
            }

            // A file naming the services an older server advertised says which setting replaced
            // them.
            std::fs::write(
                &path,
                format!(
                    "{}[services]\napp_serving = true\n",
                    toml::to_string_pretty(&cfg).unwrap()
                ),
            )
            .unwrap();
            let error = ServerConfig::load(&path).unwrap_err().to_string();
            assert!(error.contains("services") && error.contains("remove"), "{error}");
        }
    }
}
