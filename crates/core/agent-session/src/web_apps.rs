//! The apps an agent can open in a browser session: one per app whose manifest
//! declares the web platform. Keep it in step with the app manifests.
//!
//! This list only shrinks. Each entry links a Rust app's crate so its records can be
//! held with no window open, which the device daemon does not yet do for Rust apps
//! (it serves tilolang apps' views and actions only). A push that ports an app to
//! tilolang and deletes its Rust crate also drops the app's line here, and its
//! dependency in this crate's `Cargo.toml`, in that same push: the ported app is
//! reached through the daemon's view path and its actions on the one dispatch, never
//! by linking it. No app is added here.
//!
//! Settings is not linked here: an agent reaches it through Settings' actions on
//! the device's dispatch.

use ui::application::ApplicationDefinition;

/// Every application an agent's browser session can open.
#[must_use]
pub fn web_apps() -> Vec<&'static ApplicationDefinition> {
    vec![
        activity::definition(),
        assistant::definition(),
        camera::definition(),
        chip_design::definition(),
        tilo_console::definition(),
        diffmerge::definition(),
        maps::definition(),
        messages::definition(),
        model_providers::definition(),
        poker::definition(),
        scratchpad::definition(),
        shortcuts::definition(),
        slides::definition(),
        snippets::definition(),
        speed_test::definition(),
        terminal::definition(),
        totp::definition(),
        typst_editor::definition(),
        weather::definition(),
    ]
}
