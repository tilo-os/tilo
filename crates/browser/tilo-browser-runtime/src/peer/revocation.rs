//! Device revocation helpers for [`super::WasmPeer`]: issuing root-signed
//! revocations, filtering a session's own revocations, detecting revoked
//! snapshots, normalizing them to a permanent `device-revoked` failure, and
//! stopping further reconnect/retry/refresh commands once revoked.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_session::sync::{Cmd, Outbox};
use tilo_session_client::Link;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList, RevocationReason};
use wasm_bindgen::prelude::*;

use super::{PageSigner, device_key, js_error};

/// Revoke `keyHex`, a device key of this person's, with the root the page
/// holds (`root`): resolves to the root-signed revocation as JSON, which the
/// page keeps beside the person. It
/// is announced to every session of theirs at once, so their servers refuse
/// what the key signs from now on and keep valid what it signed before.
/// `reason` is why: `retired`, `lost`, `stolen` or `compromised`.
#[wasm_bindgen(js_name = revokeDevice)]
pub async fn revoke_device(
    root: JsValue,
    key_hex: String,
    reason: String,
) -> Result<String, JsError> {
    let root = PageSigner::from_js(&root)?;
    let reason = match reason.as_str() {
        "retired" => RevocationReason::Retired,
        "lost" => RevocationReason::Lost,
        "stolen" => RevocationReason::Stolen,
        "compromised" => RevocationReason::Compromised,
        _ => {
            return Err(JsError::new(
                "a device's access ends as retired, lost, stolen or compromised",
            ));
        }
    };
    let entry = tilo_server_client::revoke_device(
        &root,
        device_key(&key_hex)?,
        tilo_clock::unix_now_ns(),
        reason,
    )
    .await
    .map_err(js_error)?;
    for handle in crate::peer_sync::handles_of(&hex::encode(entry.user_public_key)) {
        handle.send(Cmd::Revoke(entry.clone())).map_err(js_error)?;
    }
    // Announced: whatever lists the person's devices on this page has them
    // read from the server once more.
    crate::identity_api::devices_moved();
    serde_json::to_string(&entry).map_err(js_error)
}

/// The revocations of `entries` the person `cert` certifies signed: a
/// session announces only its own person's, since a server reads a hello's
/// entries as its sender's root's and refuses one signed by another.
pub(super) fn own_revocations(
    entries: Vec<RevocationEntry>,
    cert: &DeviceCert,
) -> Result<RevocationList, JsError> {
    let mut revocations = RevocationList::new();
    for entry in entries {
        if entry.user_public_key == cert.user_public_key {
            revocations.insert(entry).map_err(js_error)?;
        }
    }
    Ok(revocations)
}

/// Whether a failure code indicates this device was explicitly revoked.
fn is_revoked_code(code: Option<&str>) -> bool {
    matches!(code, Some("device-revoked" | "device_revoked"))
}

/// Whether a server protocol error message indicates this device was revoked.
fn is_revoked_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("authenticated device was revoked")
        || lower.contains("device was revoked")
        || lower.contains("device is revoked")
        || lower.contains("device-revoked")
        || lower.contains("device_revoked")
}

/// Whether `snapshot` shows that the server revoked this device, either at
/// handshake or mid-session.
fn snapshot_is_revoked(snapshot: &tilo_session_client::PeerSnapshot) -> bool {
    if is_revoked_code(snapshot.error_code)
        || snapshot.error.as_deref().is_some_and(is_revoked_text)
    {
        return true;
    }
    if let Some(tilo_session_client::EntryHealth::Refused { code, message, .. }) = &snapshot.entry
        && (is_revoked_code(Some(code)) || is_revoked_text(message))
    {
        return true;
    }
    snapshot
        .health
        .failures
        .iter()
        .any(|failure| is_revoked_code(Some(failure.code)) || is_revoked_text(&failure.message))
}

/// Normalize a revoked snapshot to `failed` / `device-revoked` JSON so callers
/// always observe a permanent refusal even after the underlying link closes.
fn revoked_snapshot_json(mut snapshot: tilo_session_client::PeerSnapshot) -> String {
    snapshot.phase = "failed";
    snapshot.error_code = Some("device-revoked");
    if snapshot.error.is_none() {
        snapshot.error = Some("This device was removed from your devices.".to_owned());
    }
    serde_json::to_string(&snapshot).unwrap_or_default()
}

/// Tracks whether the server has explicitly revoked this device, freezing a
/// `device-revoked` snapshot and stopping further retries.
#[derive(Clone, Default)]
pub(super) struct RevocationState {
    /// Set once the server refuses this device as revoked; stops further retries.
    revoked: Rc<Cell<bool>>,
    /// Frozen `device-revoked` snapshot JSON kept after the link is closed.
    revoked_snapshot: Rc<RefCell<Option<String>>>,
}

impl RevocationState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Inspect `handle`'s snapshot after a state notification; if the server
    /// revoked this device, freeze the normalized snapshot and close the link.
    pub(super) fn observe(&self, handle: &Link) {
        if !self.revoked.get() {
            let snap = handle.snapshot();
            if snapshot_is_revoked(&snap) {
                self.revoked.set(true);
                *self.revoked_snapshot.borrow_mut() = Some(revoked_snapshot_json(snap));
                // Immediately stop the underlying session loop and dialer.
                handle.close();
            }
        }
    }

    /// Return the frozen revoked snapshot if known, or inspect `handle`'s
    /// current snapshot (freezing and closing if revoked).
    pub(super) fn snapshot_json(&self, handle: &Link) -> String {
        if let Some(frozen) = self.revoked_snapshot.borrow().as_ref() {
            return frozen.clone();
        }
        let snap = handle.snapshot();
        if snapshot_is_revoked(&snap) {
            self.revoked.set(true);
            let frozen = revoked_snapshot_json(snap);
            *self.revoked_snapshot.borrow_mut() = Some(frozen.clone());
            handle.close();
            return frozen;
        }
        serde_json::to_string(&snap).unwrap_or_default()
    }

    /// Refuse reconnect, retry, or refresh once this device was revoked.
    pub(super) fn ensure_active(&self) -> Result<(), JsError> {
        if self.revoked.get() {
            return Err(js_error("authenticated device was revoked"));
        }
        Ok(())
    }
}
