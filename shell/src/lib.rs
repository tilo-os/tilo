#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod bluetooth;
pub mod bluetooth_prompt;
pub mod collab;
pub mod compositor;
pub mod config;
pub mod device_fleet;
pub mod diagnostics;
pub mod firstboot;
pub mod identity_broker;
pub mod identity_registry;
pub mod network;
pub mod notifications;
pub mod onboarding;
pub mod onboarding_view;
pub mod portal;
pub mod priority_contacts;
pub mod sandbox;
pub mod security;
pub mod shell;
pub mod wayland;
pub mod workspace;
