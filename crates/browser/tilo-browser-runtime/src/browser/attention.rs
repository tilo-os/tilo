//! What the browser tab says about the workspace while the person is elsewhere: the page titles
//! the tab and badges its icon from it (`web/tab-attention.mjs`).
//!
//! The shell hears the workspace's news (a mention of this person) on its own, not only while it
//! draws: a hidden tab draws nothing, and news heard only on the next frame reached the person only
//! once they came back to the tab, which is when they no longer needed telling.

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::Function;
use wasm_bindgen::prelude::*;

use super::{Desktop, Mount, with_mount_quietly};

thread_local! {
    /// Each desktop's listener for its attention, and the last it was told.
    static LISTENERS: RefCell<HashMap<u64, (Function, String)>> = RefCell::default();
}

/// Hear what the tab should say about desktop `id` (`{unread, waiting}` JSON, see
/// [`attention`]): told now, and again whenever it changes, drawn or not.
///
/// # Errors
///
/// Rejects a closed mount or one showing a single application.
#[wasm_bindgen]
pub fn desktop_on_attention(id: u64, listener: Function) -> Result<(), JsValue> {
    with_mount_quietly(id, |m| {
        if m.desktop.is_none() {
            return Err(super::error("Desktop unavailable"));
        }
        LISTENERS.with_borrow_mut(|listeners| {
            listeners.insert(id, (listener, String::new()));
        });
        tell(id, m);
        Ok(())
    })
}

/// Tell desktop `id`'s listener what its tab should say, when that changed.
pub(super) fn tell(id: u64, m: &Mount) {
    let Some(desktop) = m.desktop.as_ref() else {
        return;
    };
    let text = attention(desktop).to_string();
    let listener = LISTENERS.with_borrow_mut(|listeners| {
        let (listener, told) = listeners.get_mut(&id)?;
        (*told != text).then(|| {
            told.clone_from(&text);
            listener.clone()
        })
    });
    if let Some(listener) = listener {
        let _ = listener.call1(&JsValue::NULL, &JsValue::from_str(&text));
    }
}

/// Forget desktop `id`'s listener: its mount closed.
pub(super) fn forget(id: u64) {
    LISTENERS.with_borrow_mut(|listeners| listeners.remove(&id));
}

/// What the tab says, and what closing it would cut short: `working`, the titles of the jobs
/// running on the shell; `unread`, the notices the person has not seen (the shell's bell count);
/// and `waiting`, the agents waiting for the person to answer them, by the name their client goes
/// by ("Claude Code").
fn attention(desktop: &Desktop) -> serde_json::Value {
    serde_json::json!({
        "unread": desktop.persistence.notifications.unread(),
        "waiting": crate::agent_questions::waiting_agents(),
        // What leaving the tab now would cut short: the jobs on the shell (a file going up).
        "working": ui::jobs::running().iter().map(|job| job.title().to_owned()).collect::<Vec<_>>(),
    })
}

impl Desktop {
    /// The workspace's news since the last call, told on the shell (and, while the tab is hidden,
    /// in the browser's notifications): mentions of this person. A drawn frame hears it too; this
    /// is for the tab that draws none.
    pub(super) fn hear_news(&self) {
        let _ = self.project_governance(self.shared.peer);
        self.tell_mentions();
    }
}
