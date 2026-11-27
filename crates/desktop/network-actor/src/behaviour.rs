//! Iroh endpoint identity helpers for Tilo peer sync.
//!
//! A device's endpoint is its daemon's, under the device key only the daemon
//! keeps. Iroh exposes the key's public half directly as an
//! [`iroh::EndpointId`], so the helpers here read nothing but that.

/// Fold an authenticated endpoint id to the numeric liveness handle carried
/// by presence messages. Authority decisions retain the complete endpoint id.
pub fn peer_liveness_handle(peer: &iroh::EndpointId) -> u64 {
    neighbour_wire::peer_handle(peer.as_bytes())
}
