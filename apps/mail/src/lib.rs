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
pub mod app;
pub mod compose;
pub mod doc;
pub mod engine;
pub mod model;
pub mod sample;
pub mod transport;
pub mod util;
pub mod wysiwyg;

pub use app::MailApp;

use ui::widget::Widget;

/// The single integration point used by both the live event loop and the
/// `--shots` offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(MailApp::new_sample())
}

/// The viewport a named `--shots` scenario should render at.
pub fn shot_viewport(scenario: &str) -> (u32, u32) {
    match scenario {
        // Narrow single-pane responsive layout.
        "narrow" => (560, 860),
        _ => (1280, 800),
    }
}

/// Build the root widget for a named `--shots` scenario, pre-seeded so a single
/// offscreen frame shows the feature in question. The same widget the live app
/// uses — only the initial state differs.
pub fn app_root_scenario(scenario: &str) -> Box<dyn Widget> {
    let mut app = MailApp::new_sample();
    match scenario {
        "compose" => app.prime_compose_shot(),
        "narrow" => app.prime_narrow_shot(),
        _ => {}
    }
    Box::new(app)
}
