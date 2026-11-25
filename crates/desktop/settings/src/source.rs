//! The settings a process reads: the source its host installed.
//!
//! The session installs its settings as resources of the workspace
//! (`settings_sections::workspace::WorkspaceSettings`); a sandboxed app installs its channel to the
//! session; a service the session started, and a browser page, install a [`HostDocument`] of
//! what their host gives them. A process with none installed reads every setting's default.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{Map, Value};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

#[derive(Debug)]
pub enum ConditionalUpdate {
    Superseded,
    Stored,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

impl ChangeOrigin {
    pub const LOCAL_USER: &'static str = "local-user";

    #[must_use]
    pub fn action(action: impl Into<String>) -> Self {
        Self {
            action: Some(action.into()),
            actor: None,
        }
    }

    #[must_use]
    pub fn by(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
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

    /// Merge each of `sections` as one change made by `origin`: one step history takes back
    /// together, a choice spanning sections (the model agents answer with) included.
    ///
    /// # Errors
    /// As [`Self::update_section`].
    fn update_sections(
        &self,
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        for (name, values) in sections {
            self.update_section_by(&name, values, origin)?;
        }
        Ok(())
    }

    /// [`Self::update_sections`], only if each of `expected` (a section, and what it held when
    /// the caller read it) still holds what it did. `Superseded` means a newer change replaced
    /// what the caller read.
    ///
    /// # Errors
    /// As [`Self::update_section`].
    fn compare_update_sections(
        &self,
        _expected: &[(&str, Map<String, Value>)],
        _sections: Vec<(String, Map<String, Value>)>,
        _origin: &ChangeOrigin,
    ) -> std::io::Result<ConditionalUpdate> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "conditional settings persistence is unavailable",
        ))
    }

    /// Take back (`undo`) or bring back the last step any of `invokers` (actors, as a change
    /// names them) took on section `section`, of the app `app` where the section keeps each
    /// app's preferences apart. Whether there was one.
    ///
    /// # Errors
    /// The settings' history is not kept here, or the step was refused.
    fn step(
        &self,
        _section: &str,
        _app: Option<&str>,
        _undo: bool,
        _invokers: &[String],
    ) -> std::io::Result<bool> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the settings' history is kept by the session",
        ))
    }

    /// The changes of section `section`, oldest first, each as the stamp it carries in the
    /// settings' history (or will, once written): which setter set what, and who invoked it.
    ///
    /// # Errors
    /// The settings' history is not kept here.
    fn stamps(&self, _section: &str) -> std::io::Result<Vec<tilo_actions::stamp::Stamp>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the settings' history is kept by the session",
        ))
    }

    fn flush_pending(&self) {}

    fn take_deferred_failures(&self) -> Vec<DeferredFailure> {
        Vec::new()
    }

    fn revision(&self) -> u64;

    /// Rings every time [`Self::revision`] changes, so a reader waiting for
    /// a change wakes at once instead of looking again and again.
    fn changes(&self) -> &ChangeSignal;
}

