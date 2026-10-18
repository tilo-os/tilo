// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[cfg(target_os = "linux")]
#[path = "app_spawner.rs"]
mod app_spawner;
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
#[cfg(target_os = "linux")]
#[path = "system_view.rs"]
mod system_view;

#[path = "managed.rs"]
mod managed;

#[cfg(target_os = "linux")]
#[path = "capture_service.rs"]
mod capture_service;
