//! Firewall's resources: the configuration a workspace shares — its zones,
//! their rules and the active zone — and what each device last pushed to its
//! firewalld, as the app that draws them and an agent that reads them both see
//! them.
//!
//! Reading and changing firewalld happen in the app, on the device: the model
//! holds only what people share and what each device reported back.

// Ports, priorities and the trust scale are bounded values the rules themselves
// carry; the arithmetic on them cannot overflow an address. Panic-producing
// lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded rule arithmetic on ports and priorities"
)]

pub mod model;
mod resource;

pub use model::{Firewall, FirewallConfig, ZoneKind};
pub use resource::AppliedConfig;
