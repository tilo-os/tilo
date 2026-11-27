//! The person's devices' fabric, as the daemon's speakers travel reaches it: the envelope one of
//! their devices sends another in, and the check that it is that device's to that one. The model
//! is the desktop network stack's ([`tilo_network_actor::fabric`]), so this program mounts it here
//! rather than the core linking it.

use tilo_network_actor::fabric::{DeviceId, MAX_FABRIC_ACTION_BYTES, PersonalFabricActionEnvelope};
use tilo_runtime::device::capabilities::DeviceFabric;

/// The fabric the desktop's network stack carries, mounted on the daemon's speakers travel.
pub(crate) struct Envelopes;

impl DeviceFabric for Envelopes {
    fn now_us(&self) -> u64 {
        tilo_network_actor::fabric::clock::fabric_monotonic_us()
    }

    fn check(
        &self,
        source: [u8; 32],
        target: [u8; 32],
        now_ms: u64,
        envelope: &[u8],
    ) -> Result<Vec<u8>, String> {
        if envelope.len() > MAX_FABRIC_ACTION_BYTES {
            return Err("an envelope is larger than one link carries".to_owned());
        }
        let device = |key: [u8; 32]| -> Result<DeviceId, String> {
            DeviceId::new(hex::encode(key)).map_err(|_| "that is not a device".to_owned())
        };
        let envelope: PersonalFabricActionEnvelope =
            postcard::from_bytes(envelope).map_err(|error| error.to_string())?;
        envelope
            .validate_at(&device(source)?, &device(target)?, now_ms)
            .map_err(|error| error.to_string())?;
        postcard::to_allocvec(&envelope)
            .map_err(|error| format!("the envelope does not encode: {error}"))
    }
}
