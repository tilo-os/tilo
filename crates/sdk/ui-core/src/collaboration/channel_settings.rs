//! An app's settings when the shell keeps them: its view of the session's
//! records, reached over the app's collaboration channel.
//!
//! A sandboxed app has no settings document of its own to read; the
//! launcher gives it the shell's collaboration service instead. At launch
//! [`install`] makes a [`ChannelSettings`] this process's
//! `settings_store::SettingsSource`, so every reader — `Settings<T>`,
//! `cx.settings::<T>()`, the toolkit's shared sections — reads the app's
//! view (its own `app_personal` / `app_device_local` entries and the shared
//! sections, see `settings_store::app_scope`) and writes its entries into
//! the shell's records. The broker worker fills the view when the primary
//! route joins and keeps it current as the records change, here or on
//! another of the person's devices; each change wakes the app.
//!
//! A write shows at once and is sent to the shell in the order it was made.
//! While writes are on their way, views from the shell are held back, so a
//! view taken before a write cannot undo it on screen; the shell's answer to
//! the last write is the view that follows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use serde_json::{Map, Value};
use settings_store::{HostDocument, SettingsSource};

/// The settings source a launched app reads through its channel.
pub(crate) struct ChannelSettings {
    app_id: String,
    state: Mutex<State>,
    revision: AtomicU64,
}

#[derive(Default)]
struct State {
    /// What this process reads: the shell's last view with this process's
    /// unanswered writes applied.
    view: Map<String, Value>,
    /// The shell's last view.
    confirmed: Map<String, Value>,
    /// Writes not yet handed to the broker worker, oldest first.
    outbox: Vec<(String, Map<String, Value>)>,
    /// Writes the worker sent and the shell has not answered.
    in_flight: usize,
}

static INSTALLED: RwLock<Option<Arc<ChannelSettings>>> = RwLock::new(None);

/// Make the shell's records, reached through the collaboration channel,
/// the settings of app `app_id` in this process. Not when the launcher
/// handed the process the session document itself (Settings), which it
/// then reads and writes directly. Only the first call installs.
pub(crate) fn install(app_id: &str) {
    if crate::host::environment().session_settings.is_some() || app_id.is_empty() {
        return;
    }
    let mut installed = INSTALLED.write().unwrap_or_else(PoisonError::into_inner);
    if installed.is_some() {
        return;
    }
    let channel = Arc::new(ChannelSettings::new(app_id));
    settings_store::install_source(Some(Arc::clone(&channel) as Arc<dyn SettingsSource>));
    *installed = Some(channel);
}

/// Restore the process's own settings document.
#[cfg(test)]
pub(crate) fn uninstall() {
    settings_store::install_source(None);
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = None;
}

