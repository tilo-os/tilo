//! The one enrollment every Tilo image uses: a machine joins the person's
//! identity with the single-use token they made for it, or by showing a short
//! code one of their devices approves.
//!
//! This crate is the protocol both ends speak — the messages, their sealing,
//! the answer that admits a machine and the certificate it gains — and the
//! serving side: what a device of the person's does when a machine dials it
//! over the internet, whoever is there to answer. It also installs the
//! verified answer on the joining native device. The image's helper
//! (`tilo-enroll`) and the person's session both call it, so an enrollment is
//! one implementation, not one per surface. Computers and browsers also
//! use [`requester`] to ask a workspace server for a member certificate.
//!
//! Which methods an image offers is its own declaration
//! (`tilo_identity::enrollment`); the enrollment itself is the same.

#[cfg(not(target_arch = "wasm32"))]
pub mod approval;
#[cfg(not(target_arch = "wasm32"))]
pub mod install;
#[cfg(not(target_arch = "wasm32"))]
pub mod protocol;
pub mod requester;
#[cfg(not(target_arch = "wasm32"))]
pub mod root;
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod waiting;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
