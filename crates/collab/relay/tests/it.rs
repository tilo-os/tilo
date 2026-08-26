// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "config_roundtrip.rs"]
mod config_roundtrip;
#[path = "identity_persistence.rs"]
mod identity_persistence;
#[path = "relay_forwards_gossipsub.rs"]
mod relay_forwards_gossipsub;
