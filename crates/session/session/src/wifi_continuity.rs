//! Wi-Fi continuity for the person's certified devices: Settings asks here whether this device's
//! interfaces can wake it, and shares the passphrase it learned with the person's other devices —
//! or joins with one they shared — through this session's device fabric. The passphrase is sealed
//! to each of their devices and never travels or stays in the clear.

use zeroize::Zeroizing;

use tilo_action_broker::services::WifiContinuityServices;
use tilo_device_fabric::link::SharedLink;

/// This session's answer to Settings' Wi-Fi continuity commands.
pub(crate) struct WifiContinuity {
    fabric: SharedLink,
}

impl WifiContinuity {
    pub(crate) fn new(fabric: SharedLink) -> Self {
        Self { fabric }
    }
}

impl WifiContinuityServices for WifiContinuity {
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply {
        use runtime_collab::WifiContinuityReply as Reply;
        let security = |security| match security {
            runtime_collab::PortableWifiSecurity::Wpa2 => os_actions::WifiSecurity::Wpa2,
            runtime_collab::PortableWifiSecurity::Wpa3 => os_actions::WifiSecurity::Wpa3,
        };
        match command {
            runtime_collab::WifiContinuityCommand::NetworkWake {
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
                Err(reason) => Reply::Rejected {
                    reason: reason.chars().take(128).collect(),
                },
            },
            runtime_collab::WifiContinuityCommand::Lookup {
                ssid,
                security: mode,
            } => self
                .fabric
                .wifi_password(&ssid, security(mode))
                .map_or(Reply::NotFound, |password| Reply::Password { password }),
            runtime_collab::WifiContinuityCommand::Share {
                ssid,
                security: mode,
                password,
            } => {
                let password = Zeroizing::new(password);
                match self
                    .fabric
                    .share_wifi_password(&ssid, security(mode), &password)
                {
                    Ok(()) => Reply::Shared { ssid },
                    Err(reason) => Reply::Rejected {
                        reason: reason.chars().take(128).collect(),
                    },
                }
            }
            runtime_collab::WifiContinuityCommand::ShareSavedProfile { profile_uuid } => {
                match share_saved_profile(&self.fabric, &profile_uuid) {
                    Ok(ssid) => Reply::Shared { ssid },
                    Err(reason) => Reply::Rejected {
                        reason: reason.chars().take(128).collect(),
                    },
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn network_wake(
    profile_uuid: &str,
    enabled: Option<bool>,
) -> Result<os_actions::network_wake::NetworkWakeStatus, String> {
    match enabled {
        Some(enabled) => os_actions::network_wake::configure(profile_uuid, enabled),
        None => os_actions::network_wake::status(profile_uuid),
    }
}

#[cfg(not(target_os = "linux"))]
fn network_wake(
    _profile_uuid: &str,
    _enabled: Option<bool>,
) -> Result<os_actions::network_wake::NetworkWakeStatus, String> {
    Err("network wake belongs to a Linux session".to_owned())
}

/// Share the passphrase of the profile `profile_uuid` saved on this machine,
/// reading only the parts that can travel between the person's devices.
#[cfg(target_os = "linux")]
fn share_saved_profile(fabric: &SharedLink, profile_uuid: &str) -> Result<String, String> {
    let profile = os_actions::portable_wifi_profile_secret(profile_uuid)?;
    fabric
        .share_wifi_password(&profile.ssid, profile.security, &profile.password)
        .map(|()| profile.ssid)
}

#[cfg(not(target_os = "linux"))]
fn share_saved_profile(_fabric: &SharedLink, _profile_uuid: &str) -> Result<String, String> {
    Err("saved Wi-Fi profiles belong to a Linux session".to_owned())
}
