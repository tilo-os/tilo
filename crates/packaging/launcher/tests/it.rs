#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "bus_filters.rs"]
mod bus_filters;
#[path = "egress_proxy.rs"]
mod egress_proxy;
#[path = "launcher_smoke.rs"]
mod launcher_smoke;
#[path = "sandbox_environment.rs"]
mod sandbox_environment;
#[path = "support.rs"]
mod support;
#[cfg(target_os = "linux")]
#[path = "system_view.rs"]
mod system_view;
#[path = "workspace_confinement.rs"]
mod workspace_confinement;

#[path = "managed.rs"]
mod managed;

#[cfg(target_os = "linux")]
#[path = "netproxy.rs"]
mod netproxy;

#[cfg(target_os = "linux")]
#[path = "capture_service.rs"]
mod capture_service;
