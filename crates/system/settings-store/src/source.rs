//! A settings document the host provides instead of the local file.
//!
//! A host that keeps settings somewhere of its own installs a
//! [`SettingsSource`]; [`crate::load`], [`crate::load_section`] and
//! [`crate::update_section`] then read and merge into it. The shell installs
//! its session's records ([`crate::SessionSettings`]); a browser page, which
//! has no config directory, installs a [`HostDocument`]. Every other process
//! reads and writes `settings.json`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use serde_json::{Map, Value};

/// Receipt of a storage operation performed under its canonical lock.
#[derive(Debug)]
pub enum ConditionalUpdate {
    /// A comparison failed; no requested fields were changed.
    Superseded,
    /// Durable records and their compatibility copy accepted the change.
    Stored,
    /// Durable records accepted it, but repairing the compatibility copy failed.
    StoredWithCopyRepair(std::io::Error),
}

impl ConditionalUpdate {
    pub(crate) fn into_sync_result(self) -> std::io::Result<()> {
        match self {
            Self::Stored => Ok(()),
            Self::Superseded => Err(std::io::Error::other("settings intent was superseded")),
            Self::StoredWithCopyRepair(error) => Err(error),
        }
    }
}

/// A display-only atomic update: the transaction and saved profiles live in
/// one device record. Expected sections include absence and malformed values,
/// so a delayed request cannot erase newly adopted display intent.
#[derive(Clone)]
pub struct DisplayUpdate {
    transaction_before: Option<Value>,
    profiles_before: Option<Value>,
    transaction: Map<String, Value>,
    profiles: Option<Map<String, Value>>,
}

impl DisplayUpdate {
    pub fn new(
        document: &Map<String, Value>,
        transaction: Map<String, Value>,
        profiles: Option<Map<String, Value>>,
    ) -> Self {
        Self {
            transaction_before: document.get("display_transaction").cloned(),
            profiles_before: document.get("display_profiles").cloned(),
            transaction,
            profiles,
        }
    }

    pub(crate) fn matches(&self, document: &Map<String, Value>) -> bool {
        document.get("display_transaction") == self.transaction_before.as_ref()
            && document.get("display_profiles") == self.profiles_before.as_ref()
    }

    pub(crate) fn sections(self) -> Vec<(&'static str, Map<String, Value>)> {
        let mut sections = vec![("display_transaction", self.transaction)];
        if let Some(profiles) = self.profiles {
            sections.push(("display_profiles", profiles));
        }
        sections
    }
}

/// Who and what made a settings change: the action (or shell entry point,
/// such as a fixed chord) that caused it, and the principal that invoked it.
///
/// A source that keeps a history ([`crate::SessionSettings`]) records it
/// beside each changed leaf, so "who changed this" has an answer and a later
/// system undo has its input. Both fields are optional: a write another
/// process made to the copy arrives with no origin at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeOrigin {
    /// The action id (`appearance.theme.dark`) or shell entry point
    /// (`fixed-toggle-mouse-keys`) that made the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Who invoked it: [`ChangeOrigin::LOCAL_USER`] for the person at this
    /// device's own input, else the collaborator's or agent's principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

impl ChangeOrigin {
    /// The person at this device's keyboard, pointer or touch screen.
    pub const LOCAL_USER: &'static str = "local-user";

    /// A change `action` made, with no principal known yet.
    #[must_use]
    pub fn action(action: impl Into<String>) -> Self {
        Self {
            action: Some(action.into()),
            principal: None,
        }
    }

    /// This origin, invoked by `principal`.
    #[must_use]
    pub fn by(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// `action`, invoked by the person at this device's own input.
    #[must_use]
    pub fn local(action: impl Into<String>) -> Self {
        Self::action(action).by(Self::LOCAL_USER)
    }
}

/// What a read-modify-write makes of the current section: the leaves to
/// merge into it. Called once, with the section as stored at that moment.
pub type SectionChange<'a> = dyn FnMut(&Map<String, Value>) -> Map<String, Value> + 'a;

/// Where the settings document lives when it is not the local file.
pub trait SettingsSource: Send + Sync {
    /// The whole document. Forgiving like the file: never fails.
    fn load(&self) -> Map<String, Value>;

    /// Merge `values` into section `name` as [`crate::update_section`] does.
    ///
    /// # Errors
    /// The host could not keep the change; the document is unchanged.
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()>;

