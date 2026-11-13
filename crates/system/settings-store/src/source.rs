//! A settings document the host provides instead of the local file.
//!
//! A host that keeps settings somewhere of its own installs a [`SettingsSource`]; the load and
//! update functions then read and merge into it. A sandboxed app installs its channel to the
//! session; a browser page, which has no config directory, installs a [`HostDocument`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{Map, Value};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

#[derive(Debug)]
pub enum ConditionalUpdate {
    Superseded,
    Stored,
}

/// A display-only atomic update: the transaction and saved profiles live in
/// one device record. Expected sections include absence and malformed values,
/// so a delayed request cannot erase newly adopted display intent.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoked_by: Option<InvokingAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvokingAction {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
}

impl InvokingAction {
    #[must_use]
    pub fn action(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            invocation: None,
        }
    }
}

impl ChangeOrigin {
    pub const LOCAL_USER: &'static str = "local-user";

    #[must_use]
    pub fn action(action: impl Into<String>) -> Self {
        Self {
            action: Some(action.into()),
            principal: None,
            invoked_by: None,
        }
    }

    #[must_use]
    pub fn by(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    #[must_use]
    pub fn invoked_by(mut self, invoked_by: Option<InvokingAction>) -> Self {
        self.invoked_by = invoked_by;
        self
    }

    #[must_use]
    pub fn local(action: impl Into<String>) -> Self {
        Self::action(action).by(Self::LOCAL_USER)
    }
}

/// What a read-modify-write makes of the current section: the leaves to
/// merge into it. Called once, with the section as stored at that moment.
pub type SectionChange<'a> = dyn FnMut(&Map<String, Value>) -> Map<String, Value> + 'a;

pub trait SettingsSource: Send + Sync {
    fn load(&self) -> Map<String, Value>;

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()>;

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

    fn flush_pending(&self) {}

    fn take_deferred_failures(&self) -> Vec<DeferredFailure> {
        Vec::new()
    }

    /// Merge only if the current section still equals `expected`, under the
    /// same lock that protects persistence and external-write adoption.
    /// `Superseded` means a newer document replaced the caller's intent.
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

    fn compare_display_update(&self, _update: DisplayUpdate) -> std::io::Result<ConditionalUpdate> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic display persistence is unavailable",
        ))
    }

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
            ConditionalUpdate::Stored => Ok(()),
            ConditionalUpdate::Superseded => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "The settings transaction changed.",
            )),
        }
    }

    fn revision(&self) -> u64;

    /// Rings every time [`Self::revision`] changes, so a reader waiting for
    /// a change wakes at once instead of looking again and again.
    fn changes(&self) -> &ChangeSignal;

    fn store(&self) -> Option<&crate::Store> {
        None
    }

    fn dictionary_service(&self) -> Option<Arc<crate::personal_dictionary::Service>> {
        None
    }
}

macro_rules! shared_source {
    ($(fn $name:ident(&self $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)?;)*) => {
        impl<T: SettingsSource + ?Sized> SettingsSource for Arc<T> {
            $(
                fn $name(&self $(, $arg: $ty)*) $(-> $ret)? {
                    (**self).$name($($arg),*)
                }
            )*
        }
    };
}

