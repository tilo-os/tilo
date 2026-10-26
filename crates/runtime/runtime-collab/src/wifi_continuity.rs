//! The local Settings lane for Wi-Fi continuity: looking up and sharing a
//! network's credentials through the person's sealed secrets, and a
//! profile's local wake setting, each command and reply bounded and checked
//! before it crosses.

use crate::valid_uuid;

/// Maximum encoded size of one local Settings Wi-Fi continuity request.
pub const MAX_WIFI_CONTINUITY_WIRE_BYTES: usize = 4 * 1024;
pub const MAX_WIFI_SSID_BYTES: usize = 256;
pub const MAX_WIFI_PASSWORD_BYTES: usize = 512;

/// Portable Wi-Fi authentication modes understood by the sealed personal
/// secret bridge. Enterprise identity/certificate state is intentionally not
/// representable here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortableWifiSecurity {
    Wpa2,
    Wpa3,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WifiContinuityCommand {
    /// Local wake configuration, never shared as a credential or fleet state.
    NetworkWake {
        profile_uuid: String,
        enabled: Option<bool>,
    },
    Lookup {
        ssid: String,
        security: PortableWifiSecurity,
    },
    Share {
        ssid: String,
        security: PortableWifiSecurity,
        password: String,
    },
    ShareSavedProfile {
        /// Local `NetworkManager` capability; never serialized into fabric state.
        profile_uuid: String,
    },
}

impl std::fmt::Debug for WifiContinuityCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkWake {
                profile_uuid,
                enabled,
            } => formatter
                .debug_struct("NetworkWake")
                .field("profile_uuid", profile_uuid)
                .field("enabled", enabled)
                .finish(),
            Self::Lookup { ssid, security } => formatter
                .debug_struct("Lookup")
                .field("ssid", ssid)
                .field("security", security)
                .finish(),
            Self::Share { ssid, security, .. } => formatter
                .debug_struct("Share")
                .field("ssid", ssid)
                .field("security", security)
                .field("password", &"<redacted>")
                .finish(),
            Self::ShareSavedProfile { profile_uuid } => formatter
                .debug_struct("ShareSavedProfile")
                .field("profile_uuid", profile_uuid)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WifiContinuityReply {
    NetworkWake { status: NetworkWakeStatus },
    Password { password: String },
    NotFound,
    Shared { ssid: String },
    Rejected { reason: String },
}

impl std::fmt::Debug for WifiContinuityReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkWake { status } => formatter
                .debug_struct("NetworkWake")
                .field("status", status)
                .finish(),
            Self::Password { .. } => formatter
                .debug_struct("Password")
                .field("password", &"<redacted>")
                .finish(),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Shared { ssid } => formatter
                .debug_struct("Shared")
                .field("ssid", ssid)
                .finish(),
            Self::Rejected { reason } => formatter
                .debug_struct("Rejected")
                .field("reason", reason)
                .finish(),
        }
    }
}

impl WifiContinuityCommand {
    pub fn valid(&self) -> bool {
        let valid_ssid = |ssid: &str| !ssid.is_empty() && ssid.len() <= MAX_WIFI_SSID_BYTES;
        match self {
            Self::Lookup { ssid, .. } => valid_ssid(ssid),
            Self::Share { ssid, password, .. } => {
                valid_ssid(ssid)
                    && !password.is_empty()
                    && password.len() <= MAX_WIFI_PASSWORD_BYTES
            }
            Self::NetworkWake { profile_uuid, .. } | Self::ShareSavedProfile { profile_uuid } => {
                valid_uuid(profile_uuid)
            }
        }
    }
}

impl WifiContinuityReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::NetworkWake { status } => status.valid(),
            Self::Password { password } => {
                !password.is_empty() && password.len() <= MAX_WIFI_PASSWORD_BYTES
            }
            Self::NotFound => true,
            Self::Shared { ssid } => !ssid.is_empty() && ssid.len() <= MAX_WIFI_SSID_BYTES,
            Self::Rejected { reason } => !reason.is_empty() && reason.len() <= 512,
        }
    }
}

/// Bounded local observations; no field claims verified physical recovery.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkWakeStatus {
    pub profile: String,
    pub interface: Option<String>,
    pub configured: bool,
    pub supported: Option<bool>,
    pub applied: Option<bool>,
    pub problem: Option<String>,
}

impl NetworkWakeStatus {
    fn valid(&self) -> bool {
        valid_uuid(&self.profile)
            && self.interface.as_ref().is_none_or(|name| {
                !name.is_empty()
                    && name.len() <= 15
                    && !name.starts_with('-')
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
            && self
                .problem
                .as_ref()
                .is_none_or(|problem| !problem.is_empty() && problem.len() <= 512)
    }
}
