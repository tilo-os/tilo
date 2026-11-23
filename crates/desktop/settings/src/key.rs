//! Typed settings: one [`Key`] per leaf of the settings document, naming its section, name, type
//! and default.
//!
//! Keys write leaf by leaf, never a whole section: writers shipping at different times share a
//! section, and each must keep the leaves it does not know. An absent leaf, or one whose value has
//! another type, reads as the key's default.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{ChangeOrigin, SettingsSource};

pub struct Key<T: 'static> {
    section: &'static str,
    name: &'static str,
    default: fn() -> T,
}

impl<T> Key<T> {
    #[must_use]
    pub const fn new(section: &'static str, name: &'static str, default: fn() -> T) -> Self {
        Self {
            section,
            name,
            default,
        }
    }

    #[must_use]
    pub const fn section(&self) -> &'static str {
        self.section
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn default_value(&self) -> T {
        (self.default)()
    }
}

impl<T: Serialize + DeserializeOwned> Key<T> {
    #[must_use]
    pub fn from_section(&self, section: &Map<String, Value>) -> T {
        section
            .get(self.name)
            .and_then(|value| T::deserialize(value).ok())
            .unwrap_or_else(|| self.default_value())
    }

    #[must_use]
    pub fn from_document(&self, document: &Map<String, Value>) -> T {
        match document.get(self.section) {
            Some(Value::Object(section)) => self.from_section(section),
            _ => self.default_value(),
        }
    }

    #[must_use]
    pub fn get(&self) -> T {
        self.from_section(&crate::load_section(self.section))
    }

    #[must_use]
    pub fn get_in(&self, source: &dyn SettingsSource) -> T {
        self.from_section(&crate::load_section_in(source, self.section))
    }

    pub fn set(&self, value: T) -> std::io::Result<()> {
        self.set_by(value, &ChangeOrigin::default())
    }

    pub fn set_by(&self, value: T, origin: &ChangeOrigin) -> std::io::Result<()> {
        crate::update_section_by(self.section, self.leaf(value)?, origin)
    }

    pub fn set_in(&self, source: &dyn SettingsSource, value: T) -> std::io::Result<()> {
        source.update_section(self.section, self.leaf(value)?)
    }

    pub fn update(&self, change: impl FnOnce(T) -> T) -> std::io::Result<T> {
        self.update_by(&ChangeOrigin::default(), change)
    }

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

    pub fn update_in(
        &self,
        source: &dyn SettingsSource,
        change: impl FnOnce(T) -> T,
    ) -> std::io::Result<T> {
        let mut change = Some(change);
        let mut stored = None;
        source.update_section_with(self.section, &ChangeOrigin::default(), &mut |section| {
            self.step(section, &mut change, &mut stored)
        })?;
        Self::stored(stored)
    }

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

    fn stored(stored: Option<std::io::Result<T>>) -> std::io::Result<T> {
        stored.unwrap_or_else(|| Err(std::io::Error::other("the settings change was not applied")))
    }

    fn leaf(&self, value: impl Serialize) -> std::io::Result<Map<String, Value>> {
        let value = serde_json::to_value(value).map_err(std::io::Error::other)?;
        Ok(Map::from_iter([(self.name.to_string(), value)]))
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

    tilo_test_env::scenario! {
        fn a_typed_key_reads_and_writes_its_one_leaf;

        phase get_returns_default_for_absent_and_mistyped;
        {
            let doc = crate::HostDocument::new(Map::new(), |_| Ok(()));
            assert!(
                TOUCHPAD_ENABLED.get_in(&doc),
                "nothing stored reads as the default"
            );
            doc.update_section("input_device", object(json!({"touchpad_enabled": "false"})))
                .unwrap();
            assert!(
                TOUCHPAD_ENABLED.get_in(&doc),
                "a string where a bool belongs reads as the default, not as false"
            );
            doc.update_section("input_device", object(json!({"touchpad_enabled": false})))
                .unwrap();
            assert!(!TOUCHPAD_ENABLED.get_in(&doc), "a stored value wins");
        }

        phase set_writes_one_leaf_in_the_shape_untyped_readers_expect;
        {
            let dir = crate::test_dir();
            let store = crate::profile(dir.path());
            store
                .update_section("input_device", object(json!({"tablets": {}})))
                .unwrap();
            store
                .update_section("display", object(json!({"night_light": true})))
                .unwrap();
            TOUCHPAD_ENABLED.set_in(&*store, false).unwrap();
            assert_eq!(
                Value::Object(store.load()),
                json!({
                    "input_device": {"tablets": {}, "touchpad_enabled": false},
                    "display": {"night_light": true},
                })
            );
            assert_eq!(
                crate::bool_at(&store.load(), "input_device", "touchpad_enabled"),
                Some(false),
                "the untyped reader the compositor uses sees the typed write"
            );
        }

        phase update_is_atomic_under_concurrency;
        {
            let dir = crate::test_dir();
            let store = crate::profile(&dir.path().join("fresh/tilo"));
            let threads: Vec<_> = (0..2)
                .map(|_| {
                    let store = Arc::clone(&store);
                    std::thread::spawn(move || {
                        for _ in 0..100 {
                            TOUCHPAD_ENABLED.update_in(&*store, |on| !on).unwrap();
                        }
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            // 200 flips from the default (on) land back on on only if no flip
            // was built on a value another flip was replacing.
            assert!(TOUCHPAD_ENABLED.get_in(&*store));
            assert_eq!(
                store.load()["input_device"]["touchpad_enabled"],
                json!(true),
                "the leaf is stored, not merely defaulted"
            );
        }

    }
}
