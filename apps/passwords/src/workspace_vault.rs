//! The person's Passwords kept in their workspace's vault, on the server that keeps the
//! workspace: what a window with no vault of its device's own reads and edits, a browser
//! page's above all. Every device of the person's reads the same entries there.
//!
//! The entries are read once the window opens, and again whenever it looks after a while, so
//! what another device changed shows here too. An edit shows at once and is sent to the server;
//! [`WorkspaceVault::flush`] says whether the server kept it yet, and a refusal is read back
//! from the server so the window never shows what it does not keep.

use std::cell::RefCell;

use accounts::{SecretId, SecretItem};
use tilo_clock::time::{Duration, Instant};
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::person_vault::{self, Entries, ForgetEntry, KeepEntry};
use ui::tilo_actions::remote::{ActionCallError, InvokeArgument};

/// How long the entries shown are taken as current before the server is asked again, so what
/// another device of the person's changed shows here.
const FRESH_FOR: Duration = Duration::from_secs(5);

pub(crate) struct WorkspaceVault {
    remote: ui::RemoteActions,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// What the server answered last, edits made here since included; `None` until it first
    /// answered.
    entries: Option<Vec<SecretItem>>,
    /// The read of every entry, while it is asked.
    listing: Option<ui::InvocationCall>,
    /// When the entries were last asked for.
    asked_at: Option<Instant>,
    /// The edits sent and not yet answered.
    writes: Vec<ui::InvocationCall>,
    /// Why the server could not be read, or did not keep an edit, until it next answers.
    trouble: Option<String>,
    /// Advances with every change of what the window shows.
    revision: u64,
}

/// What an answered request said.
enum Settled {
    Entries(Vec<SecretItem>),
    Refused(String),
}

impl WorkspaceVault {
    /// The person's entries, asked of the server `remote` reaches.
    pub(crate) fn new(remote: ui::RemoteActions) -> Self {
        let vault = Self {
            remote,
            state: RefCell::default(),
        };
        vault.ask_entries(&mut vault.state.borrow_mut());
        vault
    }

    fn ask_entries(&self, state: &mut State) {
        state.listing = Some(self.remote.invoke(
            person_vault::target(person_vault::ENTRIES),
            InvokeArgument::None,
            None,
        ));
        state.asked_at = Some(tilo_clock::now());
    }

    fn send(
        &self,
        action: &str,
        argument: serde_json::Result<serde_json::Value>,
    ) -> Result<(), String> {
        let Ok(serde_json::Value::Object(map)) = argument else {
            return Err("The entry could not be sent".to_owned());
        };
        let call = self.remote.invoke(
            person_vault::target(action),
            InvokeArgument::Json(map),
            None,
        );
        let mut state = self.state.borrow_mut();
        state.writes.push(call);
        state.revision = state.revision.wrapping_add(1);
        Ok(())
    }

    /// Take what the server answered since the last look, and ask again for entries grown
    /// stale.
    fn poll(&self) {
        let mut state = self.state.borrow_mut();
        let mut changed = false;
        if let Some(settled) = state.listing.as_ref().and_then(settled) {
            state.listing = None;
            changed = true;
            match settled {
                Settled::Entries(entries) => {
                    // An edit still on its way shows until the server answers it.
                    if state.writes.is_empty() {
                        state.entries = Some(entries);
                    }
                    state.trouble = None;
                }
                Settled::Refused(reason) => state.trouble = Some(reason),
            }
        }
        let mut waiting = Vec::new();
        let mut answered = None;
        for call in std::mem::take(&mut state.writes) {
            match settled(&call) {
                None => waiting.push(call),
                Some(Settled::Entries(entries)) => {
                    changed = true;
                    state.trouble = None;
                    answered = Some(entries);
                }
                Some(Settled::Refused(reason)) => {
                    changed = true;
                    state.trouble = Some(reason);
                    // Show what the server keeps, not the edit it refused.
                    state.asked_at = None;
                }
            }
        }
        state.writes = waiting;
        // The last answer is what the server keeps once no edit is on its way.
        if state.writes.is_empty()
            && let Some(entries) = answered
        {
            state.entries = Some(entries);
        }
        let grown_old = state
            .asked_at
            .is_none_or(|asked| tilo_clock::now().saturating_duration_since(asked) >= FRESH_FOR);
        if state.listing.is_none() && state.writes.is_empty() && grown_old {
            self.ask_entries(&mut state);
        }
        if changed {
            state.revision = state.revision.wrapping_add(1);
        }
    }

