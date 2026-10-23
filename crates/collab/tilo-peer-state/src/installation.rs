//! A bot installed in a room: the governed row that says which program
//! runs, who installed it and how it may appear. It lives in the room's
//! governed state beside members and delegated agents, inside the bot
//! device's governed identity, because installing a bot changes who may act
//! in the room.
//!
//! Its grants are not in this row. They are ordinary grants with
//! `Subject::Bot(id)` in the room's policy, so every receiver enforces them
//! with the code it already runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Longest shown name of a bot.
pub const MAX_BOT_NAME_BYTES: usize = 64;
/// Records whose heads one installation may remember as its catch-up floor.
pub const MAX_ARMED_RECORDS: usize = 64;
/// Bytes the install inputs may take, serialized.
pub const MAX_INPUT_BYTES: usize = 16 * 1024;

/// An installation id: a lowercase hyphenated UUID, minted at install.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstallationId(String);

impl InstallationId {
    /// Read an installation id.
    ///
    /// # Errors
    /// Anything but a lowercase hyphenated UUID.
    pub fn parse(value: &str) -> Result<Self, String> {
        match uuid::Uuid::parse_str(value) {
            Ok(id) if id.hyphenated().to_string() == value => Ok(Self(value.to_owned())),
            _ => Err(format!("{value:?} is not an installation id")),
        }
    }

    /// A fresh id.
    #[must_use]
    pub fn mint() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallationId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        Self::parse(&value)
    }
}

impl From<InstallationId> for String {
    fn from(id: InstallationId) -> Self {
        id.0
    }
}

impl std::fmt::Display for InstallationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a bot may post under names other than its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePolicy {
    /// Always its own name and icon.
    #[default]
    Own,
    /// May post as other names (a bridge relaying people from elsewhere),
    /// always labelled as a bot.
    Personas,
}

/// The directory listing an installation came from, when it came from one.
pub use registry_types::DirectoryRef;

/// A record's heads when the bot was installed: changes at or before them
/// never fire it, so installing a bot does not replay a room's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmedRecord {
    pub app_id: String,
    pub record_id: String,
    /// Automerge change hashes, hex.
    pub heads: Vec<String>,
}

/// The governed row of an installed bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    /// The `Subject::Bot` id.
    pub id: InstallationId,
    /// BLAKE3 (lowercase hex) of the program bytes the room keeps.
    pub program: String,
    /// The name it shows when it posts as itself.
    pub name: String,
    /// BLAKE3 of an image the room keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The installer's user root, hex: the person the bot acts for.
    pub installed_by: String,
    /// When it was installed, ms since the epoch.
    pub installed_at_ms: i64,
    #[serde(default)]
    /// Whether it may post under other names.
    pub appearance: AppearancePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The directory listing it was installed from.
    pub source: Option<DirectoryRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub armed_at: Vec<ArmedRecord>,
    /// Connector events before this are never delivered.
    pub connector_cursors_from_ms: i64,
    /// Values for the program's inputs, chosen at install.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    /// A paused bot runs nothing until it is resumed.
    pub paused: bool,
}

fn is_hex_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Installation {
    /// # Errors
    /// The first field out of bounds.
    pub fn validate(&self) -> Result<(), String> {
        if !is_hex_hash(&self.program) {
            return Err("the program is named by its BLAKE3 hash".into());
        }
        if self.icon.as_deref().is_some_and(|icon| !is_hex_hash(icon)) {
            return Err("the icon is named by its BLAKE3 hash".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > MAX_BOT_NAME_BYTES
            || self.name.chars().any(char::is_control)
        {
            return Err("a bot needs a short name".into());
        }
        if !is_hex_hash(&self.installed_by) {
            return Err("the installer is a user root".into());
        }
        if self.armed_at.len() > MAX_ARMED_RECORDS
            || self.armed_at.iter().any(|armed| armed.heads.len() > 32)
        {
            return Err("too many armed records".into());
        }
        if !serde_json::to_vec(&self.inputs).is_ok_and(|bytes| bytes.len() <= MAX_INPUT_BYTES) {
            return Err("the install inputs are too large".into());
        }
        if let Some(source) = &self.source
            && [&source.registry_owner, &source.entry, &source.version]
                .iter()
                .any(|part| part.is_empty() || part.len() > 128)
        {
            return Err("the directory reference is malformed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Installation {
        Installation {
            id: InstallationId::parse("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6").unwrap(),
            program: "a".repeat(64),
            name: "Discord Bridge".into(),
            icon: None,
            installed_by: "0".repeat(64),
            installed_at_ms: 1,
            appearance: AppearancePolicy::Personas,
            source: Some(DirectoryRef {
                registry_owner: "tilo".into(),
                entry: "discord-bridge".into(),
                version: "1.0.0".into(),
            }),
            armed_at: vec![],
            connector_cursors_from_ms: 1,
            inputs: BTreeMap::new(),
            paused: false,
        }
    }

    #[test]
    fn an_installation_id_is_a_lowercase_hyphenated_uuid() {
        assert!(InstallationId::parse("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6").is_ok());
        assert!(InstallationId::parse("6F1C2A8E-3B4D-4C5E-9F60-718293A4B5C6").is_err());
        assert!(InstallationId::parse("6f1c2a8e3b4d4c5e9f60718293a4b5c6").is_err());
        assert!(serde_json::from_str::<InstallationId>("\"bridge\"").is_err());
    }

    #[test]
    fn an_installation_round_trips_and_validates() {
        let row = sample();
        let back: Installation =
            serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
        assert_eq!(back, row);
        row.validate().unwrap();
        let mut bad = sample();
        bad.program = "not a hash".into();
        assert!(bad.validate().is_err());
        let mut nameless = sample();
        nameless.name.clear();
        assert!(nameless.validate().is_err());
        let mut installer = sample();
        installer.installed_by = "ada".into();
        assert!(installer.validate().is_err());
    }
}
