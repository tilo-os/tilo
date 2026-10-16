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
//! view taken before a write cannot undo it on screen; once the last write
//! is answered, the newest view the shell gave — its answer, or a later
//! change it reported meanwhile — is the one that shows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{Map, Value};
use settings_store::{HostDocument, SettingsSource};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// The settings source a launched app reads through its channel.
pub(crate) struct ChannelSettings {
    app_id: String,
    state: Mutex<State>,
    revision: AtomicU64,
    changes: settings_store::ChangeSignal,
}

#[derive(Default)]
struct State {
    connected: bool,
    identity_generation: u64,
    /// What this process reads: the shell's last view with this process's
    /// unanswered writes applied.
    view: Map<String, Value>,
    /// The newest view the shell gave.
    confirmed: Map<String, Value>,
    /// The shell's revision of [`Self::confirmed`]: answers and reported
    /// changes can arrive out of order, and an older one never replaces it.
    confirmed_revision: u64,
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
    if app_id.is_empty() {
        return;
    }
    let mut installed = INSTALLED.write_recover();
    if installed.is_some() {
        return;
    }
    let channel = Arc::new(ChannelSettings::new(app_id));
    if crate::host::environment().session_settings.is_none() {
        settings_store::install_source(Some(Arc::clone(&channel) as Arc<dyn SettingsSource>));
    }
    *installed = Some(channel);
}

/// Restore the process's own settings document.
#[cfg(test)]
pub(crate) fn uninstall() {
    settings_store::install_source(None);
    *INSTALLED.write_recover() = None;
}

/// The installed channel settings, for the broker worker to serve.
pub(crate) fn installed() -> Option<Arc<ChannelSettings>> {
    INSTALLED.read_recover().clone()
}

impl ChannelSettings {
    pub(crate) fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            state: Mutex::new(State::default()),
            revision: AtomicU64::new(1),
            changes: settings_store::ChangeSignal::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    /// Show `view` unless writes are still on their way. Returns whether
    /// what this process reads changed.
    fn show(&self, state: &mut State, view: Map<String, Value>) -> bool {
        if state.in_flight > 0 || !state.outbox.is_empty() || state.view == view {
            return false;
        }
        state.view = view;
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.changes.ring();
        true
    }

    /// A new link to the shell: its revisions start again.
    pub(crate) fn linked(&self) {
        let mut state = self.lock();
        state.confirmed_revision = 0;
        state.connected = true;
        state.identity_generation = crate::local_identity::generation();
    }

    pub(crate) fn disconnected(&self) {
        self.lock().connected = false;
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.changes.ring();
    }

