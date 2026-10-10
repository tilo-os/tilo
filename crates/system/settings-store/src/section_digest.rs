//! Which top-level sections of the settings document changed.
//!
//! Every writer replaces the whole file, so a reader that follows the file's
//! modification time learns only that *something* changed. A long-lived
//! reader that does different work per section (the compositor re-probes
//! displays for `display`, reconfigures input devices for `input`, re-themes
//! for `appearance`) fingerprints each section and compares, so a write to
//! one section never pays for the others.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash as _, Hasher as _};

use serde_json::{Map, Value};

/// Per-top-level-section fingerprints of the settings document.
///
/// Fingerprints ignore object key order, so a writer that serialises the
/// same values in another order is not a change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionDigest(BTreeMap<String, u64>);

impl SectionDigest {
    /// Fingerprint every top-level section of `doc`.
    pub fn of(doc: &Map<String, Value>) -> Self {
        Self(
            doc.iter()
                .map(|(name, value)| {
                    let mut hasher = std::hash::DefaultHasher::new();
                    hash_value(value, &mut hasher);
                    (name.clone(), hasher.finish())
                })
                .collect(),
        )
    }

    /// Sections whose fingerprint differs from `previous`: added, removed or
    /// changed.
    pub fn changed_since(&self, previous: &Self) -> BTreeSet<String> {
        let added_or_changed = self
            .0
            .iter()
            .filter(|(name, digest)| previous.0.get(*name) != Some(*digest))
            .map(|(name, _)| name.clone());
        let removed = previous
            .0
            .keys()
            .filter(|name| !self.0.contains_key(*name))
            .cloned();
        added_or_changed.chain(removed).collect()
    }
}

/// Hash `value` canonically: object members in key order, each node tagged
/// with its kind so `"1"`, `1` and `[1]` never collide by construction.
fn hash_value(value: &Value, hasher: &mut std::hash::DefaultHasher) {
    match value {
        Value::Null => 0_u8.hash(hasher),
        Value::Bool(flag) => {
            1_u8.hash(hasher);
            flag.hash(hasher);
        }
        Value::Number(number) => {
            2_u8.hash(hasher);
            number.to_string().hash(hasher);
        }
        Value::String(text) => {
            3_u8.hash(hasher);
            text.hash(hasher);
        }
        Value::Array(items) => {
            4_u8.hash(hasher);
            items.len().hash(hasher);
            for item in items {
                hash_value(item, hasher);
            }
        }
        Value::Object(members) => {
            5_u8.hash(hasher);
            members.len().hash(hasher);
            let mut sorted: Vec<_> = members.iter().collect();
            sorted.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (key, member) in sorted {
                key.hash(hasher);
                hash_value(member, hasher);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn section_digest_reports_only_changed_sections() {
        let before = SectionDigest::of(&doc(&json!({
            "notifications": {"do_not_disturb": false},
            "display": {"scale": 1.5},
            "input": {"tap_to_click": true, "natural_scroll": false},
        })));

        // Only `notifications` moved.
        let dnd = SectionDigest::of(&doc(&json!({
            "notifications": {"do_not_disturb": true},
            "display": {"scale": 1.5},
            "input": {"tap_to_click": true, "natural_scroll": false},
        })));
        assert_eq!(dnd.changed_since(&before), names(&["notifications"]));

        // A section added and another removed are both changes.
        let reshaped = SectionDigest::of(&doc(&json!({
            "notifications": {"do_not_disturb": false},
            "input": {"tap_to_click": true, "natural_scroll": false},
            "appearance": {"theme": "dark"},
        })));
        assert_eq!(
            reshaped.changed_since(&before),
            names(&["appearance", "display"])
        );

        // Reordered keys inside a section, at any depth, are no change.
        let reordered = SectionDigest::of(&doc(&json!({
            "input": {"natural_scroll": false, "tap_to_click": true},
            "display": {"scale": 1.5},
            "notifications": {"do_not_disturb": false},
        })));
        assert!(reordered.changed_since(&before).is_empty());
        let nested_a = SectionDigest::of(&doc(&json!({"display": {"o": {"a": 1, "b": [1, 2]}}})));
        let nested_b = SectionDigest::of(&doc(&json!({"display": {"o": {"b": [1, 2], "a": 1}}})));
        assert!(nested_b.changed_since(&nested_a).is_empty());

        // But reordered array members are.
        let swapped = SectionDigest::of(&doc(&json!({"display": {"o": {"a": 1, "b": [2, 1]}}})));
        assert_eq!(swapped.changed_since(&nested_a), names(&["display"]));

        // The first document against nothing reports every section.
        assert_eq!(
            before.changed_since(&SectionDigest::default()),
            names(&["display", "input", "notifications"])
        );
    }

    #[test]
    fn section_digest_tells_value_kinds_apart() {
        let as_string = SectionDigest::of(&doc(&json!({"power": {"v": "1"}})));
        let as_number = SectionDigest::of(&doc(&json!({"power": {"v": 1}})));
        let as_array = SectionDigest::of(&doc(&json!({"power": {"v": [1]}})));
        assert_eq!(as_number.changed_since(&as_string), names(&["power"]));
        assert_eq!(as_array.changed_since(&as_number), names(&["power"]));
    }
}
