//! Typed settings: one definition per setting.
//!
//! A [`Key`] names one leaf of the settings document — its section, its
//! name, its value type and its default — and is the way production code
//! reads and writes a setting. A misspelt section or a value of the wrong
//! type is then a compile error instead of a leaf no reader looks at, and a
//! chord, a Quick Settings tile and an action that change the same setting
//! go through the same definition.
//!
//! Keys write leaf by leaf, never a whole section: a section is shared by
//! writers that ship at different times, and each must keep the leaves it
//! does not know (see the [crate docs](crate)). The persisted shape is
//! exactly what the untyped writers stored, so readers on either side of
//! this change agree.
//!
//! Reads are forgiving like every read in this crate: an absent leaf, or one
//! whose value does not have the key's type, reads as the default.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::ChangeOrigin;

/// One setting: its section, leaf name, value type and default.
pub struct Key<T: 'static> {
    section: &'static str,
    name: &'static str,
    default: fn() -> T,
}

impl<T> Key<T> {
    /// The setting `section.name`, reading as `default()` when unset.
    #[must_use]
    pub const fn new(section: &'static str, name: &'static str, default: fn() -> T) -> Self {
        Self {
            section,
            name,
            default,
        }
    }

    /// The top-level section the setting lives in.
    #[must_use]
    pub const fn section(&self) -> &'static str {
        self.section
    }

    /// The leaf name inside its section.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The value an unset or unreadable setting reads as.
    #[must_use]
    pub fn default_value(&self) -> T {
        (self.default)()
    }
}

impl<T: Serialize + DeserializeOwned> Key<T> {
    /// The value in `section`, an already loaded section object.
    #[must_use]
    pub fn from_section(&self, section: &Map<String, Value>) -> T {
        section
            .get(self.name)
            .and_then(|value| T::deserialize(value).ok())
            .unwrap_or_else(|| self.default_value())
    }

    /// The value in `document`, an already loaded settings document.
    #[must_use]
    pub fn from_document(&self, document: &Map<String, Value>) -> T {
        match document.get(self.section) {
            Some(Value::Object(section)) => self.from_section(section),
            _ => self.default_value(),
        }
    }

    /// The current value, from the installed source or the settings file.
    #[must_use]
    pub fn get(&self) -> T {
        self.from_section(&crate::load_section(self.section))
    }

    /// The value in the settings file at `path`.
    #[must_use]
    pub fn get_at(&self, path: &Path) -> T {
        self.from_section(&crate::load_section_at(path, self.section))
    }

    /// Store `value`, leaving every other leaf alone.
    ///
    /// # Errors
    /// The value cannot be serialised, or the write fails as
    /// [`crate::update_section`] does.
    pub fn set(&self, value: T) -> std::io::Result<()> {
        self.set_by(value, &ChangeOrigin::default())
    }

    /// [`Self::set`], recording who and what made the change.
    ///
    /// # Errors
    /// As [`Self::set`].
    pub fn set_by(&self, value: T, origin: &ChangeOrigin) -> std::io::Result<()> {
        crate::update_section_by(self.section, self.leaf(value)?, origin)
    }

    /// [`Self::set`] on the settings file at `path`.
    ///
    /// # Errors
    /// As [`crate::update_section_at`].
    pub fn set_at(&self, path: &Path, value: T) -> std::io::Result<()> {
        crate::update_section_at(path, self.section, self.leaf(value)?)
    }

    /// Replace the value with what `change` makes of it, as one step no
    /// other write lands inside: two toggles in quick succession flip twice.
    /// Returns the stored value.
    ///
    /// # Errors
    /// As [`Self::set`].
    pub fn update(&self, change: impl FnOnce(T) -> T) -> std::io::Result<T> {
        self.update_by(&ChangeOrigin::default(), change)
    }

    /// [`Self::update`], recording who and what made the change.
    ///
    /// # Errors
    /// As [`Self::set`].
    pub fn update_by(
        &self,
        origin: &ChangeOrigin,
        change: impl FnOnce(T) -> T,
    ) -> std::io::Result<T> {
        let mut change = Some(change);
        let mut stored = None;
        crate::update_section_with(self.section, origin, &mut |section| {
            self.step(section, &mut change, &mut stored)
        })?;
        Self::stored(stored)
    }

    /// [`Self::update`] on the settings file at `path`, under its lock.
    ///
    /// # Errors
    /// As [`crate::update_section_with_at`].
    pub fn update_at(&self, path: &Path, change: impl FnOnce(T) -> T) -> std::io::Result<T> {
        let mut change = Some(change);
        let mut stored = None;
        crate::update_section_with_at(path, self.section, &mut |section| {
            self.step(section, &mut change, &mut stored)
        })?;
        Self::stored(stored)
    }

    /// One read-modify-write step: apply `change` (once) to the value in
    /// `section`, keep the result in `stored`, and return the leaf to merge.
    fn step(
        &self,
        section: &Map<String, Value>,
        change: &mut Option<impl FnOnce(T) -> T>,
        stored: &mut Option<std::io::Result<T>>,
    ) -> Map<String, Value> {
        let Some(change) = change.take() else {
            return Map::new();
        };
        let next = change(self.from_section(section));
        match self.leaf(&next) {
            Ok(leaf) => {
                *stored = Some(Ok(next));
                leaf
            }
            Err(error) => {
                *stored = Some(Err(error));
                Map::new()
            }
        }
    }