    /// Whether the server answered with the entries yet.
    pub(crate) fn is_open(&self) -> bool {
        self.poll();
        self.state.borrow().entries.is_some()
    }

    /// Why the server can't be read now, or did not keep the last edit.
    pub(crate) fn trouble(&self) -> Option<String> {
        self.poll();
        self.state.borrow().trouble.clone()
    }

    /// Advances with every change of what the window shows.
    pub(crate) fn revision(&self) -> u64 {
        self.poll();
        self.state.borrow().revision
    }

    /// Every entry, as the server last answered with the edits made here since.
    pub(crate) fn list(&self) -> Vec<SecretItem> {
        self.poll();
        let mut entries = self.state.borrow().entries.clone().unwrap_or_default();
        entries.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        entries
    }

    pub(crate) fn get(&self, id: &SecretId) -> Option<SecretItem> {
        self.list().into_iter().find(|entry| &entry.id == id)
    }

    /// Keep `entry`, new or edited: it shows at once, and is the server's to keep.
    pub(crate) fn put(&self, entry: &mut SecretItem) -> Result<(), String> {
        entry.updated_unix = tilo_clock::unix_now();
        let entry_value =
            serde_json::to_value(&*entry).map_err(|_| "The entry could not be sent".to_owned())?;
        self.send(
            person_vault::KEEP_ENTRY,
            serde_json::to_value(KeepEntry { entry: entry_value }),
        )?;
        let mut state = self.state.borrow_mut();
        let entries = state.entries.get_or_insert_with(Vec::new);
        entries.retain(|kept| kept.id != entry.id);
        entries.push(entry.clone());
        Ok(())
    }

    /// Forget the entry `id`: whether one was shown.
    pub(crate) fn remove(&self, id: &SecretId) -> Result<bool, String> {
        let shown = self.get(id).is_some();
        self.send(
            person_vault::FORGET_ENTRY,
            serde_json::to_value(ForgetEntry { id: id.0.clone() }),
        )?;
        if let Some(entries) = self.state.borrow_mut().entries.as_mut() {
            entries.retain(|kept| &kept.id != id);
        }
        Ok(shown)
    }

    /// Whether the server kept every edit sent: `Ok(false)` while one is on its way.
    ///
    /// # Errors
    ///
    /// Why the server did not keep the last edit.
    pub(crate) fn flush(&self) -> Result<bool, String> {
        self.poll();
        let state = self.state.borrow();
        if !state.writes.is_empty() {
            return Ok(false);
        }
        match &state.trouble {
            Some(reason) if state.entries.is_some() => Err(reason.clone()),
            _ => Ok(true),
        }
    }
}

/// What `call` was answered, or `None` while it is still on its way.
fn settled(call: &ui::InvocationCall) -> Option<Settled> {
    let refused = |reason: String| Some(Settled::Refused(reason));
    match call.get() {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
            match output
                .value
                .map(serde_json::from_value::<Entries>)
                .and_then(Result::ok)
                .map(|answered| {
                    answered
                        .entries
                        .into_iter()
                        .filter_map(|entry| serde_json::from_value(entry).ok())
                        .collect()
                }) {
                Some(entries) => Some(Settled::Entries(entries)),
                None => refused("The vault's answer could not be read".to_owned()),
            }
        }
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => refused(error.message),
        ui::Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => {
            refused(message.unwrap_or_else(|| "The request was cancelled".to_owned()))
        }
        ui::Arrival::Failed(
            ActionCallError::Refused { reason } | ActionCallError::InvalidRequest { reason },
        ) => refused(reason),
        ui::Arrival::Failed(error) => refused(error.to_string()),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => None,
    }
}
