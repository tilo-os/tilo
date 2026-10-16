// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "bus_filters.rs"]
mod bus_filters;
#[path = "launcher_smoke.rs"]
mod launcher_smoke;
#[path = "netgate_egress.rs"]
mod netgate_egress;
#[path = "room_confinement.rs"]
mod room_confinement;
#[path = "sandbox_environment.rs"]
mod sandbox_environment;
#[path = "support.rs"]
mod support;

#[path = "managed.rs"]
mod managed;
