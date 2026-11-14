//! What the browser tab says about the workspace while the person is elsewhere: the page titles
//! the tab and badges its icon from it (`web/tab-attention.mjs`).
//!
//! The desk hears the workspace's news (an agent finishing, a mention, someone renaming the
//! workspace) on its own, not only while it draws: a hidden tab draws nothing, and news heard
//! only on the next frame reached the person only once they came back to the tab, which is when
//! they no longer needed telling. What the person did not cause and would want to hear of
//! elsewhere also goes to the browser's notifications while the tab is hidden, and choosing one
//! brings the tab forward onto it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

use js_sys::Function;
use tilo_lock::MutexRecover as _;
use wasm_bindgen::prelude::*;

use super::{Desktop, Mount, with_mount_quietly};

/// The scheme of a browser notification about an agent: choosing it opens the agent's card.
const AGENT_SCHEME: &str = "tilo-agent";

thread_local! {
    /// Each desktop's listener for its attention, and the last it was told.
    static LISTENERS: RefCell<HashMap<u64, (Function, String)>> = RefCell::default();
}

/// The agents (by client) whose browser notification the person chose, waiting for the desk.
static CHOSEN_AGENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
/// running on the desk; `unread`, the notices the person has not seen (the desk's bell count);
/// `waiting`, the agents waiting for the person to answer them, by the name their client goes
/// by ("Claude Code"); and `finished`, the agents whose finishing the person has not seen yet,
/// each with its notice's `id`, so the page tells apart one it already showed in view.
fn attention(desktop: &Desktop) -> serde_json::Value {
    let notifications = &desktop.persistence.notifications;
    let finished: Vec<serde_json::Value> = {
        let log = notifications.log();
        log.active()
            .iter()
            .chain(log.history())
            .filter(|notice| notifications.is_unread(notice.id))
            .filter_map(|notice| {
                // An agent's finishing is the notice that offers its card.
                let agent = notice
                    .actions
                    .iter()
                    .find_map(|(key, _)| key.strip_prefix(tilo_desktop::here::AGENT_CARD_ACTION))?;
                Some(serde_json::json!({"id": notice.id, "agent": agent}))
            })
            .collect()
    };
    serde_json::json!({
        "unread": notifications.unread(),
        "waiting": crate::agent_questions::waiting_agents(),
        "finished": finished,
        // What leaving the tab now would cut short: the jobs on the desk (a file going up).
        "working": ui::jobs::running().iter().map(|job| job.title().to_owned()).collect::<Vec<_>>(),
    })
}

/// Tell the browser of `notice`, posted on the desk about an agent: while the tab is hidden and
/// the person allows it, a notification that, chosen, brings the tab forward onto the agent's card.
pub(super) fn tell_browser_of_agent(
    notice: &tilo_desktop_surface::notifications::Notification,
    client: &str,
) {
    static LISTENING: std::sync::Once = std::sync::Once::new();
    LISTENING.call_once(|| {
        ui::notify::set_activation_handler(AGENT_SCHEME, |uri| {
            if let Some(client) = uri
                .strip_prefix(AGENT_SCHEME)
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|client| js_sys::decode_uri_component(client).ok())
                .and_then(|client| client.as_string())
            {
                CHOSEN_AGENTS.lock_recover().push(client);
                super::wake_desktops();
            }
        });
    });
    let mut told = ui::notify::Notification::new(
        // A tag per agent: a newer run's notice replaces the older one's.
        format!("agent-{:08x}", tag_hash(client)),
        notice.title.clone(),
        notice.body.clone(),
    );
    told.activation = Some(format!(
        "{AGENT_SCHEME}:{}",
        String::from(js_sys::encode_uri_component(client))
    ));
    crate::host_services::tell_browser(&told);
}

/// The agents whose browser notification the person chose since the last call.
pub(super) fn take_chosen_agents() -> Vec<String> {
    std::mem::take(&mut *CHOSEN_AGENTS.lock_recover())
}

/// A short stable number for `text`, for a notification tag of at most 15 characters.
fn tag_hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

impl Desktop {
    /// The workspace's news since the last call, told on the desk (and, while the tab is hidden,
    /// in the browser's notifications): what others did to the desk, agents that finished, and
    /// mentions of this person. A drawn frame hears it too; this is for the tab that draws none.
    pub(super) fn hear_news(&self) {
        self.post_workspace_notices();
        self.announce_agents_finished();
        self.tell_mentions();
    }

    /// When the news is next due with nothing else arriving, at or after `now`: an agent that
    /// left is said finished only after a reconnect's grace.
    pub(super) fn news_due(&self, now: ui::time::Instant) -> Option<ui::time::Instant> {
        let due_ms = self.agent_notices_due_ms()?;
        let wait = due_ms.saturating_sub(crate::shell::now_ms());
        now.checked_add(ui::time::Duration::from_millis(wait.max(1)))
    }
}