/// The installed channel settings, for the broker worker to serve.
pub(crate) fn installed() -> Option<Arc<ChannelSettings>> {
    INSTALLED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

impl ChannelSettings {
    pub(crate) fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            state: Mutex::new(State::default()),
            revision: AtomicU64::new(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Show `view` unless writes are still on their way. Returns whether
    /// what this process reads changed.
    fn show(&self, state: &mut State, view: Map<String, Value>) -> bool {
        if state.in_flight > 0 || !state.outbox.is_empty() || state.view == view {
            return false;
        }
        state.view = view;
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// The shell's view arrived or changed. Returns whether what this
    /// process reads changed.
    pub(crate) fn received(&self, view: Map<String, Value>) -> bool {
        let mut state = self.lock();
        state.confirmed.clone_from(&view);
        self.show(&mut state, view)
    }

    /// Writes for the worker to send, in order; each is answered with
    /// [`Self::answered`].
    pub(crate) fn take_outbox(&self) -> Vec<(String, Map<String, Value>)> {
        let mut state = self.lock();
        let outbox = std::mem::take(&mut state.outbox);
        state.in_flight += outbox.len();
        outbox
    }

    /// The shell's answer to one sent write: the view after it, or why it
    /// was refused. Returns whether what this process reads changed.
    pub(crate) fn answered(&self, answer: Result<Map<String, Value>, String>) -> bool {
        let mut state = self.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        let view = match answer {
            Ok(view) => {
                state.confirmed.clone_from(&view);
                view
            }
            Err(error) => {
                tracing::warn!(app = %self.app_id, %error, "settings: the shell refused a write");
                state.confirmed.clone()
            }
        };
        self.show(&mut state, view)
    }
}

/// One sent write until the shell answers it. A write whose worker stops
/// before the answer counts as refused, so views are not held back for it.
pub(crate) struct Unanswered<'a> {
    settings: Option<&'a ChannelSettings>,
}

impl<'a> Unanswered<'a> {
    pub(crate) fn new(settings: &'a ChannelSettings) -> Self {
        Self {
            settings: Some(settings),
        }
    }

    /// See [`ChannelSettings::answered`].
    pub(crate) fn answer(mut self, answer: Result<Map<String, Value>, String>) -> bool {
        self.settings
            .take()
            .is_some_and(|settings| settings.answered(answer))
    }
}

impl Drop for Unanswered<'_> {
    fn drop(&mut self) {
        if let Some(settings) = self.settings.take() {
            settings.answered(Err("the connection closed before the shell answered".into()));
        }
    }
}

impl SettingsSource for ChannelSettings {
    fn load(&self) -> Map<String, Value> {
        self.lock().view.clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        {
            let mut state = self.lock();
            // The shell's own rules, applied to what this process shows: a
            // write it would refuse is refused here, and one it takes shows
            // as it will be stored.
            let shown = HostDocument::new(state.view.clone(), |_| Ok(()));
            settings_store::app_scope::update_for_app(&shown, &self.app_id, name, values.clone())?;
            state.view = shown.load();
            state.outbox.push((name.to_owned(), values));
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        super::broker::flush_settings();
        Ok(())
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_write_shows_at_once_and_an_older_view_cannot_undo_it() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        channel.received(object(json!({"locale": {"region": "DE"}})));
        let before = channel.revision();
        channel
            .update_section(
                "app_personal",
                object(json!({"so.tilo.monitor": {"confirm_kill": false}})),
            )
            .unwrap();
        assert!(channel.revision() > before);
        assert_eq!(
            channel.load()["app_personal"]["so.tilo.monitor"]["confirm_kill"],
            false
        );

        let sent = channel.take_outbox();
        assert_eq!(sent.len(), 1);
        // A view the shell took before the write arrives first.
        assert!(!channel.received(object(json!({"locale": {"region": "DE"}}))));
        assert_eq!(
            channel.load()["app_personal"]["so.tilo.monitor"]["confirm_kill"],
            false
        );
        // The answer to the write is the view that follows.
        let after = object(json!({
            "app_personal": {"so.tilo.monitor": {"confirm_kill": false}},
            "locale": {"region": "AT"}
        }));
        assert!(channel.answered(Ok(after.clone())));
        assert_eq!(channel.load(), after);
    }

    #[test]
    fn a_refused_write_falls_back_to_the_shells_view() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        let shell = object(json!({"app_device_local": {"so.tilo.monitor": {"x": 1}}}));
        channel.received(shell.clone());
        channel
            .update_section(
                "app_device_local",
                object(json!({"so.tilo.monitor": {"x": 2}})),
            )
            .unwrap();
        let _ = channel.take_outbox();
        assert!(channel.answered(Err("disk full".into())));
        assert_eq!(channel.load(), shell);
    }

    #[test]
    fn writes_the_shell_would_refuse_are_refused_here() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        for (section, values) in [
            ("app_personal", json!({"so.tilo.notes": {"x": 1}})),
            ("locale", json!({"region": "US"})),
        ] {
            let error = channel.update_section(section, object(values)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        }
        assert!(channel.take_outbox().is_empty());
        assert!(channel.load().is_empty());
    }
}
