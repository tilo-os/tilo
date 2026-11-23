//! The changes a caller asks a device's package cache for: each one action of the daemon's
//! (`devices/<device>/packages#<id>`), with its parameters, and the [`Change`] it makes.

use serde_json::{Map, Value};
use tilo_actions::{ActionParam, ParamType};

use crate::Change;

/// One published action: its id and label, its parameters, and the change its arguments make.
pub struct Published {
    pub id: &'static str,
    pub label: &'static str,
    /// The [`Change`] it makes, by name.
    pub(crate) change: &'static str,
    /// What the action itself says of the change, beside its arguments (`pinned` for pinning).
    pub(crate) fixed: Option<(&'static str, bool)>,
    params: fn() -> Vec<ActionParam>,
}

impl Published {
    /// What it takes.
    #[must_use]
    pub fn params(&self) -> Vec<ActionParam> {
        (self.params)()
    }

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

fn runtime_fetch() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "registry",
            ParamType::Text,
            "The repository, by the name the workspace's registries give it.",
        ),
        ActionParam::required(
            "runtime",
            ParamType::Text,
            "The runtime build, as `tilo-runtime/<version>` names it.",
        ),
        ActionParam::required(
            "package",
            ParamType::Text,
            "The package root the offer names, 64 hex characters: another package is refused.",
        ),
        ActionParam::required(
            "publisher",
            ParamType::Text,
            "The publisher key the offer names, 64 hex characters.",
        ),
    ]
}

fn version() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "version",
        ParamType::Text,
        "The runtime build, as `tilo-runtime/<version>` names it.",
    )]
}

fn file(what: &str) -> ActionParam {
    ActionParam::required_owned(
        "file",
        ParamType::Object,
        format!("{what}: `{{\"source\": \"tilo.content:<hash>\"}}`, its bytes put first."),
    )
}

fn collect() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "dry_run",
            ParamType::Boolean,
            "List what would go, removing nothing.",
        ),
        ActionParam::optional(
            "grace_secs",
            ParamType::Integer,
            "How long a runtime no app names yet is kept, in seconds.",
        ),
        ActionParam::optional(
            "max_store_bytes",
            ParamType::Integer,
            "The cache's size cap in bytes; 0 evicts no app for size.",
        ),
    ]
}

fn verify() -> Vec<ActionParam> {
    vec![ActionParam::optional(
        "app",
        ParamType::Text,
        "The app whose installed tree is checked; every installed app's when absent.",
    )]
}

fn runtime_install() -> Vec<ActionParam> {
    let mut params = version();
    params.extend([
        ActionParam::optional(
            "from",
            ParamType::Array,
            "The host root's files the bundle takes, each `{\"path\": \"/lib64/…\", \"file\": \
             {\"source\": \"tilo.content:<hash>\"}}`; the device's own host root when absent.",
        ),
        ActionParam::optional("arch", ParamType::Text, "The architecture it is built for."),
        ActionParam::optional(
            "identity",
            ParamType::Object,
            "The identity that signs it: `{\"source\": \"tilo.content:<hash>\"}`.",
        ),
        ActionParam::optional(
            "force",
            ParamType::Boolean,
            "Replace a bundle of that version.",
        ),
    ]);
    params
}

/// The workspace's trust list, as the caller reads it from `workspace/trust`:
/// the rows an image's signature is checked against beside the image's own roots.
fn trust() -> ActionParam {
    ActionParam::optional(
        "trust",
        ParamType::Object,
        "The workspace's trust: `{attestors: [{id, pubkey_hex, scope}], revoked: [{pubkey_hex, reason}]}`.",
    )
}

fn image_add() -> Vec<ActionParam> {
    vec![
        file("The image's .tapp"),
        ActionParam::optional(
            "publisher",
            ParamType::Text,
            "A publisher key, in hex, this image may be signed by.",
        ),
        ActionParam::optional(
            "payload_from",
            ParamType::Text,
            "The registry its payload blobs are fetched from.",
        ),
        trust(),
    ]
}

fn image_id() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "id",
        ParamType::Text,
        "The image's package id, `tapp:<hex>`.",
    )]
}

fn image_fetch() -> Vec<ActionParam> {
    let mut params = image_id();
    params.push(ActionParam::required(
        "registry",
        ParamType::Text,
        "The registry to fetch it from when no store holds it.",
    ));
    params.push(trust());
    params
}

fn flatpak_remote_add() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "name",
            ParamType::Text,
            "The remote's name on this device, such as flathub.",
        ),
        ActionParam::required(
            "url",
            ParamType::Text,
            "The repository URL to read it from.",
        ),
    ]
}

fn flatpak_remote_name() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The remote's name on this device.",
    )]
}

