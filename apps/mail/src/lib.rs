//! Mail — Tilo native email client, built on the CRDT-native `ui` toolkit.
//!
//! ## Layout
//! The toolkit's window shell with three columns: a **sidebar** of accounts
//! and their folders (`sidebar.rs`), the **message list** threaded by
//! conversation (`list.rs`), and the **reader** showing the open conversation
//! (`reader.rs`). The column headers carry the list's and the message's
//! actions (`header.rs`). A **compose** window and the **account** sheet
//! layer on top.
//!
//! ## Protocols
//! * Incoming MIME is parsed with `mail-parser` (`transport::parse_eml`).
//! * Outgoing MIME is built with `mail-builder` (`transport::build_mime`).
//! * The live app drives IMAP (`async-imap`) and SMTP (`lettre`) on a worker
//!   thread; see `transport.rs` for the seam. Those TLS/async stacks are kept
//!   out of the headless build so it stays small and green.
//!
//! ## State
//! Per-message flags (read/starred/folder), selected folder/view state, and
//! the active draft live in the Automerge document via `doc::MailDoc`, so they
//! sync and undo. Account identities, connection endpoints, and credentials
//! are device-local in the shared Tilo account store + encrypted vault. The
//! immutable message corpus is hydrated into `model::Mailbox`.
//!
//! ## Offline / `--shots`
//! `--shots` has no event loop and no live account, so it renders a bundled
//! sample mailbox (`sample.rs`) — several threaded messages with bodies and
//! attachments, two of them hydrated through the `mail-parser` path. The live
//! app uses IMAP/SMTP instead.

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

pub mod account;
pub mod actions;
pub mod app;
pub mod attachment_store;
pub mod autoconfig;
pub mod compose;
pub mod doc;
pub mod engine;
pub mod header;
pub mod html_mail;
pub mod i18n;
pub mod list;
pub mod model;
pub mod oauth;
pub mod reader;
pub mod resource;
pub mod sample;
pub mod search_provider;
pub mod shortcuts_overlay;
pub mod sidebar;
pub mod state;
pub mod transport;
pub mod util;
pub mod validate;

pub use app::MailApp;

use ui::widget::Widget;

/// The live event-loop root: a real account list loaded from disk and the
/// account engine spawned. No fabricated sample corpus — an unconfigured
/// account shows an empty state.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(MailApp::new_live())
}

/// The viewport a named `--shots` scenario should render at.
pub fn shot_viewport(scenario: &str) -> (u32, u32) {
    match scenario {
        "narrow" | "narrow_detail" | "narrow_drawer" | "compose_narrow" | "setup_narrow" => {
            (560, 860)
        }
        "mid" => (1000, 720),
        "compose" | "reply" => (1180, 820),
        "setup" | "setup_add" | "setup_oauth" | "firstrun" | "error" => (1100, 780),
        "html_mail" => (1280, 820),
        _ => (1280, 800),
    }
}

/// Build the root widget for a named `--shots` scenario, pre-seeded from the
/// bundled sample corpus so a single offscreen frame shows the feature. The
/// sample corpus is hermetic — used only here and in tests, never at runtime.
pub fn app_root_scenario(scenario: &str) -> Box<dyn Widget> {
    // The first-run scenario has NO account configured, so it builds the empty
    // app (which shows the onboarding screen) rather than the sample corpus.
    if scenario == "firstrun" {
        return Box::new(MailApp::new_first_run());
    }
    let mut app = MailApp::new_sample();
    match scenario {
        "compose" | "compose_narrow" => app.prime_compose_shot(),
        "narrow_detail" => app.prime_detail_shot(),
        "narrow_drawer" => app.prime_drawer_shot(),
        "setup" | "setup_narrow" => app.prime_setup_shot(),
        // The add-account variant opens a blank setup sheet (no editing target)
        // so the shot shows the "Add account" flow with autofilled server
        // fields rather than an existing account's values.
        "setup_add" => app.prime_setup_add_shot(),
        "setup_oauth" => app.prime_setup_oauth_shot(),
        "html_mail" => app.prime_html_mail_shot(),
        "preview" => app.prime_preview_shot(),
        "bulk" => app.prime_bulk_shot(),
        "bulk_purge_confirmation" => app.prime_bulk_purge_confirmation_shot(),
        "reply" => app.prime_reply_shot(),
        "context_menu" => app.prime_context_menu_shot(),
        "empty" => app.prime_empty_shot(),
        "loading" => app.prime_loading_shot(),
        "offline" => app.prime_offline_shot(),
        "error" => app.prime_error_shot(),
        _ => {}
    }
    Box::new(app)
}
pub use resource::{Mail, MailShell};
