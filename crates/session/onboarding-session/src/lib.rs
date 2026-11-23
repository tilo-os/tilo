//! The session side of first use: everything setup does that the setup app
//! (`tilo-onboarding`) only asks for. The identity ceremony and its storage,
//! first-boot provisioning, adding this computer with a code, and the
//! ownership of every secret those steps take.
//!
//! The session that runs setup lends what only it has through
//! [`services::SessionServices`].

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod firstboot;
pub mod identity;
pub mod identity_provisioning;
pub mod nearby;
pub mod services;
pub mod view;

