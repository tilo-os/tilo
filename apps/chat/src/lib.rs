//! Chat — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A collaborative chat + calls surface tied to workspaces. The
//! convergent state — workspaces, categories, channels, messages, reactions,
//! edits + edit history, threads, members, and voice membership — lives in an
//! Automerge document ([`doc`]) that is **persisted to disk** ([`storage`]) so
//! it survives across runs. Attachments are qdoc content-addressed references:
//! the CRDT stores their identity and bounded metadata, while bytes stay in the
//! shared blob store. The [`app::ChatApp`] widget is a projection of that
//! document plus an input router.
//!
//! ## Layout
//! Two adaptive flaps ([`responsive`]) frame a conversation column: a left
//! flap (workspace rail + channel sidebar) and a right flap (members /
//! thread). Each docks beside the content on a wide window and slides over
//! it behind a scrim on a narrow one — a GNOME Adwaita overlay-flap.
//!
//! ## Calls
//! Conferencing ([`call`]) is a movable floating overlay (drag to
//! reposition) or an expanded grid, with an active-speaker ring, an
//! adaptive participant list, voice-channel presence in the sidebar, and
//! join/leave sounds. App call actions are checked against the host-provided
//! [`permissions::ChatPermissions`] context; capture additionally requires a
//! source the user selected through the system portal. Chat makes no claim of
//! room encryption or its own ACL system. The real-time media engine ([`media`])
//! defines a MoQ-over-iroh transport trait with a simulated local backend and a
//! feature-gated live one.
//!
//! `app_root()` is the single integration point used by the live event
//! loop; [`app_root_for_shot`] stages a scenario for the offscreen renderer.

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

pub mod actions;
pub mod app;
// Live call media (real microphone/Opus audio + camera/screen H.264) — compiled
// only under `live-media`; the default build carries no GStreamer/openh264/iroh.
#[cfg(feature = "live-media")]
pub mod audio;
pub mod call;
pub mod continuity;
pub mod doc;
pub mod emoji;
pub mod events;
pub mod geometry;
pub mod i18n;
pub mod identity;
pub mod images;
pub mod media;
pub mod mention;
pub mod model;
pub mod paint;
pub mod permissions;
pub mod picker;
pub mod render_call;
pub mod render_chat;
pub mod responsive;
pub mod search_provider;
pub mod storage;
#[cfg(feature = "live-media")]
pub mod video;

pub use app::ChatApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree (live event loop). The
/// document behind it is loaded and written by `app-state`; see `main`.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ChatApp::new())
}

/// Like [`app_root`] but staged with a named scenario for a headless
/// screenshot, and explicitly NON-persistent (it never touches disk, so a
/// `--shots` run can't clobber a real workspace). An empty/unknown name
/// yields the default frame.
///
/// Recognised scenarios:
/// * `"narrow"` — collapse both flaps for the responsive overlay capture.
/// * `"compact"` — medium window: channels docked, members summoned on demand.
/// * `"starter"` — the real first-run empty workspace (no demo content).
/// * `"people"` — the workspace People roster.
/// * `"thread-narrow"` — a thread as a phone-width pushed detail view.
/// * `"collaboration"` — the desktop thread plus active huddle state.
/// * `"context"` — open a channel context menu.
/// * `"msgmenu"` — open an owned-message action menu.
/// * `"attach"` — open the workspace file picker over the composer.
/// * `"attach-loading"` / `"attach-error"` — bounded attachment lifecycle
///   feedback in the same picker.
/// * `"edit"` — show the shared inline editor on a sent message.
/// * `"composer"` — stage a multiline draft in the main composer.
/// * `"call"` / `"share"` — expanded call and screen-share states.
/// * `"floating-call"` and its ownership-transition variants — the compact
///   call surface before, during, and after compositor handoff.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = ChatApp::new_ephemeral();
    app.seed_shot_scenario(scenario);
    Box::new(app)
}