    /// [`Self::update_section`], recording `origin` with every leaf it
    /// changes. A source without a history ignores the origin.
    ///
    /// # Errors
    /// As [`Self::update_section`].
    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        let _ = origin;
        self.update_section(name, values)
    }

    /// Read section `name` and merge what `change` makes of it, recording
    /// `origin`, as one step no other write to this source lands inside.
    /// This is the read-modify-write behind [`crate::Key::update`]: a toggle
    /// pressed twice in quick succession flips twice.
    ///
    /// The default reads and then writes, which is atomic only where the
    /// source's writer serialises them itself (an app's settings channel,
    /// whose host applies writes in order); a source that owns its document
    /// overrides it to hold its lock across both.
    ///
    /// # Errors
    /// As [`Self::update_section`].
    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        let current = match self.load().remove(name) {
            Some(Value::Object(section)) => section,
            _ => Map::new(),
        };
        self.update_section_by(name, change(&current), origin)
    }

    /// Preserve the storage owner's explicit durable/copy-repair receipt.
    /// Ordinary sources report only successful writes; errors stay errors.
    ///
    /// # Errors
    /// The source could not accept the change.
    fn update_section_report(
        &self,
        name: &str,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.update_section(name, values)
            .map(|()| ConditionalUpdate::Stored)
    }

    /// Merge only if the current section still equals `expected`, under the
    /// same lock that protects persistence and external-write adoption.
    /// `Superseded` means a newer document replaced the caller's intent.
    ///
    /// # Errors
    /// Storage failed, or this source cannot provide atomic comparison.
    fn compare_update_section(
        &self,
        _name: &str,
        _expected: &Map<String, Value>,
        _values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "conditional settings persistence is unavailable",
        ))
    }

    /// Atomically compare and merge display transaction/profile intent in
    /// its single device record, after adopting external-copy changes.
    ///
    /// # Errors
    /// Storage failed, or the source cannot provide atomic display updates.
    fn compare_display_update(&self, _update: DisplayUpdate) -> std::io::Result<ConditionalUpdate> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic display persistence is unavailable",
        ))
    }

    /// Compare a host-owned display transaction, including an absent section.
    /// Main's atomic display update also protects the observed profile state.
    ///
    /// # Errors
    /// Refuses stale intent, unsupported sources or failed durable persistence.
    fn compare_display_transaction(
        &self,
        expected: Option<&Value>,
        transaction: Map<String, Value>,
    ) -> std::io::Result<()> {
        let document = self.load();
        if document.get("display_transaction") != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "The settings transaction changed.",
            ));
        }
        match self.compare_display_update(DisplayUpdate::new(&document, transaction, None))? {
            ConditionalUpdate::Stored | ConditionalUpdate::StoredWithCopyRepair(_) => Ok(()),
            ConditionalUpdate::Superseded => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "The settings transaction changed.",
            )),
        }
    }

    /// Changes whenever the document does, so readers can cache it.
    fn revision(&self) -> u64;

    /// Rings every time [`Self::revision`] changes, so a reader waiting for
    /// a change wakes at once instead of looking again and again.
    fn changes(&self) -> &ChangeSignal;

    /// Host-owned personal vocabulary. App/profile sources provide no host
    /// authority; the authenticated settings capability projects scoped reads.
    fn dictionary_service(&self) -> Option<Arc<crate::personal_dictionary::Service>> {
        None
    }

    /// Verified original profile identity permitted to migrate unscoped words.
    fn dictionary_legacy_owner(&self) -> Option<[u8; 32]> {
        None
    }
}

/// Wakes the readers of a [`SettingsSource`] when its document changes.
#[derive(Default)]
pub struct ChangeSignal {
    listeners: Mutex<Vec<std::sync::Weak<Listener>>>,
}

/// What a reader runs when the document changed: hand the news on, nothing
/// slow; it runs on the thread that made or noticed the change.
pub type Listener = dyn Fn() + Send + Sync;

impl ChangeSignal {
    /// Call `listener` after every change for as long as the caller keeps
    /// it.
    pub fn listen(&self, listener: &Arc<Listener>) {
        let mut listeners = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        listeners.retain(|known| known.strong_count() > 0);
        listeners.push(Arc::downgrade(listener));
    }

    /// The document changed: wake every reader still listening.
    pub fn ring(&self) {
        let listeners: Vec<Arc<Listener>> = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        for listener in listeners {
            listener();
        }
    }
}

static SOURCE: RwLock<Option<Arc<dyn SettingsSource>>> = RwLock::new(None);

