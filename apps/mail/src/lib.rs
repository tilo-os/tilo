//! Mail — qOS native email client, built on the CRDT-native `ui` toolkit.
//!
//! ## Layout
//! Three panes: a folder/account **sidebar** (Inbox/Starred/Sent/Drafts/
//! Archive/Junk/Trash with unread counts), a **message list** threaded by
//! conversation (sender · subject · snippet · date · read/star/attachment
//! indicators), and a **reader** (headers · body · attachments). A modal
//! **Compose** window (To/Cc/Bcc · subject · body · attachments · send /
//! save-draft) and an **Account-setup** form (IMAP/SMTP host/port/TLS/user)
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
//! Per-message flags (read/starred/folder), the selected folder / view mode /
//! search / selection, the account config, and the active draft all live in
//! the Automerge document via `doc::MailDoc`, so they sync and undo. The
//! immutable message corpus is hydrated into `model::Mailbox`.
//!
//! ## Offline / `--shots`
//! `--shots` has no event loop and no live account, so it renders a bundled
//! sample mailbox (`sample.rs`) — several threaded messages with bodies and
//! attachments, two of them hydrated through the **real** `mail-parser` path.
//! The live app does real IMAP/SMTP instead.

pub mod account;
pub mod actions;
pub mod app;
pub mod autoconfig;
pub mod compose;
pub mod doc;
pub mod draw;
pub mod engine;
pub mod html_mail;
pub mod model;
pub mod oauth;
pub mod pane;
pub mod sample;
pub mod search_provider;
pub mod state;
pub mod transport;
pub mod util;
pub mod validate;
pub mod wysiwyg;

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
        "narrow" | "narrow_detail" | "narrow_drawer" => (560, 860),
        "mid" => (1000, 720),
        "compose" => (1180, 820),
        "setup" | "setup_add" | "setup_oauth" | "firstrun" => (1100, 780),
        "html_mail" => (1280, 820),
        "sidebar_collapsed" => (1200, 760),
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
        "compose" => app.prime_compose_shot(),
        "narrow_detail" => app.prime_detail_shot(),
        "narrow_drawer" => app.prime_drawer_shot(),
        "setup" => app.prime_setup_shot(),
        // The add-account variant opens a blank setup sheet (no editing target)
        // so the shot shows the "Add account" flow with autofilled server
        // fields rather than an existing account's values.
        "setup_add" => app.prime_setup_add_shot(),
        "setup_oauth" => app.prime_setup_oauth_shot(),
        "html_mail" => app.prime_html_mail_shot(),
        "sidebar_collapsed" => app.prime_sidebar_collapsed_shot(),
        _ => {}
    }
    Box::new(app)
}
