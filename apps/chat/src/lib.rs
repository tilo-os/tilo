//! Chat — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative chat + calls surface that runs in a Tilo workspace, its
//! room. The convergent state — categories, channels, messages, reactions,
//! edits + edit history, threads and members — is the [`record::Chat`]
//! model, the room's own record of the chat, changed only through its verbs
//! and field edits. Attachments are content-addressed references:
//! the CRDT stores their identity and bounded metadata, while bytes stay in the
//! app's content store. The [`app::ChatApp`] widget is a projection of that
//! document plus an input router.
//!
//! ## Layout
//! The window's shell (`ui::widget::NavShell`) keeps the room's channels in
//! its sidebar (`sidebar.rs`) beside the conversation; a toolkit drawer
//! (`ui::widget::SidebarDrawer`) holds the detail (people, or the open
//! thread). Each docks beside the conversation on a wide window and floats
//! over it on a narrow one.
//!
//! ## Calls
//! Voice controls send typed local commands to the privileged native media
//! service. Joins begin muted; microphone use requires fresh local consent.
//! Current audio streams come only from its verified publisher inventory;
//! nothing about a call is written to the document. Read access permits listening,
//! while publication also requires current Write authority and a capture lease.
//!
//! [`ChatShell`] is the app every host runs and mounts ([`definition`]).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod agent_conversation_surface;
pub mod agent_stream;
pub mod app;
pub mod body;
pub mod call;
mod channel_sheet;
pub mod composer;
pub mod continuity;
pub mod demo;
pub mod drafts;
pub mod effects;
pub mod events;
mod external;
pub mod feedback;
pub mod geometry;
mod host_model;
pub mod i18n;
pub mod identity;
mod image_arrival;
pub mod images;
mod live_context;
mod local_voice;
pub mod mention;
mod mention_alerts;
pub mod messages;
pub mod model;
mod moderation;
pub mod paint;
pub mod parts;
pub mod permissions;
mod person_conversation;
#[cfg(test)]
mod person_conversation_tests;
pub mod picker;
pub mod preferences;
mod private_draft;
pub mod projection;
pub mod record;
pub mod render_call;
pub mod render_chat;
pub mod resource;
mod shared_resources;
mod sidebar;
pub mod transcript;
mod window_bar;

pub use app::ChatApp;

impl ChatApp {
    /// Bind a mount-local model capability supplied by the host.
    #[must_use]
    pub fn with_models(mut self, models: ui::models::Models) -> Self {
        self.host_model.models = models;
        self
    }
}

#[cfg(test)]
mod component_tests;
#[cfg(all(test, unix))]
mod native_models_tests;
#[cfg(test)]
mod testing;

mod local_view;
pub use resource::{Chat, ChatShell};

// The framework lends the mounting host's model capability to the live view.
// Keep its reload entry points and standard capture adapter.
mod framework_entry {
    ui::dev_reload_entry!(super::ChatShell);
}

/// The standard Chat resource component, with this mount's host model capability.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    framework_entry::definition()
}

thread_local! {
    static STANDALONE_MODELS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Permit only the standalone binary's live root to adopt its launcher descriptor.
pub fn enable_standalone_models() {
    STANDALONE_MODELS.set(true);
}

pub(crate) fn standalone_models() -> Option<ui::models::Models> {
    if STANDALONE_MODELS.replace(false) {
        return Some(ui::models::Models::from_launcher().unwrap_or_default());
    }
    None
}