    pub(crate) fn dictionary_snapshot(
        &self,
    ) -> std::io::Result<settings_store::personal_dictionary::Snapshot> {
        use settings_store::personal_dictionary::host::{Delivery, VIEW_KEY};
        let owner = crate::local_identity::bound()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "no authenticated personal dictionary",
                )
            })?
            .group_public_key;
        let state = self.lock();
        if !state.connected || state.identity_generation != crate::local_identity::generation() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "dictionary channel was revoked",
            ));
        }
        let delivery = state
            .view
            .get(settings_store::continuity::APP_DEVICE_LOCAL_SECTION)
            .and_then(|section| section.get(&self.app_id))
            .and_then(|entry| entry.get(VIEW_KEY))
            .cloned();
        drop(state);
        let delivery: Delivery = delivery
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "personal dictionary is unavailable",
                )
            })?;
        if delivery.owner != owner {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "dictionary belongs to another person",
            ));
        }
        Ok(delivery.snapshot)
    }

    pub(crate) fn dictionary_edit(
        &self,
        revision: u64,
        edit: settings_store::personal_dictionary::Edit,
    ) -> std::io::Result<()> {
        use settings_store::personal_dictionary::host::{Command, EDIT_KEY};
        let snapshot = self.dictionary_snapshot()?;
        if snapshot.revision != revision {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "dictionary changed; reload before editing",
            ));
        }
        let owner = crate::local_identity::bound()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "dictionary identity was revoked",
                )
            })?
            .group_public_key;
        if !matches!(edit, settings_store::personal_dictionary::Edit::Add(_))
            && !["so.tilo.settings", "so.tilo.word-processor"].contains(&self.app_id.as_str())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "this app can add words but cannot edit the dictionary",
            ));
        }
        let preview = snapshot.preview(&edit)?;
        let value = serde_json::to_value(Command {
            owner,
            revision,
            edit,
        })
        .map_err(std::io::Error::other)?;
        let values = Map::from_iter([(
            self.app_id.clone(),
            serde_json::Value::Object(Map::from_iter([(EDIT_KEY.to_owned(), value)])),
        )]);
        {
            let mut state = self.lock();
            if !state.connected || state.identity_generation != crate::local_identity::generation()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "dictionary channel revoked",
                ));
            }
            let delivery = state
                .view
                .get_mut(settings_store::continuity::APP_DEVICE_LOCAL_SECTION)
                .and_then(|section| section.get_mut(&self.app_id))
                .and_then(|entry| {
                    entry.get_mut(settings_store::personal_dictionary::host::VIEW_KEY)
                })
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "dictionary channel closed",
                    )
                })?;
            let current: settings_store::personal_dictionary::host::Delivery =
                serde_json::from_value(delivery.clone()).map_err(std::io::Error::other)?;
            if current.owner != owner || current.snapshot.revision != revision {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "dictionary changed",
                ));
            }
            *delivery = serde_json::to_value(settings_store::personal_dictionary::host::Delivery {
                owner,
                snapshot: preview,
            })
            .map_err(std::io::Error::other)?;
            state.outbox.push((
                settings_store::continuity::APP_DEVICE_LOCAL_SECTION.to_owned(),
                values,
            ));
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        self.changes.ring();
        super::broker::flush_settings();
        Ok(())
    }

    /// Keep `view`, the shell's at `revision`, unless a newer one is kept.
    fn confirm(state: &mut State, revision: u64, view: Map<String, Value>) {
        if revision >= state.confirmed_revision {
            state.confirmed = view;
            state.confirmed_revision = revision;
        }
    }

    /// The shell's view at `revision` arrived or changed. Returns whether
    /// what this process reads changed.
    pub(crate) fn received(&self, revision: u64, view: Map<String, Value>) -> bool {
        let mut state = self.lock();
        Self::confirm(&mut state, revision, view);
        let shown = state.confirmed.clone();
        self.show(&mut state, shown)
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
    pub(crate) fn answered(&self, answer: Result<(u64, Map<String, Value>), String>) -> bool {
        let mut state = self.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        match answer {
            Ok((revision, view)) => Self::confirm(&mut state, revision, view),
            Err(error) => {
                tracing::warn!(app = %self.app_id, %error, "settings: the shell refused a write");
            }
        }
        let shown = state.confirmed.clone();
        self.show(&mut state, shown)
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
    pub(crate) fn answer(mut self, answer: Result<(u64, Map<String, Value>), String>) -> bool {
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

fn settings_document(view: &Map<String, Value>) -> Map<String, Value> {
    let mut document = view.clone();
    if let Some(entries) = document
        .get_mut(settings_store::continuity::APP_DEVICE_LOCAL_SECTION)
        .and_then(Value::as_object_mut)
    {
        for entry in entries.values_mut().filter_map(Value::as_object_mut) {
            entry.remove(settings_store::personal_dictionary::host::VIEW_KEY);
        }
    }
    document
}

impl SettingsSource for ChannelSettings {
    fn load(&self) -> Map<String, Value> {
        settings_document(&self.lock().view)
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        {
            let mut state = self.lock();
            // The shell's own rules, applied to what this process shows: a
            // write it would refuse is refused here, and one it takes shows
            // as it will be stored.
            let shown = HostDocument::new(settings_document(&state.view), |_| Ok(()));
            settings_store::app_scope::update_for_app(&shown, &self.app_id, name, values.clone())?;
            let delivery = state
                .view
                .get(settings_store::continuity::APP_DEVICE_LOCAL_SECTION)
                .and_then(|entries| entries.get(&self.app_id))
                .and_then(|entry| entry.get(settings_store::personal_dictionary::host::VIEW_KEY))
                .cloned();
            state.view = shown.load();
            if let Some(delivery) = delivery {
                let section = state
                    .view
                    .entry(settings_store::continuity::APP_DEVICE_LOCAL_SECTION.to_owned())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(section) = section.as_object_mut() {
                    let entry = section
                        .entry(self.app_id.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                    if let Some(entry) = entry.as_object_mut() {
                        entry.insert(
                            settings_store::personal_dictionary::host::VIEW_KEY.to_owned(),
                            delivery,
                        );
                    }
                }
            }
            state.outbox.push((name.to_owned(), values));
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        self.changes.ring();
        super::broker::flush_settings();
        Ok(())
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn changes(&self) -> &settings_store::ChangeSignal {
        &self.changes
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
    fn dictionary_delivery_preserves_settings_and_revokes_cached_hints() {
        use settings_store::personal_dictionary::{
            Edit, Snapshot, Word, host::Delivery, host::VIEW_KEY,
        };
        let _guard = tilo_test_env::lock();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::local_identity::install_source(None);
            }
        }
        let _restore = Restore;
        let identity = |key| {
            Arc::new(crate::local_identity::FixedIdentity(
                crate::local_identity::LocalIdentity {
                    group_public_key: [key; 32],
                    display_name: String::new(),
                    avatar_color: [0; 3],
                },
            ))
        };
        crate::local_identity::install_source(Some(identity(1)));
        let channel = ChannelSettings::new("so.tilo.settings");
        channel.linked();
        let snapshot = Snapshot {
            revision: 1,
            words: vec![Word::new("Tilo", None).unwrap()],
        };
        let delivery = serde_json::to_value(Delivery {
            owner: [1; 32],
            snapshot: snapshot.clone(),
        })
        .unwrap();
        channel.received(1, object(json!({"app_device_local": {"so.tilo.settings": { VIEW_KEY: delivery, "local_setting": true }}})));
        assert_eq!(channel.dictionary_snapshot().unwrap(), snapshot);
        assert!(
            channel.load()["app_device_local"]["so.tilo.settings"]
                .get(VIEW_KEY)
                .is_none()
        );
        channel
            .update_section(
                "app_device_local",
                object(json!({"so.tilo.settings": {"local_setting": false}})),
            )
            .unwrap();
        assert_eq!(channel.dictionary_snapshot().unwrap(), snapshot);
        assert!(
            channel.take_outbox()[0].1["so.tilo.settings"]
                .get(VIEW_KEY)
                .is_none()
        );
        channel
            .dictionary_edit(
                1,
                Edit::Replace {
                    old: Word::new("Tilo", None).unwrap(),
                    new: Word::new("TiLo", None).unwrap(),
                },
            )
            .unwrap();
        assert_eq!(
            channel
                .dictionary_snapshot()
                .unwrap()
                .hints("en")
                .collect::<Vec<_>>(),
            ["TiLo"]
        );
        assert!(channel.dictionary_edit(1, Edit::Reset).is_err());
        channel.disconnected();
        assert!(channel.dictionary_snapshot().is_err());
        channel.linked();
        crate::local_identity::install_source(Some(identity(2)));
        assert!(channel.dictionary_snapshot().is_err());
        assert!(channel.dictionary_edit(2, Edit::Reset).is_err());
        channel.linked();
        assert!(
            channel.dictionary_snapshot().is_err(),
            "old owner's cached delivery cannot survive account switching"
        );
        crate::local_identity::install_source(Some(Arc::new(
            crate::local_identity::NoLocalIdentity,
        )));
        assert!(channel.dictionary_snapshot().is_err());
    }

    #[test]
    fn a_write_shows_at_once_and_an_older_view_cannot_undo_it() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        channel.received(1, object(json!({"locale": {"region": "DE"}})));
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
        assert!(!channel.received(1, object(json!({"locale": {"region": "DE"}}))));
        assert_eq!(
            channel.load()["app_personal"]["so.tilo.monitor"]["confirm_kill"],
            false
        );
        // The answer to the write is the view that follows.
        let after = object(json!({
            "app_personal": {"so.tilo.monitor": {"confirm_kill": false}},
            "locale": {"region": "AT"}
        }));
        assert!(channel.answered(Ok((2, after.clone()))));
        assert_eq!(channel.load(), after);
    }

    #[test]
    fn a_change_reported_before_the_answer_is_not_undone_by_it() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        channel.received(
            1,
            object(json!({"app_personal": {"so.tilo.monitor": {"x": 1}}})),
        );
        channel
            .update_section("app_personal", object(json!({"so.tilo.monitor": {"x": 2}})))
            .unwrap();
        let _ = channel.take_outbox();
        // Another device's change, reported while the answer is on its way.
        let later = object(json!({"app_personal": {"so.tilo.monitor": {"x": 3}}}));
        assert!(
            !channel.received(3, later.clone()),
            "held back for the write"
        );
        // The answer, the shell's view right after this write, arrives last.
        assert!(channel.answered(Ok((
            2,
            object(json!({"app_personal": {"so.tilo.monitor": {"x": 2}}}))
        ))));
        assert_eq!(channel.load(), later, "the newest view shows");
    }

    #[test]
    fn a_refused_write_falls_back_to_the_shells_view() {
        let channel = ChannelSettings::new("so.tilo.monitor");
        let shell = object(json!({"app_device_local": {"so.tilo.monitor": {"x": 1}}}));
        channel.received(1, shell.clone());
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

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn replace_for_test(
    channel: Option<Arc<ChannelSettings>>,
) -> Option<Arc<ChannelSettings>> {
    std::mem::replace(&mut *INSTALLED.write_recover(), channel)
}
