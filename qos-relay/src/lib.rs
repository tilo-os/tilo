//! `qos-relay` — standalone libp2p gossipsub relay for the qOS
//! collaboration mesh.
//!
//! Phase 9 of
//! `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`.
//!
//! ## What it is
//!
//! A long-lived libp2p node that:
//!
//! - Joins the same gossipsub mesh the shells form so messages on the
//!   `qos.*` topics fan out across the network without each pair of
//!   shells needing a direct TCP/QUIC connection.
//! - Participates in a Kademlia DHT so peers behind NAT can find each
//!   other given a single bootstrap relay.
//! - Stays online with a stable [`PeerId`](libp2p::PeerId) (the
//!   ed25519 keypair lives on disk) so a community can publish the
//!   relay's `/dns4/.../tcp/4001/p2p/<peerid>` multiaddr in
//!   documentation and not have to rotate it.
//! - Exposes Prometheus counters on an optional HTTP endpoint
//!   (`/metrics`) for monitoring connected peers, forwarded message
//!   counts, and Kademlia query rate.
//!
//! ## Trust model
//!
//! The relay is a forwarding intermediary, not a member of any
//! channel. When MLS encryption lands (Phase 7 of the spec) it will
//! see only ciphertext + metadata: topic name, message size,
//! timestamp, signing PeerId. CRDT contents stay encrypted to the
//! channel members. Today's wire format is plaintext JSON — the
//! relay is structured so the MLS switch is a layer below it and
//! requires no relay-side changes.
//!
//! ## Crate layout
//!
//! - [`config`] — [`RelayConfig`] TOML schema + defaults + loader.
//! - [`identity`] — load_or_create ed25519 keypair persisted at
//!   `RelayConfig.identity_path` with mode 0600.
//! - [`behaviour`] — `derive(NetworkBehaviour)` combining gossipsub,
//!   kademlia, identify, mdns and ping.
//! - [`relay`] — the [`Relay`](relay::Relay) struct and its async
//!   event loop. Public surface used by `main.rs` and by integration
//!   tests in `tests/`.
//! - [`log`] — `tracing_subscriber` setup keyed off the config's
//!   `log_level` field.
//! - [`metrics`] — Prometheus counters + a tiny TCP HTTP server.
//!
//! ## Stability
//!
//! Versioning is pre-`0.1.0`. The public API is the binary; the
//! library exists so the integration tests can spawn a relay
//! in-process. Outside that role, callers are expected to use the
//! built binary, not link against the crate.

pub mod behaviour;
pub mod config;
pub mod identity;
pub mod log;
pub mod metrics;
pub mod relay;

pub use config::RelayConfig;
pub use identity::load_or_create_keypair;
pub use relay::Relay;
