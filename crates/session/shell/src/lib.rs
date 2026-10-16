#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app_icons;
pub mod bluetooth;
pub mod bluetooth_prompt;
pub mod collab;
pub mod compositor;
pub mod config;
pub mod crash_log;
#[cfg(unix)]
pub mod stderr_guard;
pub use tilo_identity_host::device_fleet;
pub mod diagnostics;
pub mod firstboot;
pub mod identity_broker;
mod idle_wake;
pub mod runtime_host;

pub use tilo_identity_host::registry as identity_registry;
pub mod network;
pub mod network_secrets;
pub mod notifications;
pub mod onboarding;
mod onboarding_nearby;
pub mod onboarding_view;
pub mod polkit_agent;
pub mod portal;
pub mod priority_contacts;
mod random;
#[cfg(test)]
#[path = "../tests/runtime_host.rs"]
mod runtime_host_tests;
pub mod security;
pub mod session_runtime;
#[cfg(target_os = "linux")]
pub mod shader_seed;
pub mod shell;
// Room and workspace admission decodes peer bytes in automerge worker
// processes started from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
pub mod tray;
pub mod wayland;
pub mod workspace;