/// Make `source` the settings document of this process, or restore the
/// local file with `None`.
pub fn install_source(source: Option<Arc<dyn SettingsSource>>) {
    *SOURCE.write().unwrap_or_else(PoisonError::into_inner) = source;
}

/// The installed source, if the host provides the document.
pub fn installed_source() -> Option<Arc<dyn SettingsSource>> {
    SOURCE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Merge `values` into section `name` of `doc`, keeping every other section
/// and every key `values` does not mention. A section of the wrong type is
/// replaced: there is nothing to preserve in a value that cannot be read.
pub fn merge_section(doc: &mut Map<String, Value>, name: &str, values: Map<String, Value>) {
    let entry = doc
        .entry(name.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in values {
            obj.insert(k, v);
        }
    }
}

type Persist = dyn Fn(&Map<String, Value>) -> std::io::Result<()> + Send + Sync;

/// A document held in memory and handed to the host on every change.
///
/// `persist` receives the whole merged document before it becomes current;
/// if it fails, the change is refused and readers keep the previous one.
pub struct HostDocument {
    doc: Mutex<Map<String, Value>>,
    revision: AtomicU64,
    persist: Box<Persist>,
    changes: ChangeSignal,
}

impl HostDocument {
    pub fn new(
        initial: Map<String, Value>,
        persist: impl Fn(&Map<String, Value>) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            doc: Mutex::new(initial),
            revision: AtomicU64::new(1),
            persist: Box::new(persist),
            changes: ChangeSignal::default(),
        }
    }
}

impl SettingsSource for HostDocument {
    fn load(&self) -> Map<String, Value> {
        self.doc
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        let mut values = Some(values);
        self.update_section_with(name, &ChangeOrigin::default(), &mut |_| {
            values.take().unwrap_or_default()
        })
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "held across persist so concurrent merges apply one after another"
    )]
    fn update_section_with(
        &self,
        name: &str,
        _origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        let mut doc = self.doc.lock().unwrap_or_else(PoisonError::into_inner);
        let current = match doc.get(name) {
            Some(Value::Object(section)) => section.clone(),
            _ => Map::new(),
        };
        let values = change(&current);
        let mut next = doc.clone();
        merge_section(&mut next, name, values);
        (self.persist)(&next)?;
        *doc = next;
        self.revision.fetch_add(1, Ordering::AcqRel);
        drop(doc);
        self.changes.ring();
        Ok(())
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the document lock covers comparison, persistence and revision publication"
    )]
    fn compare_display_update(&self, update: DisplayUpdate) -> std::io::Result<ConditionalUpdate> {
        let mut doc = self.doc.lock().unwrap_or_else(PoisonError::into_inner);
        if !update.matches(&doc) {
            return Ok(ConditionalUpdate::Superseded);
        }
        let mut next = doc.clone();
        for (section, values) in update.sections() {
            merge_section(&mut next, section, values);
        }
        (self.persist)(&next)?;
        *doc = next;
        self.revision.fetch_add(1, Ordering::AcqRel);
        drop(doc);
        self.changes.ring();
        Ok(ConditionalUpdate::Stored)
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn changes(&self) -> &ChangeSignal {
        &self.changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn host_document_merges_persists_then_publishes() {
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&persisted);
        let doc = HostDocument::new(
            object(json!({"region": {"locale": "de-DE"}, "power": 3})),
            move |doc| {
                sink.lock().unwrap().push(Value::Object(doc.clone()));
                Ok(())
            },
        );
        let before = doc.revision();
        doc.update_section("power", object(json!({"idle": 5})))
            .unwrap();
        doc.update_section("region", object(json!({"units": "metric"})))
            .unwrap();
        let expected =
            json!({"region": {"locale": "de-DE", "units": "metric"}, "power": {"idle": 5}});
        assert_eq!(Value::Object(doc.load()), expected);
        assert_eq!(persisted.lock().unwrap().last(), Some(&expected));
        assert_eq!(doc.revision(), before + 2);
    }

    #[test]
    fn a_refused_persist_leaves_the_document_unchanged() {
        let doc = HostDocument::new(object(json!({"a": {"x": 1}})), |_| {
            Err(std::io::Error::other("quota"))
        });
        let before = doc.revision();
        assert!(doc.update_section("a", object(json!({"x": 2}))).is_err());
        assert_eq!(Value::Object(doc.load()), json!({"a": {"x": 1}}));
        assert_eq!(doc.revision(), before);
    }
}
