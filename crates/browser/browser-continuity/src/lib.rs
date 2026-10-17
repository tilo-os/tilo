//! Shared metadata contract for explicitly reviewed browser-profile migration.
//!
//! Paths are components relative to the separately granted migration source.
//! Metadata validation never discovers profiles or reads credentials. Optional
//! native capture reads only explicitly selected fixed files from held grants.
//! The host must bind delivery to its existing identity
//! and setup authority before interpreting this descriptive plan.

pub mod engine_host;
pub mod handoff;
pub mod imported_folders;
pub mod imports;
pub mod service_profiles;
pub mod session_engine;
pub mod sessions;

pub mod sources;

use std::fmt;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};

#[cfg(all(unix, feature = "native"))]
pub mod completion;
#[cfg(all(unix, feature = "native"))]
pub mod snapshot;

/// Fixed launcher-owned read-only projection location; no permission is
/// implied by this name or by metadata referring to it.
pub const PROJECTION_DIRECTORY: &str = "/run/tilo/browser-continuity";
/// Plan child in the projected continuity directory.
pub const PLAN_FILE: &str = "plan.json";
/// Directory containing reviewed, per-profile projections.
pub const PROFILES_DIRECTORY: &str = "profiles";

pub const MAX_PLAN_BYTES: usize = 16 * 1024;
pub const MAX_PROFILES: usize = 16;
pub const SCHEMA: u32 = 1;
const MAX_BINDING_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENT_BYTES: usize = 255;

/// Provider determines the exact legacy files a reviewed selection grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Chromium,
    Firefox,
}

/// One reviewed source; never an absolute host path or an ambient scanner root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub provider: Provider,
    pub relative_path: Vec<String>,
    pub bookmarks: bool,
    pub history: bool,
}

impl Profile {
    /// Construct a structurally valid profile selection.
    ///
    /// # Errors
    /// Rejects invalid IDs, paths, or an empty data selection.
    pub fn new(
        id: String,
        provider: Provider,
        relative_path: Vec<String>,
        bookmarks: bool,
        history: bool,
    ) -> Result<Self, Error> {
        let profile = Self {
            id,
            provider,
            relative_path,
            bookmarks,
            history,
        };
        profile.validate()?;
        Ok(profile)
    }

    /// Validate exact identity spelling and safe relative path components.
    ///
    /// # Errors
    /// Rejects invalid IDs, unsafe or oversized paths, and no selected data.
    pub fn validate(&self) -> Result<(), Error> {
        if self.id.len() != 64
            || !self
                .id
                .bytes()
                .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
            || !self.bookmarks && !self.history
            || self.relative_path.is_empty()
        {
            return Err(Error::InvalidProfile);
        }
        let mut bytes = 0usize;
        for component in &self.relative_path {
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.len() > MAX_COMPONENT_BYTES
                || component.contains(['/', '\\'])
                || component.chars().any(char::is_control)
            {
                return Err(Error::InvalidProfile);
            }
            bytes = bytes
                .checked_add(component.len())
                .and_then(|length| length.checked_add(1))
                .ok_or(Error::InvalidProfile)?;
        }
        if bytes.saturating_sub(1) > MAX_PATH_BYTES {
            return Err(Error::InvalidProfile);
        }
        Ok(())
    }

    /// Exact fixed children allowed by this provider and reviewed selection.
    /// An invalid empty selection grants no files; callers still validate the
    /// complete plan before applying any grants.
    #[must_use]
    pub fn allowed_files(&self) -> &'static [&'static str] {
        match (self.provider, self.bookmarks, self.history) {
            (_, false, false) => &[],
            (Provider::Chromium, true, false) => &["Bookmarks"],
            (Provider::Chromium, false, true) => &["History", "History-wal"],
            (Provider::Chromium, true, true) => &["Bookmarks", "History", "History-wal"],
            (Provider::Firefox, _, _) => &["places.sqlite", "places.sqlite-wal"],
        }
    }
}

/// Reviewed metadata, carried through the existing authenticated setup handoff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: u32,
    pub setup_binding: String,
    pub identity_public_key: [u8; 32],
    pub profiles: Vec<Profile>,
}

impl Plan {
    /// Construct a bounded schema-one plan.
    ///
    /// # Errors
    /// Rejects malformed or oversized plans and duplicate source identities.
    pub fn new(
        setup_binding: String,
        identity_public_key: [u8; 32],
        profiles: Vec<Profile>,
    ) -> Result<Self, Error> {
        let plan = Self {
            schema: SCHEMA,
            setup_binding,
            identity_public_key,
            profiles,
        };
        plan.validate()?;
        Ok(plan)
    }

