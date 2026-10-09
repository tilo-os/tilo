//! Chat — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative chat + calls surface tied to workspaces. The
//! convergent state — workspaces, categories, channels, messages, reactions,
//! edits + edit history, threads, members and drafts — lives in an
//! Automerge document ([`doc`]), the room's own record of the chat, changed
//! only through [`actions::ChatAction`]s. Attachments are content-addressed references:
//! the CRDT stores their identity and bounded metadata, while bytes stay in the
//! app's content store. The [`app::ChatApp`] widget is a projection of that
//! document plus an input router.
//!
//! ## Layout
//! Two toolkit drawers (`ui::widget::SidebarDrawer`) frame a conversation
//! column: the navigation drawer (workspace rail + channel list) and the
//! detail drawer (people, or the open thread). Each docks beside the
//! conversation on a wide window and floats over it behind a scrim on a
//! narrow one.
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

pub mod actions;
mod agent_conversation_surface;
pub mod app;
pub mod body;
pub mod call;
pub mod composer;
pub mod continuity;
pub mod doc;
pub mod effects;
pub mod events;
mod external;
pub mod feedback;
pub mod geometry;
pub mod i18n;
pub mod identity;
pub mod images;
mod live_context;
mod local_voice;
pub mod mention;
pub mod model;
pub mod paint;
pub mod permissions;
pub mod picker;
pub mod preferences;
pub mod render_call;
pub mod render_chat;
pub mod resource;
pub mod semantic_read;
mod shared_resources;

pub use app::ChatApp;

#[cfg(test)]
mod component_tests;

mod local_view;
pub use resource::{Chat, ChatShell};

ui::dev_reload_entry!(ChatShell);
