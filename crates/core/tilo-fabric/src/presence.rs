//! What a device is, and what it says of itself while it is there: its kind and what it does for
//! the person's other devices ([`DeviceAbout`]), how it is woken, whether the person's session on it
//! is locked, and where its screens stand ([`DevicePresence`]).

use serde::{Deserialize, Serialize};

use crate::{
    DisplayConnectorPlacement, DisplayOutputObservation, DisplayRect, FabricError, unique,
};

/// More devices than one person keeps.
pub const MAX_DEVICES: usize = 64;
/// More network interfaces than any one device wakes through.
pub const MAX_WAKE_ROUTES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Desktop,
    Laptop,
    Tablet,
    Phone,
    Television,
    Speaker,
    Server,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "wire capabilities are an independently negotiable set, not mutually exclusive state"
)]
pub struct FabricCapabilities {
    pub display: bool,
    pub pointer: bool,
    pub keyboard: bool,
    pub audio_output: bool,
    pub video_output: bool,
    pub clipboard: bool,
    pub roaming_settings: bool,
    pub secret_store: bool,
    pub app_continuation: bool,
    /// The device can play as a member of a speaker group.
    pub audio_group: bool,
}

/// The kind of network interface a device can be woken through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeInterfaceKind {
    Ethernet,
    WiFi,
}

/// An IPv4 network in canonical form: no host bits set beyond the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ipv4Subnet {
    pub network: [u8; 4],
    pub prefix: u8,
}

impl Ipv4Subnet {
    /// The subnet `address/prefix` lies in, or `None` for a prefix that names
    /// no broadcast domain (wider than /8 or a single host).
    #[must_use]
    pub fn containing(address: std::net::Ipv4Addr, prefix: u8) -> Option<Self> {
        if !(8..=30).contains(&prefix) {
            return None;
        }
        let mask = u32::MAX.checked_shl(32_u32.checked_sub(u32::from(prefix))?)?;
        Some(Self {
            network: (u32::from(address) & mask).to_be_bytes(),
            prefix,
        })
    }

    #[must_use]
    pub fn is_canonical(self) -> bool {
        Self::containing(std::net::Ipv4Addr::from(self.network), self.prefix) == Some(self)
    }

    /// The subnet's directed broadcast address.
    #[must_use]
    pub fn broadcast(self) -> std::net::Ipv4Addr {
        let host = u32::MAX.checked_shr(u32::from(self.prefix)).unwrap_or(0);
        std::net::Ipv4Addr::from(u32::from_be_bytes(self.network) | host)
    }
}

impl std::fmt::Display for Ipv4Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}",
            std::net::Ipv4Addr::from(self.network),
            self.prefix
        )
    }
}

/// One network interface of a device, as the network sees it: what a sibling
/// on the same LAN needs to wake it with a magic packet. Only the person's
/// own devices receive it, inside their certified presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRoute {
    /// The MAC the network sees, which for Wi-Fi is the per-network one.
    pub mac: [u8; 6],
    pub kind: WakeInterfaceKind,
    /// Whether the interface wakes the device on a magic packet.
    pub magic_packet: bool,
    /// The IPv4 subnet the interface was last seen on.
    pub subnet: Option<Ipv4Subnet>,
}

impl WakeRoute {
    #[must_use]
    pub fn valid(&self) -> bool {
        // A unicast, non-zero MAC; a multicast one cannot address a NIC.
        self.mac != [0; 6]
            && self.mac.first().is_some_and(|octet| octet & 1 == 0)
            && self.subnet.is_none_or(Ipv4Subnet::is_canonical)
    }
}

/// Whether the person's session on a device is locked, when they last locked
/// or unlocked it themselves, and when they last used it
/// (the shell's owner session).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSession {
    pub locked: bool,
    /// The person's own lock or unlock. `0` while the device is locked by
    /// its idle timer or by following another device: a lock the others do
    /// not follow.
    pub changed_unix_ms: u64,
    /// Their last input, rounded down to [`OWNER_SESSION_ACTIVE_STEP_MS`] so
    /// typing does not republish the presence.
    pub active_unix_ms: u64,
}

/// How finely [`OwnerSession::active_unix_ms`] follows input.
pub const OWNER_SESSION_ACTIVE_STEP_MS: u64 = 10_000;