shared_source! {
    fn load(&self) -> Map<String, Value>;
    fn store(&self) -> Option<&crate::Store>;
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()>;
    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()>;
    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()>;
    fn flush_pending(&self);
    fn take_deferred_failures(&self) -> Vec<DeferredFailure>;
    fn compare_update_section(
        &self,
        name: &str,
        expected: &Map<String, Value>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate>;
    fn compare_display_update(&self, update: DisplayUpdate) -> std::io::Result<ConditionalUpdate>;
    fn compare_display_transaction(
        &self,
        expected: Option<&Value>,
        transaction: Map<String, Value>,
    ) -> std::io::Result<()>;
    fn revision(&self) -> u64;
    fn changes(&self) -> &ChangeSignal;
    fn dictionary_service(&self) -> Option<Arc<crate::personal_dictionary::Service>>;
}

#[derive(Default)]
pub struct ChangeSignal {
    listeners: Mutex<Vec<std::sync::Weak<Listener>>>,
}

/// What a reader runs when the document changed: hand the news on, nothing
/// slow; it runs on the thread that made or noticed the change.
pub type Listener = dyn Fn() + Send + Sync;

impl ChangeSignal {
    pub fn listen(&self, listener: &Arc<Listener>) {
        let mut listeners = self.listeners.lock_recover();
        listeners.retain(|known| known.strong_count() > 0);
        listeners.push(Arc::downgrade(listener));
    }

    pub fn ring(&self) {
        let listeners: Vec<Arc<Listener>> = self
            .listeners
            .lock_recover()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        for listener in listeners {
            listener();
        }
    }
}

static SOURCE: RwLock<Option<Arc<dyn SettingsSource>>> = RwLock::new(None);

pub fn flush_pending_writes() {
    if let Some(source) = crate::source() {
        source.flush_pending();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredFailure {
    pub sections: Vec<String>,
    pub error: String,
}

static DEFERRED_FAILED: AtomicBool = AtomicBool::new(false);

/// A source records that a deferred write failed and was reverted (or, an
/// app's view of the session's settings, that the session refused it).
pub fn note_deferred_failure() {
    DEFERRED_FAILED.store(true, Ordering::Release);
}

#[must_use]
pub fn deferred_failure_pending() -> bool {
    DEFERRED_FAILED.load(Ordering::Acquire)
}

/// The failures of deferred writes since the last call, each once
/// ([`SettingsSource::take_deferred_failures`]).
#[must_use]
pub fn take_deferred_failures() -> Vec<DeferredFailure> {
    if !DEFERRED_FAILED.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    crate::source().map_or_else(Vec::new, |source| source.take_deferred_failures())
}

/// Make `source` the settings document of this process, or restore the
/// local profile with `None`. A host does this once, as it starts; a test
/// that needs a document of its own takes a [`scoped_source`], which no
/// other test sees.
pub fn install_source(source: Option<Arc<dyn SettingsSource>>) {
    *SOURCE.write_recover() = source;
}

pub fn installed_source() -> Option<Arc<dyn SettingsSource>> {
    if let Some(scoped) = SCOPED.with(|scoped| scoped.borrow().last().cloned()) {
        return scoped;
    }
    SOURCE.read_recover().clone()
}

thread_local! {
    static SCOPED: std::cell::RefCell<Vec<Option<Arc<dyn SettingsSource>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[must_use = "the scope ends when it is dropped"]
pub fn scoped_source(source: Option<Arc<dyn SettingsSource>>) -> SourceScope {
    SCOPED.with(|scoped| scoped.borrow_mut().push(source));
    SourceScope {
        _not_send: std::marker::PhantomData,
    }
}

pub struct SourceScope {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for SourceScope {
    fn drop(&mut self) {
        SCOPED.with(|scoped| {
            scoped.borrow_mut().pop();
        });
    }
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
        self.doc.lock_recover().clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        let mut values = Some(values);
        self.update_section_with(name, &ChangeOrigin::default(), &mut |_| {
            values.take().unwrap_or_default()
        })
    }

    fn update_section_with(
        &self,
        name: &str,
        _origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        let mut doc = self.doc.lock_recover();
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

    fn compare_update_section(
        &self,
        name: &str,
        expected: &Map<String, Value>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut doc = self.doc.lock_recover();
        let current = match doc.get(name) {
            Some(Value::Object(section)) => section,
            _ => &Map::new(),
        };
        if current != expected {
            return Ok(ConditionalUpdate::Superseded);
        }
        let mut next = doc.clone();
        merge_section(&mut next, name, values);
        (self.persist)(&next)?;
        *doc = next;
        self.revision.fetch_add(1, Ordering::AcqRel);
        drop(doc);
        self.changes.ring();
        Ok(ConditionalUpdate::Stored)
    }

    fn compare_display_update(&self, update: DisplayUpdate) -> std::io::Result<ConditionalUpdate> {
        let mut doc = self.doc.lock_recover();
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
    fn a_conditional_section_update_stores_only_over_what_it_read() {
        let doc = HostDocument::new(object(json!({"a": {"x": 1}})), |_| Ok(()));
        let read = object(json!({"x": 1}));
        assert!(matches!(
            doc.compare_update_section("a", &read, object(json!({"x": 2})))
                .unwrap(),
            ConditionalUpdate::Stored
        ));
        assert!(matches!(
            doc.compare_update_section("a", &read, object(json!({"x": 3})))
                .unwrap(),
            ConditionalUpdate::Superseded
        ));
        assert!(matches!(
            doc.compare_update_section("new", &Map::new(), object(json!({"y": 1})))
                .unwrap(),
            ConditionalUpdate::Stored
        ));
        assert_eq!(
            Value::Object(doc.load()),
            json!({"a": {"x": 2}, "new": {"y": 1}})
        );
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

    #[test]
    fn a_scoped_document_is_its_threads_alone_and_ends_with_its_scope() {
        let document = |mode: &str| -> Arc<dyn SettingsSource> {
            Arc::new(HostDocument::new(
                object(json!({"appearance": {"mode": mode}})),
                |_| Ok(()),
            ))
        };
        let mode = || -> Option<String> {
            let doc = installed_source()?.load();
            Some(doc.get("appearance")?.get("mode")?.as_str()?.to_owned())
        };
        let outer = scoped_source(Some(document("light")));
        assert_eq!(mode().as_deref(), Some("light"));
        let elsewhere = std::thread::spawn(move || mode().as_deref() != Some("light"))
            .join()
            .unwrap_or(false);
        assert!(elsewhere);
        {
            let _inner = scoped_source(Some(document("dark")));
            assert_eq!(mode().as_deref(), Some("dark"));
        }
        assert_eq!(mode().as_deref(), Some("light"));
        drop(outer);
        assert!(SCOPED.with(|scoped| scoped.borrow().is_empty()));
    }
}