    /// What a step stored, once its write landed.
    fn stored(stored: Option<std::io::Result<T>>) -> std::io::Result<T> {
        stored.unwrap_or_else(|| Err(std::io::Error::other("the settings change was not applied")))
    }

    /// `{name: value}`, the one leaf this key writes.
    fn leaf(&self, value: impl Serialize) -> std::io::Result<Map<String, Value>> {
        let value = serde_json::to_value(value).map_err(std::io::Error::other)?;
        Ok(Map::from_iter([(self.name.to_string(), value)]))
    }
}

/// Several keys of one section written together, as one merge: a choice
/// that sets two leaves (dark mode, which also leaves high contrast) must not
/// be seen half made.
pub struct SectionWrite {
    section: Option<&'static str>,
    values: Map<String, Value>,
    error: Option<std::io::Error>,
}

impl Default for SectionWrite {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionWrite {
    /// An empty write.
    #[must_use]
    pub fn new() -> Self {
        Self {
            section: None,
            values: Map::new(),
            error: None,
        }
    }

    /// Add `key = value`. Every key must share the first key's section;
    /// one that does not makes [`Self::commit`] fail without writing.
    #[must_use]
    pub fn set<T: Serialize + DeserializeOwned>(mut self, key: &Key<T>, value: &T) -> Self {
        if self.error.is_some() {
            return self;
        }
        if self.section.is_some_and(|section| section != key.section) {
            self.error = Some(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{}.{} is not in section {:?}",
                    key.section,
                    key.name,
                    self.section.unwrap_or_default()
                ),
            ));
            return self;
        }
        self.section = Some(key.section);
        match key.leaf(value) {
            Ok(leaf) => self.values.extend(leaf),
            Err(error) => self.error = Some(error),
        }
        self
    }

    /// Store every key at once, recording `origin`.
    ///
    /// # Errors
    /// A key was in another section or could not be serialised, or the write
    /// fails as [`crate::update_section`] does.
    pub fn commit_by(self, origin: &ChangeOrigin) -> std::io::Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let Some(section) = self.section else {
            return Ok(());
        };
        crate::update_section_by(section, self.values, origin)
    }

    /// [`Self::commit_by`] with no recorded origin.
    ///
    /// # Errors
    /// As [`Self::commit_by`].
    pub fn commit(self) -> std::io::Result<()> {
        self.commit_by(&ChangeOrigin::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::TOUCHPAD_ENABLED;
    use serde_json::json;
    use std::sync::Arc;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn get_returns_default_for_absent_and_mistyped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(
            TOUCHPAD_ENABLED.get_at(&path),
            "no file reads as the default"
        );
        std::fs::write(&path, r#"{"input":{"touchpad_enabled":"false"}}"#).unwrap();
        assert!(
            TOUCHPAD_ENABLED.get_at(&path),
            "a string where a bool belongs reads as the default, not as false"
        );
        std::fs::write(&path, r#"{"input":"broken"}"#).unwrap();
        assert!(
            TOUCHPAD_ENABLED.get_at(&path),
            "a section of the wrong type"
        );
        std::fs::write(&path, r#"{"input":{"touchpad_enabled":false}}"#).unwrap();
        assert!(!TOUCHPAD_ENABLED.get_at(&path), "a stored value wins");
    }

    #[test]
    fn set_writes_one_leaf_in_the_shape_untyped_readers_expect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"input":{"tap_to_click":true},"display":{"night_light":true}}"#,
        )
        .unwrap();
        TOUCHPAD_ENABLED.set_at(&path, false).unwrap();
        assert_eq!(
            Value::Object(crate::load_at(&path)),
            json!({
                "input": {"tap_to_click": true, "touchpad_enabled": false},
                "display": {"night_light": true},
            })
        );
        assert_eq!(
            crate::bool_at(&crate::load_at(&path), "input", "touchpad_enabled"),
            Some(false),
            "the untyped reader the compositor uses sees the typed write"
        );
    }

    #[test]
    fn update_is_atomic_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        // No directory yet: the first flips race to create it, under the lock.
        let path = Arc::new(dir.path().join("fresh/tilo/settings.json"));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let path = Arc::clone(&path);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        TOUCHPAD_ENABLED.update_at(&path, |on| !on).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        // 200 flips from the default (on) land back on on only if no flip
        // was built on a value another flip was replacing.
        assert!(TOUCHPAD_ENABLED.get_at(&path));
        assert_eq!(
            crate::load_at(&path)["input"]["touchpad_enabled"],
            json!(true),
            "the leaf is stored, not merely defaulted"
        );
    }

    #[test]
    fn update_through_a_source_holds_its_lock_and_returns_the_stored_value() {
        let doc = Arc::new(crate::HostDocument::new(Map::new(), |_| Ok(())));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let doc = Arc::clone(&doc);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        let mut step = |section: &Map<String, Value>| {
                            let count = section.get("count").and_then(Value::as_u64).unwrap_or(0);
                            object(json!({"count": count.saturating_add(1)}))
                        };
                        crate::SettingsSource::update_section_with(
                            doc.as_ref(),
                            "counter",
                            &ChangeOrigin::default(),
                            &mut step,
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            crate::SettingsSource::load(doc.as_ref())["counter"]["count"],
            json!(200)
        );
    }

    #[test]
    fn a_section_write_refuses_a_key_from_another_section_and_writes_nothing() {
        let error = SectionWrite::new()
            .set(&crate::appearance::HIGH_CONTRAST, &true)
            .set(&TOUCHPAD_ENABLED, &false)
            .commit()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(SectionWrite::new().commit().is_ok(), "nothing to write");
    }
}
