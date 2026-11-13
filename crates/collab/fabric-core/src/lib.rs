//! Who a device is ([`DeviceId`]), the revision a fact carries ([`FabricStamp`]) and the fresh ids
//! that name one exchange: pure wire types, read alike by the desktop's peer lane and by a page.

use getrandom::SysRng;
use getrandom::rand_core::{Rng as _, UnwrapErr};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};

/// Aggregate state is metadata and small clipboard payloads, never media data.
pub const MAX_FABRIC_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FABRIC_ACTION_BYTES: usize = 64 * 1024;
const MAX_DEVICE_ID_BYTES: usize = 128;

/// A stable, human-identity-certified device identifier.
///
/// The network layer will supply this from an authenticated device key. It is
/// kept textual here so the pure model does not depend on a transport crate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

/// A device id that is empty, too long, carries characters outside
/// `[A-Za-z0-9.-_:]` or looks like a local path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("device id is invalid")]
pub struct InvalidDeviceId;

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDeviceId> {
        let value = value.into();
        if !valid_identifier(&value, MAX_DEVICE_ID_BYTES) {
            return Err(InvalidDeviceId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DeviceId::new(self.0.clone())
            .map_err(S::Error::custom)?
            .0
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DeviceId::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A short machine identifier that can never name a local file: ASCII
/// letters, digits and `.-_:`, at most `max` bytes, and nothing shaped like
/// a path.
pub fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !looks_like_local_path(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

/// Whether `value` reads as a local path on any platform.
pub fn looks_like_local_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("file:")
        || value.starts_with("\\\\")
        || value.as_bytes().get(1) == Some(&b':')
        || value.contains('/')
        || value.contains('\\')
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FabricStamp {
    pub counter: u64,
    pub device: DeviceId,
}

impl FabricStamp {
    pub fn is_newer_than(&self, other: &Self) -> bool {
        (self.counter, self.device.as_str()) > (other.counter, other.device.as_str())
    }
}

/// Fresh bytes from the host's generator, for ids and nonces.
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut value = [0; N];
    UnwrapErr(SysRng).fill_bytes(&mut value);
    value
}

/// `N` random bytes, never all zero: the id and nonce formats reserve zero for "none".
pub fn nonzero<const N: usize>() -> [u8; N] {
    loop {
        let value = bytes::<N>();
        if value != [0; N] {
            return value;
        }
    }
}