    fn validate_structure(&self) -> Result<(), Error> {
        if self.schema != SCHEMA
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > MAX_BINDING_BYTES
            || self.setup_binding.chars().any(char::is_control)
            || self.profiles.len() > MAX_PROFILES
        {
            return Err(Error::InvalidPlan);
        }
        for (index, profile) in self.profiles.iter().enumerate() {
            profile.validate()?;
            if self
                .profiles
                .iter()
                .take(index)
                .any(|prior| prior.id == profile.id)
            {
                return Err(Error::InvalidPlan);
            }
        }
        Ok(())
    }

    /// Validate structure and the bounded serialized representation.
    ///
    /// # Errors
    /// Rejects invalid fields, duplicate profiles, and metadata over 16 KiB.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_structure()?;
        self.encode_bounded().map(|_| ())
    }

    /// Decode only bounded, valid UTF-8 JSON with known schema fields.
    ///
    /// # Errors
    /// Rejects oversized input, unknown fields, and malformed plans.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(Error::Oversized);
        }
        let plan: Self = serde_json::from_slice(bytes).map_err(|_| Error::Serialization)?;
        plan.validate()?;
        Ok(plan)
    }

    /// Encode validated metadata without allocating beyond the plan byte limit.
    ///
    /// # Errors
    /// Rejects invalid fields or a serialized representation over 16 KiB.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate_structure()?;
        self.encode_bounded()
    }

    fn encode_bounded(&self) -> Result<Vec<u8>, Error> {
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            exceeded: false,
        };
        if serde_json::to_writer(&mut writer, self).is_err() {
            return Err(if writer.exceeded {
                Error::Oversized
            } else {
                Error::Serialization
            });
        }
        Ok(writer.bytes)
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PLAN_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("browser plan exceeds its byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stable failure categories; errors never expose source paths or user data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPlan,
    InvalidProfile,
    Oversized,
    Serialization,
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPlan => "Invalid browser continuity plan",
            Self::InvalidProfile => "Invalid browser continuity profile",
            Self::Oversized => "Browser continuity plan exceeds its byte limit",
            Self::Serialization => "Browser continuity plan is not valid JSON",
        })
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: Provider, bookmarks: bool, history: bool) -> Result<Profile, Error> {
        Profile::new(
            "a".repeat(64),
            provider,
            vec![".config".into(), "chromium".into(), "Default".into()],
            bookmarks,
            history,
        )
    }

    #[test]
    fn round_trip_preserves_reviewed_selection_and_exact_provider_file_grants() {
        for (provider, bookmarks, history, expected) in [
            (Provider::Chromium, true, false, vec!["Bookmarks"]),
            (
                Provider::Chromium,
                false,
                true,
                vec!["History", "History-wal"],
            ),
            (
                Provider::Chromium,
                true,
                true,
                vec!["Bookmarks", "History", "History-wal"],
            ),
            (
                Provider::Firefox,
                true,
                false,
                vec!["places.sqlite", "places.sqlite-wal"],
            ),
            (
                Provider::Firefox,
                false,
                true,
                vec!["places.sqlite", "places.sqlite-wal"],
            ),
        ] {
            let selected = profile(provider, bookmarks, history).unwrap();
            assert_eq!(selected.allowed_files(), expected);
            assert!(!selected.allowed_files().iter().any(|name| matches!(
                *name,
                "Login Data" | "Cookies" | "History-shm" | "places.sqlite-shm"
            )));
            let plan = Plan::new("setup:reviewed".into(), [7; 32], vec![selected]).unwrap();
            assert_eq!(Plan::decode(&plan.encode().unwrap()).unwrap(), plan);
        }
        let mut empty = profile(Provider::Chromium, true, false).unwrap();
        empty.bookmarks = false;
        assert!(empty.allowed_files().is_empty());
        assert_eq!(empty.validate(), Err(Error::InvalidProfile));
    }

    #[test]
    fn relative_components_reject_traversal_controls_and_oversized_utf8() {
        for component in [
            "", ".", "..", "/home", "a/b", "a\\b", "a\0b", "a\nb", "a\u{85}b",
        ] {
            let mut selected = profile(Provider::Chromium, true, false).unwrap();
            selected.relative_path = vec![component.into()];
            assert_eq!(selected.validate(), Err(Error::InvalidProfile));
        }
        let mut selected = profile(Provider::Chromium, true, false).unwrap();
        selected.relative_path.clear();
        assert_eq!(selected.validate(), Err(Error::InvalidProfile));
        selected.relative_path = vec!["é".repeat(128)];
        assert_eq!(selected.validate(), Err(Error::InvalidProfile));
        selected.relative_path = vec!["x".repeat(255); 16];
        selected.validate().unwrap();
        selected.relative_path.push("a".into());
        assert_eq!(selected.validate(), Err(Error::InvalidProfile));
    }

    #[test]
    fn schema_binding_profile_ids_and_duplicates_are_strict() {
        let selected = profile(Provider::Chromium, true, true).unwrap();
        assert_eq!(
            Plan::new(
                "setup".into(),
                [7; 32],
                vec![selected.clone(), selected.clone()]
            ),
            Err(Error::InvalidPlan)
        );
        for id in [
            "A".repeat(64),
            "g".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
        ] {
            let mut changed = selected.clone();
            changed.id = id;
            assert_eq!(changed.validate(), Err(Error::InvalidProfile));
        }
        let mut plan = Plan::new("x".repeat(256), [7; 32], vec![selected]).unwrap();
        plan.schema = 2;
        assert_eq!(plan.validate(), Err(Error::InvalidPlan));
        plan.schema = SCHEMA;
        for binding in [
            String::new(),
            "x".repeat(257),
            "setup\0other".into(),
            "setup\nother".into(),
        ] {
            plan.setup_binding = binding;
            assert_eq!(plan.validate(), Err(Error::InvalidPlan));
        }
    }

    #[test]
    fn decode_refuses_unknown_fields_invalid_utf8_and_oversized_input() {
        let plan = Plan::new(
            "setup".into(),
            [7; 32],
            vec![profile(Provider::Firefox, true, true).unwrap()],
        )
        .unwrap();
        let mut encoded = serde_json::to_value(&plan).unwrap();
        encoded["unexpected"] = serde_json::json!("credential");
        assert_eq!(
            Plan::decode(&serde_json::to_vec(&encoded).unwrap()),
            Err(Error::Serialization)
        );
        let mut encoded = serde_json::to_value(&plan).unwrap();
        encoded["profiles"][0]["absolute_path"] = serde_json::json!("/home/person");
        assert_eq!(
            Plan::decode(&serde_json::to_vec(&encoded).unwrap()),
            Err(Error::Serialization)
        );
        let mut encoded = serde_json::to_value(&plan).unwrap();
        encoded["profiles"][0]["provider"] = serde_json::json!("other-browser");
        assert_eq!(
            Plan::decode(&serde_json::to_vec(&encoded).unwrap()),
            Err(Error::Serialization)
        );
        let mut invalid_utf8 = plan.encode().unwrap();
        let position = invalid_utf8
            .windows(9)
            .position(|bytes| bytes == b"\".config\"")
            .expect("fixture profile path missing");
        *invalid_utf8
            .get_mut(position.saturating_add(1))
            .expect("fixture profile path missing") = 0xff;
        assert_eq!(Plan::decode(&invalid_utf8), Err(Error::Serialization));
        assert_eq!(
            Plan::decode(&vec![b' '; MAX_PLAN_BYTES.saturating_add(1)]),
            Err(Error::Oversized)
        );
    }

    #[test]
    fn every_entry_and_encoding_byte_remains_bounded() {
        let mut profiles = Vec::new();
        for index in 0..MAX_PROFILES {
            let mut selected = profile(Provider::Chromium, true, false).unwrap();
            selected.id = format!("{index:064x}");
            profiles.push(selected);
        }
        let mut plan = Plan::new("setup".into(), [7; 32], profiles).unwrap();
        assert!(plan.encode().unwrap().len() <= MAX_PLAN_BYTES);
        let mut extra = profile(Provider::Chromium, true, false).unwrap();
        extra.id = "b".repeat(64);
        plan.profiles.push(extra);
        assert_eq!(plan.validate(), Err(Error::InvalidPlan));
        plan.profiles.pop();
        for selected in &mut plan.profiles {
            selected.relative_path = vec!["x".repeat(255); 16];
            selected.validate().unwrap();
        }
        assert_eq!(plan.validate(), Err(Error::Oversized));
        assert_eq!(plan.encode(), Err(Error::Oversized));
    }
}
