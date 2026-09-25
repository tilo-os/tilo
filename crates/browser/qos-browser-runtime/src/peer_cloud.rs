//! Private hosted enrollment custody. Only the owner facade admits a Peer state.
use tilo_relay as relay;
use tilo_user_identity as identity;
mod protocol;
// Browser-only owner continuation; the host protocol test includes only
// `protocol.rs`, never this admission facade.
mod owner;
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(unused_imports, reason = "re-exports used only by the wasm32 build")
)]
pub(crate) use owner::{CLOCK_SKEW, CloudOwner, CloudStep, EnrolledOwner, create, scope};
#[cfg(test)]
mod tests;
