//! Library root for the Tilo native Personal Settings app — the device and
//! account settings hub. Workspace-owned policy stays in the shell's
//! contextual Workspace Settings surface; cross-cutting domains share a
//! resolver instead of letting either UI write the other's state.
//!
//! `SettingsApp` (in [`settings_app`]) is the root widget: a categorized
//! sidebar plus a content pane per category, responsive between a two-pane
//! desktop layout and a single-pane narrow layout. The binary in `main.rs`
//! is a thin shim that hands the widget to `ui::App` for the live event
//! loop, or renders one offscreen frame for `--shots`.
//! Release builds keep this composition app-local while service engines live
//! in the shared runtime, preserving independently updatable settings UI.
//!
//! ## Module map
//!
//! - [`categories`] — the sidebar's [`categories::Section`]s and
//!   [`categories::Category`]s.
//! - [`sidebar`] — the sidebar's navigation list of categories.
//! - [`panes`] — one module per category content pane.
//! - [`widgets`] — small composition helpers (card, setting row, caption)
//!   shared across panels.
//! - [`accounts`] — the **shared external-accounts store** Settings owns
//!   and Mail / Calendar / file apps consume.
//! - [`region`] — the **shared region & formats store** (global user
//!   locale) Settings owns and Sheets / Weather consume.
//! - [`prefs`] — the **shared system-preferences store** (device sounds,
//!   display comfort, pointer/keyboard behaviour, update policy) Settings
//!   owns and the compositor / update service consume.
//! - [`dispatcher`] — the `os-actions` plumbing the hardware panes share.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod account_probe;
pub mod accounts;
pub mod actions;
pub mod categories;
pub mod resource;
pub use os_actions::codec_pack;
mod continuity;
mod device_fleet;
pub mod dispatcher;
pub mod i18n;
pub(crate) mod input_sources;
pub mod panes;
pub mod prefs;
pub mod region;
pub mod search_provider;
pub mod settings_app;
#[cfg(not(feature = "thin"))]
pub mod shots;
pub mod sidebar;
pub mod speaker_setup;
pub mod widgets;

pub use settings_app::SettingsApp;

/// Ensure an Automerge `Text` object exists at `key` under `root`, returning it.
///
/// A `TextInput` bound via `with_on_change_path` persists typed characters by
/// splicing into an **existing** `Text` object — it never creates one. So any
/// field the user is meant to type into (the add-account
/// form, a Wi-Fi password) must have its backing object seeded first. Without
/// this, edits update only the widget's local buffer and are silently lost on
/// the next structure rebuild — which is exactly why those fields read as dead.
///
/// Idempotent: returns the existing object when one is already present, and
/// preserves a scalar string previously parked at the key (e.g. a seeded value)
/// by splicing it into the new object.
pub(crate) fn ensure_text_object(
    state: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    key: &str,
) -> automerge::ObjId {
    use automerge::{ObjType, ReadDoc, Value, transaction::Transactable};
    if let Ok(Some((Value::Object(ObjType::Text), obj))) = state.get(root, key) {
        return obj;
    }
    let existing = match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(t) => t.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    // `put_object` only fails when `root` doesn't exist or isn't a keyed
    // (Map/Table) object; every caller passes either the document root or a
    // map key resolved earlier in the same synchronous `sync`, so neither
    // case is reachable here. There is no meaningful fallback `ObjId` to
    // return instead — `automerge::ROOT` would silently redirect the
    // caller's later `splice_text` onto the wrong (non-Text) object.
    #[allow(
        clippy::expect_used,
        reason = "root is always a live keyed object in the current transaction; see comment above"
    )]
    let obj = state
        .put_object(root, key, ObjType::Text)
        .expect("seed a Text object for an editable field");
    if !existing.is_empty() {
        let _ = state.splice_text(&obj, 0, 0, &existing);
    }
    obj
}

/// Clear the contents of the `Text` object at `key` (seeding one if absent) so
/// an editable field is reset to empty **without** replacing the object with a
/// scalar — which would break the next round of typing.
pub(crate) fn clear_text_object(
    state: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    key: &str,
) {
    use automerge::{ReadDoc, transaction::Transactable};
    let obj = ensure_text_object(state, root, key);
    if let Ok(text) = state.text(&obj) {
        let n = text.chars().count();
        if n > 0 {
            let _ = state.splice_text(&obj, 0, n as isize, "");
        }
    }
}
pub use resource::{Settings, SettingsShell};