/// [`SettingsSource::compare_update_sections`] of one section: merge `values` into `name` only
/// if it still holds `expected`.
///
/// # Errors
/// As [`SettingsSource::update_section`].
pub fn compare_update_section(
    source: &dyn SettingsSource,
    name: &str,
    expected: &Map<String, Value>,
    values: Map<String, Value>,
) -> std::io::Result<ConditionalUpdate> {
    source.compare_update_sections(
        &[(name, expected.clone())],
        vec![(name.to_owned(), values)],
        &ChangeOrigin::default(),
    )
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
    fn update_sections(
        &self,
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()>;
    fn compare_update_sections(
        &self,
        expected: &[(&str, Map<String, Value>)],
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<ConditionalUpdate>;
    fn step(
        &self,
        section: &str,
        app: Option<&str>,
        undo: bool,
        invokers: &[String],
    ) -> std::io::Result<bool>;
    fn stamps(&self, section: &str) -> std::io::Result<Vec<tilo_actions::stamp::Stamp>>;
    fn flush_pending(&self);
    fn take_deferred_failures(&self) -> Vec<DeferredFailure>;
    fn revision(&self) -> u64;
    fn changes(&self) -> &ChangeSignal;
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
    INSTALLED.fetch_add(1, Ordering::AcqRel);
}

/// How many settings documents this process has installed in place of others.
static INSTALLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The version of `source` as this process reads it: the document's own revision, told apart
/// from every document installed in another's place before it, so a reader that caches what it
/// read by version reads a replaced document again even where the two number alike. A reader
/// told only the document's own revision takes a document put in place of another for the one
/// it replaced, and keeps reading what the old one said.
#[must_use]
pub fn installed_version(source: &dyn SettingsSource) -> u64 {
    let mut folded = 0xcbf2_9ce4_8422_2325u64 ^ INSTALLED.load(Ordering::Acquire);
    for byte in source.revision().to_le_bytes() {
        folded = (folded ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    folded
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
    // A document put in front of the one this thread read is a new version of it, as
    // [`install_source`]'s is.
    INSTALLED.fetch_add(1, Ordering::AcqRel);
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
        INSTALLED.fetch_add(1, Ordering::AcqRel);
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

impl HostDocument {
    /// Merge what `change` makes of the document, under its lock, persisting it first; `None`
    /// from `change` leaves it as it is.
    fn change(
        &self,
        change: impl FnOnce(&Map<String, Value>) -> Option<Vec<(String, Map<String, Value>)>>,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut doc = self.doc.lock_recover();
        let Some(sections) = change(&doc) else {
            return Ok(ConditionalUpdate::Superseded);
        };
        let mut next = doc.clone();
        for (name, values) in sections {
            merge_section(&mut next, &name, values);
        }
        (self.persist)(&next)?;
        *doc = next;
        self.revision.fetch_add(1, Ordering::AcqRel);
        drop(doc);
        self.changes.ring();
        Ok(ConditionalUpdate::Stored)
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
        self.change(|doc| {
            let current = match doc.get(name) {
                Some(Value::Object(section)) => section.clone(),
                _ => Map::new(),
            };
            Some(vec![(name.to_owned(), change(&current))])
        })
        .map(drop)
    }

    fn update_sections(
        &self,
        sections: Vec<(String, Map<String, Value>)>,
        _origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        self.change(|_| Some(sections)).map(drop)
    }

    fn compare_update_sections(
        &self,
        expected: &[(&str, Map<String, Value>)],
        sections: Vec<(String, Map<String, Value>)>,
        _origin: &ChangeOrigin,
    ) -> std::io::Result<ConditionalUpdate> {
        self.change(|doc| {
            expected
                .iter()
                .all(|(name, expected)| match doc.get(*name) {
                    Some(Value::Object(section)) => section == expected,
                    _ => expected.is_empty(),
                })
                .then_some(sections)
        })
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

    tilo_test_env::scenario! {
        fn a_host_document_stores_what_its_persist_accepts;

        phase host_document_merges_persists_then_publishes;
        {
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

        phase a_conditional_section_update_stores_only_over_what_it_read;
        {
            let doc = HostDocument::new(object(json!({"a": {"x": 1}})), |_| Ok(()));
            let read = object(json!({"x": 1}));
            assert!(matches!(
                compare_update_section(&doc, "a", &read, object(json!({"x": 2}))).unwrap(),
                ConditionalUpdate::Stored
            ));
            assert!(matches!(
                compare_update_section(&doc, "a", &read, object(json!({"x": 3}))).unwrap(),
                ConditionalUpdate::Superseded
            ));
            assert!(matches!(
                compare_update_section(&doc, "new", &Map::new(), object(json!({"y": 1})))
                    .unwrap(),
                ConditionalUpdate::Stored
            ));
            assert_eq!(
                Value::Object(doc.load()),
                json!({"a": {"x": 2}, "new": {"y": 1}})
            );
        }

        phase a_refused_persist_leaves_the_document_unchanged;
        {
            let doc = HostDocument::new(object(json!({"a": {"x": 1}})), |_| {
                Err(std::io::Error::other("quota"))
            });
            let before = doc.revision();
            assert!(doc.update_section("a", object(json!({"x": 2}))).is_err());
            assert_eq!(Value::Object(doc.load()), json!({"a": {"x": 1}}));
            assert_eq!(doc.revision(), before);
        }
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
