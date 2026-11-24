//! The changes a caller asks a device's package cache for: each one action of the daemon's
//! (`devices/<device>/packages#<id>`), and the [`Change`] it makes. What each action is called and
//! what it takes is declared with the daemon that publishes it.

use serde_json::{Map, Value};

use crate::Change;

/// One published action: its id and the change its arguments make.
pub struct Published {
    pub id: &'static str,
    /// The [`Change`] it makes, by name.
    pub(crate) change: &'static str,
    /// What the action itself says of the change, beside its arguments (`pinned` for pinning).
    pub(crate) fixed: Option<(&'static str, bool)>,
}

impl Published {
    /// The change `arguments` ask for.
    ///
    /// # Errors
    ///
    /// The arguments are not the ones it takes.
    pub fn change(&self, mut arguments: Map<String, Value>) -> Result<Change, String> {
        if let Some((name, value)) = self.fixed {
            arguments.insert(name.to_owned(), Value::Bool(value));
        }
        let mut tagged = Map::new();
        tagged.insert(self.change.to_owned(), Value::Object(arguments));
        serde_json::from_value(Value::Object(tagged))
            .map_err(|error| format!("{}: {error}", self.id))
    }
}

/// The cache's published actions.
const PUBLISHED: [Published; 15] = [
    Published {
        id: "collect",
        change: "Collect",
        fixed: None,
    },
    Published {
        id: "verify",
        change: "Verify",
        fixed: None,
    },
    Published {
        id: "runtime-install",
        change: "InstallRuntime",
        fixed: None,
    },
    Published {
        id: "runtime-fetch",
        change: "FetchRuntime",
        fixed: None,
    },
    Published {
        id: "runtime-remove",
        change: "RemoveRuntime",
        fixed: None,
    },
    Published {
        id: "runtime-pin",
        change: "PinRuntime",
        fixed: Some(("pinned", true)),
    },
    Published {
        id: "runtime-unpin",
        change: "PinRuntime",
        fixed: Some(("pinned", false)),
    },
    Published {
        id: "image-add",
        change: "AddImage",
        fixed: None,
    },
    Published {
        id: "image-remove",
        change: "RemoveImage",
        fixed: None,
    },
    Published {
        id: "image-fetch",
        change: "FetchImage",
        fixed: None,
    },
    Published {
        id: "flatpak-remote-add",
        change: "FlatpakRemoteAdd",
        fixed: None,
    },
    Published {
        id: "flatpak-remote-remove",
        change: "FlatpakRemoteRemove",
        fixed: None,
    },
    Published {
        id: "flatpak-remote-enable",
        change: "FlatpakRemoteSetEnabled",
        fixed: Some(("enabled", true)),
    },
    Published {
        id: "flatpak-remote-disable",
        change: "FlatpakRemoteSetEnabled",
        fixed: Some(("enabled", false)),
    },
    Published {
        id: "flatpak-flathub",
        change: "FlatpakFlathub",
        fixed: None,
    },
];

/// The actions a device's package cache publishes.
#[must_use]
pub fn published() -> &'static [Published] {
    &PUBLISHED
}

/// Answer the reading `id` with `arguments`: its value, or why it could not be read; `None` for
/// no reading of the cache's.
#[must_use]
pub fn reading(
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<Result<serde_json::Value, String>> {
    crate::flatpak::reading(id, arguments).or_else(|| crate::catalogue::reading(id, arguments))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn change(id: &str, arguments: Value) -> Result<Change, String> {
        let Value::Object(arguments) = arguments else {
            return Err(format!("{arguments} is no object of arguments"));
        };
        published()
            .iter()
            .find(|published| published.id == id)
            .expect("published")
            .change(arguments)
    }

    /// Each action's arguments make its change, with what the action itself says of it.
    #[test]
    fn an_action_makes_the_change_it_names() {
        assert_eq!(
            change("runtime-unpin", json!({"version": "1.2"})),
            Ok(Change::PinRuntime {
                version: "1.2".into(),
                pinned: false
            })
        );
        assert_eq!(
            change("collect", json!({"dry_run": true})),
            Ok(Change::Collect {
                dry_run: true,
                grace_secs: runtime_bundle::gc::DEFAULT_GRACE_SECS,
                max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
            })
        );
        let refused = change("image-fetch", json!({"id": "tapp:00"})).unwrap_err();
        assert!(refused.contains("registry"), "{refused}");
    }
}
