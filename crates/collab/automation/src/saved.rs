//! A shortcut as a workspace keeps it: its id and its encoded program,
//! decoded only where it runs or is edited.
//!
//! A workspace is read whole wherever a desktop shows it, and most readers
//! never look inside its shortcuts; decoding them there was work (and, in a
//! browser, code) every desktop paid for the few that run or edit one.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Shortcut;

/// One saved shortcut, still encoded.
#[derive(Clone, Debug)]
pub struct SavedShortcut {
    id: String,
    json: String,
}

impl SavedShortcut {
    /// `json` as a saved shortcut: a JSON object with a non-empty `id`.
    /// The rest of it is checked when it is decoded ([`Self::decode`]).
    ///
    /// # Errors
    ///
    /// Why `json` is not a shortcut.
    pub fn parse(json: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| format!("not a shortcut: {error}"))?;
        let id = Self::id_of(&value)?;
        Ok(Self {
            id,
            json: json.to_owned(),
        })
    }

    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        Ok(Self {
            id: Self::id_of(value)?,
            json: value.to_string(),
        })
    }

    fn id_of(value: &serde_json::Value) -> Result<String, String> {
        let id = value
            .as_object()
            .ok_or("not a shortcut: not an object")?
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if id.trim().is_empty() {
            return Err("a shortcut needs an id".into());
        }
        Ok(id.to_owned())
    }

    /// `shortcut`, encoded.
    ///
    /// # Panics
    ///
    /// Never: a shortcut always encodes.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a shortcut is plain data with string keys, which JSON always encodes"
    )]
    pub fn encode(shortcut: &Shortcut) -> Self {
        Self {
            id: shortcut.id.clone(),
            json: serde_json::to_string(shortcut).expect("a shortcut encodes"),
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The encoded program.
    #[must_use]
    pub fn json(&self) -> &str {
        &self.json
    }

    /// The program.
    ///
    /// # Errors
    ///
    /// Why the saved value is not a shortcut this build reads.
    pub fn decode(&self) -> Result<Shortcut, String> {
        serde_json::from_str(&self.json).map_err(|error| format!("not a shortcut: {error}"))
    }
}

/// The same shortcut, however its encoding orders or spaces it.
impl PartialEq for SavedShortcut {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && (self.json == other.json
                || serde_json::from_str::<serde_json::Value>(&self.json).ok()
                    == serde_json::from_str::<serde_json::Value>(&other.json).ok())
    }
}

impl Eq for SavedShortcut {}

/// Decode the shortcuts of `saved` that decode, in order.
pub fn decode_saved<'a>(saved: impl IntoIterator<Item = &'a SavedShortcut>) -> Vec<Shortcut> {
    saved
        .into_iter()
        .filter_map(|saved| saved.decode().ok())
        .collect()
}

/// Stored as the shortcut itself, so a workspace saved before keeps reading.
impl Serialize for SavedShortcut {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value: serde_json::Value =
            serde_json::from_str(&self.json).map_err(serde::ser::Error::custom)?;
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SavedShortcut {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_value(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(id: &str) -> Shortcut {
        serde_json::from_value(serde_json::json!({"id": id, "name": "Water", "steps": []})).unwrap()
    }

    #[test]
    fn a_saved_shortcut_reads_and_writes_as_the_shortcut() {
        let water = shortcut("water");
        let stored = serde_json::to_value(&water).unwrap();
        let saved: SavedShortcut = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(saved.id(), "water");
        assert_eq!(serde_json::to_value(&saved).unwrap(), stored);
        assert_eq!(saved.decode().unwrap(), water);
        assert_eq!(SavedShortcut::encode(&water), saved);
    }

    #[test]
    fn only_an_object_with_an_id_is_saved() {
        assert!(SavedShortcut::parse("[]").is_err());
        assert!(SavedShortcut::parse(r#"{"id": "  ", "steps": []}"#).is_err());
        assert!(SavedShortcut::parse("{").is_err());
        let odd = SavedShortcut::parse(r#"{"id": "odd", "steps": 3}"#).unwrap();
        assert!(
            odd.decode().is_err(),
            "the rest is checked where it is decoded"
        );
        assert!(decode_saved([&odd]).is_empty());
    }
}
