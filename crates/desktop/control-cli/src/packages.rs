//! The package resources a workspace keeps — its trust list and the registries it fetches apps
//! from — read through the desktop's action API. The device daemon, the launcher, the installer
//! and the command line all read them here, so one reader answers "what does this workspace
//! trust" and "where does it fetch from". A device keeps no list of its own.

use serde_json::Value;

/// An attestor whose signatures the workspace accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustAttestor {
    pub id: String,
    pub pubkey_hex: String,
    /// `all`, `mime:<type>` or `prefix:<id>`.
    pub scope: String,
}

/// A publisher key the workspace refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokedPublisher {
    pub pubkey_hex: String,
    pub reason: String,
}

/// A registry the workspace fetches apps from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub name: String,
    pub base_url: String,
    pub owner_pubkey_hex: String,
    pub enabled: bool,
    /// `stable` or `development`.
    pub channel: String,
}

/// What names a workspace's trust list (its JSON, `{attestors, revoked}`) for [`trust`] to read
/// instead of asking a desktop: what the daemon hands each launch it starts, read from the
/// workspace the launch runs in, and what a test or a development build sets.
pub const WORKSPACE_TRUST_ENV: &str = "TILO_WORKSPACE_TRUST";

/// The workspace's trust list: the attestors it accepts and the publisher keys it refuses. Empty
/// when no desktop answers, which trusts nothing beyond a package's own signature. A process the
/// daemon started reads the list it was handed ([`WORKSPACE_TRUST_ENV`]): the daemon never waits
/// on its own action listing to start what it launches.
#[must_use]
pub fn trust() -> (Vec<TrustAttestor>, Vec<RevokedPublisher>) {
    let Some(value) = reading(WORKSPACE_TRUST_ENV, "trust.list") else {
        return (Vec::new(), Vec::new());
    };
    let attestors = rows(&value, "attestors")
        .filter_map(|row| {
            Some(TrustAttestor {
                id: text(row, "id")?,
                pubkey_hex: text(row, "pubkey_hex")?,
                scope: text(row, "scope").unwrap_or_else(|| "all".to_owned()),
            })
        })
        .collect();
    let revoked = rows(&value, "revoked")
        .filter_map(|row| {
            Some(RevokedPublisher {
                pubkey_hex: text(row, "pubkey_hex")?,
                reason: text(row, "reason").unwrap_or_default(),
            })
        })
        .collect();
    (attestors, revoked)
}

/// The registries the workspace fetches apps from; none when no desktop answers.
///
/// `TILO_WORKSPACE_REGISTRIES` names the list (its JSON, `{"registries": [...]}`) a test or a
/// development build reads instead of asking a desktop.
#[must_use]
pub fn registries() -> Vec<Registry> {
    let Some(value) = reading("TILO_WORKSPACE_REGISTRIES", "registry.list") else {
        return Vec::new();
    };
    rows(&value, "registries")
        .filter_map(|row| {
            Some(Registry {
                name: text(row, "name")?,
                base_url: text(row, "base_url")?,
                owner_pubkey_hex: text(row, "owner_pubkey_hex")?,
                enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                channel: text(row, "channel").unwrap_or_else(|| "stable".to_owned()),
            })
        })
        .collect()
}

/// Run the workspace action `name` with `args` and answer what it answered.
///
/// # Errors
/// No desktop answered, the action is unknown or ambiguous, or it failed.
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    crate::front::call_action(
        &crate::client::sockets_from_env(),
        name,
        args,
        &crate::front::Spelling::standalone(),
    )
}

/// The JSON `env` names, or else the workspace's reading `action`.
fn reading(env: &str, action: &str) -> Option<Value> {
    match std::env::var_os(env) {
        Some(json) => serde_json::from_str(&json.to_string_lossy()).ok(),
        None => call(action, &Value::Object(serde_json::Map::new())).ok(),
    }
}

fn rows<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn text(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trust_reading_names_its_attestors_and_refused_keys() {
        let value = serde_json::json!({
            "attestors": [{"id": "curated", "pubkey_hex": "ab", "scope": "prefix:so.tilo."}],
            "revoked": [{"pubkey_hex": "cd", "reason": "leaked"}, {"reason": "no key"}],
        });
        let attestors: Vec<_> = rows(&value, "attestors").collect();
        assert_eq!(
            text(attestors[0], "scope").as_deref(),
            Some("prefix:so.tilo.")
        );
        let revoked: Vec<_> = rows(&value, "revoked")
            .filter_map(|row| text(row, "pubkey_hex"))
            .collect();
        assert_eq!(revoked, ["cd"], "a row without a key refuses nothing");
    }
}
