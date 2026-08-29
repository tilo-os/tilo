//! `qos-relay` — standalone libp2p gossipsub relay for the qOS
//! collaboration mesh.
//!
//! A long-lived libp2p node that joins the same gossipsub mesh the shells
//! form (so `qos.*` topics fan out without direct shell-to-shell
//! connections), participates in a Kademlia DHT so `NATed` peers can find
//! each other via a bootstrap relay, stays online with a stable
//! [`PeerId`](libp2p::PeerId) (ed25519 keypair on disk, so its
//! `/dns4/.../tcp/4001/p2p/<peerid>` multiaddr doesn't need to rotate),
//! and optionally exposes Prometheus counters over HTTP (`/metrics`).
//!
//! The relay is a forwarding intermediary, not a channel member. Once MLS
//! encryption lands it will see only ciphertext + metadata (topic,
//! message size, timestamp, signing `PeerId`) — CRDT contents stay
//! encrypted to channel members. Today's wire format is plaintext JSON;
//! the relay is structured so the MLS switch is a layer below it and
//! needs no relay-side changes.
//!
//! Crate layout: [`config`] (TOML schema/defaults/loader), [`identity`]
//! (load-or-create ed25519 keypair at `RelayConfig.identity_path`, mode
//! 0600), [`behaviour`] (`NetworkBehaviour` combining gossipsub,
//! kademlia, identify, mdns, ping), [`relay`] (the
//! [`Relay`](relay::Relay) event loop, used by `main.rs` and integration
//! tests), [`log`] (`tracing_subscriber` setup from `log_level`),
//! [`metrics`] (Prometheus counters + a tiny HTTP server).
//!
//! Versioning is pre-`0.1.0`; the public API is the binary. The library
//! exists only so integration tests can spawn a relay in-process.

pub mod behaviour;
pub mod config;
pub mod identity;
pub mod log;
pub mod metrics;
pub mod relay;

pub use config::RelayConfig;
pub use identity::load_or_create_keypair;
pub use relay::Relay;
