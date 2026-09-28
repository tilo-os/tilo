//! Host core shared by Tilo's workspace hosts: the serialized owner over the signed
//! aggregate, delivery, the browser-facing ingress, the embedded relay, desktop
//! commands, collaboration and app hosting.
pub mod canonical_records;
pub mod collaboration;
pub mod host;
pub mod https;
pub mod https_assets;
mod identity;
pub mod owner;
mod seed;
pub mod store;
/// Pre-commit forwarding of verified state items, shared with the Cloud host.
pub use owner::delivery::early_forward;
pub use owner::{CommitReceipt, PendingOperation, SessionOwner, Status};
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
/// Bounds a test wait that ends as soon as its condition holds; only a hang reaches it.
#[cfg(test)]
pub(crate) const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

pub mod enrollment;
pub mod enrollment_http;
pub mod media_authority;
pub mod media_governance;

pub mod owner_commit;

mod action_host;
mod action_pump_cadence;
mod action_timing;
mod committed_authority;
mod desktop_commands;
mod desktop_delivery;
mod document_imports;
mod host_signing;

mod publication;

#[cfg(feature = "media")]
pub(crate) mod media_service;

#[cfg(feature = "media")]
pub mod owner_turn;

#[cfg(feature = "media")]
mod room_media_policy;

#[cfg(feature = "media")]
mod delivery_http;
#[cfg(feature = "media")]
mod onboarding_http;
/// The bounded borrowed decode of the legacy join DTO, shared with hosts that
/// serve their own onboarding carrier.
#[cfg(feature = "media")]
pub use onboarding_http::input as onboarding_input;
#[cfg(feature = "media")]
mod private_http;

#[cfg(feature = "media")]
mod media_control;

#[cfg(feature = "media")]
mod media_ingress;

#[cfg(feature = "media")]
mod legacy_windows;

#[cfg(feature = "media")]
pub mod live_service;

mod host_bytes;

mod staging_manifest;

pub mod ingress;

mod publication_custody;

pub(crate) mod delivery_planning;