/// What a device says it is, in its own part of the person's store: the name the person knows it
/// by, what kind of device it is, and what it does for their other devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAbout {
    pub label: String,
    pub kind: DeviceKind,
    pub capabilities: FabricCapabilities,
}

impl DeviceAbout {
    /// A device of the person's that wrote nothing of itself yet: named as its certificate names
    /// it, and offering nothing.
    #[must_use]
    pub fn named(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: DeviceKind::Desktop,
            capabilities: FabricCapabilities::default(),
        }
    }

    /// # Errors
    /// [`FabricError::InvalidLabel`] for a name no one could read.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.label.len() > 128 || self.label.chars().any(char::is_control) {
            return Err(FabricError::InvalidLabel);
        }
        Ok(())
    }
}

/// Where a device's displays stand and what the hardware behind each says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisplay {
    /// The area its outputs make together.
    pub rect: DisplayRect,
    pub scale_milli: u16,
    /// Where each connector's output stands.
    pub connectors: Vec<DisplayConnectorPlacement>,
    /// What the hardware behind each output says: every placed connector has its active output
    /// here, at the same place and scale.
    pub outputs: Vec<DisplayOutputObservation>,
}

impl DeviceDisplay {
    /// # Errors
    /// [`FabricError::InvalidDisplayPlacement`] when the area, a connector or an output is out of
    /// bounds, or the outputs do not describe the connectors.
    pub fn validate(&self) -> Result<(), FabricError> {
        let invalid = Err(FabricError::InvalidDisplayPlacement);
        if self.rect.width == 0
            || self.rect.height == 0
            || !(500..=4000).contains(&self.scale_milli)
            || self.connectors.len() > 32
            || self.outputs.len() > settings_store::person_displays::MAX_DEVICE_OUTPUTS
        {
            return invalid;
        }
        unique(
            self.connectors
                .iter()
                .map(|output| output.connector.as_str()),
        )?;
        unique(self.outputs.iter().map(|item| item.connector.as_str()))?;
        unique(self.outputs.iter().map(|item| item.output.id.as_str()))?;
        for connector in &self.connectors {
            if !valid_name(&connector.connector)
                || connector.label.len() > 256
                || connector.label.chars().any(char::is_control)
                || connector.rect.logical_placement().is_none()
                || !(500..=4000).contains(&connector.scale_milli)
            {
                return invalid;
            }
        }
        for item in &self.outputs {
            if !valid_name(&item.connector) || !item.output.valid() {
                return invalid;
            }
            let placed = self
                .connectors
                .iter()
                .find(|connector| connector.connector == item.connector);
            let output = &item.output;
            match placed {
                Some(connector) if output.active() => {
                    let rect = DisplayRect {
                        x: output.x,
                        y: output.y,
                        width: output.width,
                        height: output.height,
                    };
                    if connector.rect != rect || connector.scale_milli != output.scale_milli {
                        return invalid;
                    }
                }
                Some(_) => return invalid,
                None if output.active() => return invalid,
                None => {}
            }
        }
        if self.connectors.iter().any(|connector| {
            !self
                .outputs
                .iter()
                .any(|item| item.connector == connector.connector && item.output.active())
        }) {
            return invalid;
        }
        Ok(())
    }
}

/// What a device says of itself while it is there, on its session with each of the person's other
/// devices. Nothing keeps it: once the session ends, the device is gone until it says it again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePresence {
    /// How siblings on the same LAN can wake it.
    pub wake_routes: Vec<WakeRoute>,
    /// The person's session on it, which their other devices' locks follow.
    pub session: OwnerSession,
    /// Its displays, when it offers them.
    pub display: Option<DeviceDisplay>,
}

impl DevicePresence {
    /// # Errors
    /// Why this is not what a device could say of itself.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.wake_routes.len() > MAX_WAKE_ROUTES {
            return Err(FabricError::TooLarge);
        }
        if !self.wake_routes.iter().all(WakeRoute::valid) {
            return Err(FabricError::InvalidWakeRoute);
        }
        unique(self.wake_routes.iter().map(|route| route.mac))?;
        self.display
            .as_ref()
            .map_or(Ok(()), DeviceDisplay::validate)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
