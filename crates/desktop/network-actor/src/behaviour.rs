//! Iroh endpoint identity helpers for Tilo peer sync.
//!
//! A device's endpoint is its daemon's, under the device key only the daemon
//! keeps. Iroh exposes the key's public half directly as an
//! [`iroh::EndpointId`], so the helpers here read nothing but that.

/// Deterministic 16-bit peer salt from the durable iroh endpoint key
/// ([`tilo_desktop_model::ids::device_salt_of_key`], which the workspace reads
/// too).
pub fn peer_salt_from_keypair(key: &iroh::SecretKey) -> u16 {
    tilo_desktop_model::ids::device_salt_of_key(key.public().as_bytes())
}

/// Fold an authenticated endpoint id to the numeric liveness handle carried
/// by presence messages. Authority decisions retain the complete endpoint id.
pub fn peer_liveness_handle(peer: &iroh::EndpointId) -> u64 {
    neighbour_wire::peer_handle(peer.as_bytes())
}

#[cfg(test)]
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, bytes);
    std::hash::Hasher::finish(&hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_ids_are_folded_from_the_endpoint_key() {
        {
            let key = iroh::SecretKey::from_bytes(&[3; 32]);
            assert_eq!(peer_salt_from_keypair(&key), peer_salt_from_keypair(&key));
            // The salt is the key's FNV-1a hash folded to 16 bits, as every
            // device minted its ids under before the workspace read it too.
            let h = fnv1a_64(key.public().as_bytes());
            let folded = [0, 16, 32, 48].into_iter().fold(0_u16, |folded, shift| {
                folded ^ ((h >> shift) & 0xFFFF) as u16
            });
            assert_eq!(
                peer_salt_from_keypair(&key),
                tilo_desktop_model::ids::device_salt(folded)
            );
            assert_eq!(
                peer_liveness_handle(&key.public()),
                peer_liveness_handle(&key.public())
            );
            assert_ne!(
                peer_liveness_handle(&key.public()),
                peer_liveness_handle(&iroh::SecretKey::from_bytes(&[4; 32]).public())
            );
        }
    }
}
