//! Wi-Fi continuity for the person's certified devices: Settings asks here whether this device's
//! interfaces can wake it, and shares the passphrase it learned with the person's other devices,
//! or joins with one they shared. The passphrases are the person's and the device's daemon keeps
//! them in their own workspace ([`tilo_runtime::device::wifi`]); this session reads only what this
//! computer's NetworkManager holds, and hands the daemon a passphrase only sealed to this device.

use ed25519_dalek::SigningKey;
use runtime_collab::{WifiContinuityCommand as Command, WifiContinuityReply as Reply};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_runtime::device::wifi::{LOOKUP, PROVIDER, RESOURCE, SHARE, lookup_arguments, permitted};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_workspace_service::apps::services::WifiContinuityServices;
use tilo_workspace_service::workspaces::person_workspace::wifi::{Credential, Security};

/// This session's answer to Settings' Wi-Fi continuity commands.
pub(crate) struct WifiContinuity {
    daemon: DaemonShell,
    /// The person's own workspace, where the daemon keeps the passphrases.
    workspace: String,
    /// This device's key: what a passphrase crosses the dispatch sealed to.
    key: SigningKey,
}

impl WifiContinuity {
    /// The Wi-Fi continuity of this device's person, through `daemon`: `None` on a computer with
    /// no identity of theirs.
    pub(crate) fn new(daemon: DaemonShell) -> Option<Self> {
        let identity = tilo_session_client::identity::device_identity()?;
        let key = SigningKey::from_bytes(&identity.signer.endpoint_secret()?);
        Some(Self {
            daemon,
            workspace: tilo_server_protocol::servers::person_workspace_id(
                &identity.device_cert.user_public_key,
            ),
            key,
        })
    }

    /// Ask the daemon's Wi-Fi provider `action` with `arguments`, as the person: what it answered.
    fn ask(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let link = self.daemon.link()?;
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let workspace = self.workspace.clone();
        self.daemon.runtime().spawn(async move {
            let _ = answered.send(link.dispatch().settled(Some(workspace), request).await);
        });
        answer
            .recv()
            .map_err(|_| "the daemon's connection stopped before it answered".to_owned())?
    }

    /// The passphrase of `ssid` the person's devices share, opened here.
    fn lookup(&self, ssid: &str, security: Security) -> Result<Option<String>, String> {
        let answer = self.ask(LOOKUP, lookup_arguments(ssid, security))?;
        let Some(credential) = answer["credential"].as_str() else {
            return Ok(None);
        };
        let credential = Credential::from_hex(credential)
            .ok_or("the daemon answered a passphrase that does not read")?;
        credential
            .open(&self.key)
            .map(|password| Some(password.to_string()))
            .map_err(|refused| refused.to_string())
    }

    /// Share `password` of `ssid` with the person's devices: sealed to this device for the daemon,
    /// which seals it to each of theirs.
    fn share(&self, ssid: &str, security: Security, password: &str) -> Result<String, String> {
        let own = self.key.verifying_key().to_bytes();
        let sealed = Credential::seal(
            ssid,
            security,
            password,
            own,
            [own],
            tilo_clock::unix_now_ms(),
        )
        .map_err(|refused| refused.to_string())?;
        let mut arguments = Map::new();
        arguments.insert("credential".to_owned(), json!(sealed.to_hex()));
        let answer = self.ask(SHARE, arguments)?;
        Ok(answer["ssid"].as_str().unwrap_or(ssid).to_owned())
    }
}

/// The security Settings names, as a passphrase is kept for it.
const fn security(security: runtime_collab::PortableWifiSecurity) -> Security {
    match security {
        runtime_collab::PortableWifiSecurity::Wpa2 => Security::Wpa2,
        runtime_collab::PortableWifiSecurity::Wpa3 => Security::Wpa3,
    }
}

/// Settings' words for why it was not done, as short as the wire takes.
fn rejected(reason: &str) -> Reply {
    Reply::Rejected {
        reason: reason.chars().take(128).collect(),
    }
}

impl WifiContinuityServices for WifiContinuity {
    fn command(&self, command: Command) -> Reply {
        match command {
            Command::NetworkWake {
                profile_uuid,
                enabled,
            } => match network_wake(&profile_uuid, enabled) {
                Ok(status) => Reply::NetworkWake {
                    status: runtime_collab::NetworkWakeStatus {
                        profile: status.profile,
                        interface: status.interface,
                        configured: status.configured,
                        supported: status.supported,
                        applied: status.applied,
                        problem: status
                            .problem
                            .map(|problem| problem.chars().take(128).collect()),
                    },
                },
                Err(reason) => rejected(&reason),
            },
            Command::Lookup {
                ssid,
                security: mode,
            } => {
                if permitted().is_err() {
                    return Reply::NotFound;
                }
                match self.lookup(&ssid, security(mode)) {
                    Ok(Some(password)) => Reply::Password { password },
                    Ok(None) => Reply::NotFound,
                    Err(error) => {
                        tracing::warn!(%error, "a shared Wi-Fi passphrase was not looked up");
                        Reply::NotFound
                    }
                }
            }
            Command::Share {
                ssid,
                security: mode,
                password,
            } => {
                let password = zeroize::Zeroizing::new(password);
                match permitted().and_then(|()| self.share(&ssid, security(mode), &password)) {
                    Ok(ssid) => Reply::Shared { ssid },
                    Err(reason) => rejected(&reason),
                }
            }
            Command::ShareSavedProfile { profile_uuid } => {
                match permitted().and_then(|()| share_saved_profile(self, &profile_uuid)) {
                    Ok(ssid) => Reply::Shared { ssid },
                    Err(reason) => rejected(&reason),
                }
            }
        }
    }
}

fn network_wake(
    profile_uuid: &str,
    enabled: Option<bool>,
) -> Result<os_actions::network_wake::NetworkWakeStatus, String> {
    match enabled {
        Some(enabled) => os_actions::network_wake::configure(profile_uuid, enabled),
        None => os_actions::network_wake::status(profile_uuid),
    }
}

/// Share the passphrase of the profile `profile_uuid` saved on this machine,
/// reading only the parts that can travel between the person's devices.
fn share_saved_profile(wifi: &WifiContinuity, profile_uuid: &str) -> Result<String, String> {
    let profile = os_actions::portable_wifi_profile_secret(profile_uuid)?;
    let security = match profile.security {
        os_actions::WifiSecurity::Wpa2 => Security::Wpa2,
        os_actions::WifiSecurity::Wpa3 => Security::Wpa3,
        _ => return Err("only a passphrase network's profile is shared".to_owned()),
    };
    wifi.share(&profile.ssid, security, &profile.password)
}
