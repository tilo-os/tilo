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

/// Where the settings document lives when it is not the local file.
pub trait SettingsSource: Send + Sync {
    /// The whole document. Forgiving like the file: never fails.
    fn load(&self) -> Map<String, Value>;

    /// Merge `values` into section `name` as [`crate::update_section`] does.
    ///
    /// # Errors
    /// The host could not keep the change; the document is unchanged.
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()>;

    /// Changes whenever the document does, so readers can cache it.
    fn revision(&self) -> u64;
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

    #[allow(
        clippy::significant_drop_tightening,
        reason = "held across persist so concurrent merges apply one after another"
    )]
    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        let mut doc = self.doc.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = doc.clone();
        merge_section(&mut next, name, values);
        (self.persist)(&next)?;
        *doc = next;
        self.revision.fetch_add(1, Ordering::AcqRel);
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