fn nothing() -> Vec<ActionParam> {
    Vec::new()
}

/// The cache's published actions.
const PUBLISHED: [Published; 15] = [
    Published {
        id: "collect",
        label: "Reclaim what nothing needs",
        change: "Collect",
        fixed: None,
        params: collect,
    },
    Published {
        id: "verify",
        label: "Check the installed apps against their packages",
        change: "Verify",
        fixed: None,
        params: verify,
    },
    Published {
        id: "runtime-install",
        label: "Build and install a runtime",
        change: "InstallRuntime",
        fixed: None,
        params: runtime_install,
    },
    Published {
        id: "runtime-fetch",
        label: "Fetch and install a runtime a repository offers",
        change: "FetchRuntime",
        fixed: None,
        params: runtime_fetch,
    },
    Published {
        id: "runtime-remove",
        label: "Remove a runtime",
        change: "RemoveRuntime",
        fixed: None,
        params: version,
    },
    Published {
        id: "runtime-pin",
        label: "Keep a runtime",
        change: "PinRuntime",
        fixed: Some(("pinned", true)),
        params: version,
    },
    Published {
        id: "runtime-unpin",
        label: "Let a runtime go when nothing runs on it",
        change: "PinRuntime",
        fixed: Some(("pinned", false)),
        params: version,
    },
    Published {
        id: "image-add",
        label: "Add an environment image",
        change: "AddImage",
        fixed: None,
        params: image_add,
    },
    Published {
        id: "image-remove",
        label: "Remove an environment image",
        change: "RemoveImage",
        fixed: None,
        params: image_id,
    },
    Published {
        id: "image-fetch",
        label: "Fetch an environment image",
        change: "FetchImage",
        fixed: None,
        params: image_fetch,
    },
    Published {
        id: "flatpak-remote-add",
        label: "Add a Flatpak repository",
        change: "FlatpakRemoteAdd",
        fixed: None,
        params: flatpak_remote_add,
    },
    Published {
        id: "flatpak-remote-remove",
        label: "Remove a Flatpak repository",
        change: "FlatpakRemoteRemove",
        fixed: None,
        params: flatpak_remote_name,
    },
    Published {
        id: "flatpak-remote-enable",
        label: "Enable a Flatpak repository",
        change: "FlatpakRemoteSetEnabled",
        fixed: Some(("enabled", true)),
        params: flatpak_remote_name,
    },
    Published {
        id: "flatpak-remote-disable",
        label: "Disable a Flatpak repository",
        change: "FlatpakRemoteSetEnabled",
        fixed: Some(("enabled", false)),
        params: flatpak_remote_name,
    },
    Published {
        id: "flatpak-flathub",
        label: "Add Flathub",
        change: "FlatpakFlathub",
        fixed: None,
        params: nothing,
    },
];

/// The actions a device's package cache publishes.
#[must_use]
pub fn published() -> &'static [Published] {
    &PUBLISHED
}

/// One reading a device's package cache serves, changing nothing: what its Flatpak installation
/// has right now, or what a repository the workspace configures offers.
pub struct PublishedReading {
    pub id: &'static str,
    pub label: &'static str,
    params: fn() -> Vec<ActionParam>,
}

impl PublishedReading {
    /// What it takes.
    #[must_use]
    pub fn params(&self) -> Vec<ActionParam> {
        (self.params)()
    }
}

fn catalogue() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "registry",
            ParamType::Text,
            "The repository, by the name the workspace's registries give it.",
        ),
        ActionParam::optional(
            "fresh",
            ParamType::Boolean,
            "Ask the repository now (the default); false reads the catalogue this device last \
             accepted from it, without the network.",
        ),
    ]
}

fn runtime_offer() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "registry",
            ParamType::Text,
            "The repository, by the name the workspace's registries give it.",
        ),
        ActionParam::required(
            "runtime",
            ParamType::Text,
            "The runtime build, as `tilo-runtime/<version>` names it.",
        ),
    ]
}

const READINGS: [PublishedReading; 4] = [
    PublishedReading {
        id: "flatpak",
        label: "What this device's Flatpak installation has",
        params: nothing,
    },
    PublishedReading {
        id: "flatpak-catalogue",
        label: "The apps a Flatpak repository offers",
        params: flatpak_remote_name,
    },
    PublishedReading {
        id: "catalogue",
        label: "The apps a Tilo repository offers",
        params: catalogue,
    },
    PublishedReading {
        id: "runtime-offer",
        label: "The build of a runtime a Tilo repository offers",
        params: runtime_offer,
    },
];

/// The readings a device's package cache serves.
#[must_use]
pub fn readings() -> &'static [PublishedReading] {
    &READINGS
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
