//! Secret-free projection of personal-settings convergence decisions.
//!
//! The shell owns the authenticated device fabric and writes this bounded
//! sidecar after a merge. Settings can then explain which certified device
//! won and whether other devices still advertise a different value without
//! receiving fabric keys, ciphertext, Wi-Fi passphrases, or mutation access.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u16 = 2;
pub const FILE_NAME: &str = "continuity-provenance.json";
const MAX_RECORDS: usize = 512;
const MAX_ORIGINS: usize = 32;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub device_id: String,
    pub device_label: String,
    pub counter: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingResolution {
    pub key: String,
    pub winner: Origin,
    /// Other certified origins advertising a value different from the
    /// winner, sorted by revision and device id for stable presentation.
    pub alternatives: Vec<Origin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiResolution {
    /// Lower-case hex of the keyed network identifier. It is correlation
    /// metadata, not a password or decryptable envelope.
    pub network_id: String,
    pub ssid: String,
    pub security: String,
    pub winner: Origin,
    pub alternatives: Vec<Origin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoamingProvenance {
    pub schema_version: u16,
    pub generated_unix_ms: u64,
    pub settings: Vec<SettingResolution>,
    pub wifi: Vec<WifiResolution>,
}

impl Default for RoamingProvenance {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generated_unix_ms: 0,
            settings: Vec::new(),
            wifi: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ProvenanceError {
    UnsupportedSchema(u16),
    Invalid,
    /// Syntax or field-type failure, retaining the decoder's diagnostic.
    Json(serde_json::Error),
    /// Retains OS error kinds and atomic-write publication/durability state.
    Io(std::io::Error),
}

impl std::fmt::Display for ProvenanceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported roaming provenance schema {version}")
            }
            Self::Invalid => formatter.write_str("invalid roaming provenance"),
            Self::Json(error) => write!(formatter, "roaming provenance JSON failed: {error}"),
            Self::Io(error) => write!(formatter, "roaming provenance I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ProvenanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::UnsupportedSchema(_) | Self::Invalid => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1 {
    #[serde(rename = "schema_version")]
    _schema_version: u16,
    #[serde(default)]
    generated_unix_ms: u64,
    #[serde(default)]
    settings: Vec<LegacySettingResolution>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySettingResolution {
    key: String,
    device_id: String,
    #[serde(default)]
    device_label: String,
    counter: u64,
}

impl RoamingProvenance {
    /// Decode the current projection or migrate the read-only v1 shape.
    /// Future versions fail closed rather than being partially displayed.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceError::Json`] for malformed JSON or mismatched field types,
    /// [`ProvenanceError::Invalid`] for out-of-policy data, or
    /// [`ProvenanceError::UnsupportedSchema`] for a future schema.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ProvenanceError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(ProvenanceError::Json)?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(ProvenanceError::Invalid)?;
        let mut projection = match version {
            1 => {
                let legacy: LegacyV1 =
                    serde_json::from_value(value).map_err(ProvenanceError::Json)?;
                Self {
                    schema_version: SCHEMA_VERSION,
                    generated_unix_ms: legacy.generated_unix_ms,
                    settings: legacy
                        .settings
                        .into_iter()
                        .map(|entry| SettingResolution {
                            key: entry.key,
                            winner: Origin {
                                device_label: if entry.device_label.is_empty() {
                                    entry.device_id.clone()
                                } else {
                                    entry.device_label
                                },
                                device_id: entry.device_id,
                                counter: entry.counter,
                            },
                            alternatives: Vec::new(),
                        })
                        .collect(),
                    wifi: Vec::new(),
                }
            }
            SCHEMA_VERSION => serde_json::from_value(value).map_err(ProvenanceError::Json)?,
            other => return Err(ProvenanceError::UnsupportedSchema(other)),
        };
        projection.normalise()?;
        Ok(projection)
    }

    /// Load and validate provenance from the configured settings directory.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when no configuration directory is available or
    /// the sidecar cannot be read, and forwards decoding and validation errors.
    pub fn load() -> Result<Self, ProvenanceError> {
        let path = path()
            .ok_or_else(|| ProvenanceError::Io(std::io::Error::other("no config directory")))?;
        Self::load_at(&path)
    }

    /// Load and validate provenance from an explicit sidecar path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `path` cannot be read, and forwards decoding
    /// and validation errors from [`Self::from_slice`].
    pub fn load_at(path: &Path) -> Result<Self, ProvenanceError> {
        let bytes = std::fs::read(path).map_err(ProvenanceError::Io)?;
        Self::from_slice(&bytes)
    }

    /// Validate, deterministically order, and atomically replace the sidecar.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when no configuration directory is available or
    /// the sidecar cannot be replaced. Validation returns [`ProvenanceError::Invalid`];
    /// serialization returns [`ProvenanceError::Json`].
    pub fn save(&mut self) -> Result<(), ProvenanceError> {
        let path = path()
            .ok_or_else(|| ProvenanceError::Io(std::io::Error::other("no config directory")))?;
        self.save_at(&path)
    }

    /// Validate and atomically replace an explicit sidecar path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `path` cannot be replaced. Validation returns
    /// [`ProvenanceError::Invalid`]; serialization returns [`ProvenanceError::Json`].
    pub fn save_at(&mut self, path: &Path) -> Result<(), ProvenanceError> {
        self.schema_version = SCHEMA_VERSION;
        self.normalise()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(ProvenanceError::Json)?;
        bytes.push(b'\n');
        tilo_files::native::atomic::write_creating_parent_with_mode(path, &bytes, 0o600)
            .map_err(ProvenanceError::Io)
    }

    fn normalise(&mut self) -> Result<(), ProvenanceError> {
        if self.schema_version != SCHEMA_VERSION
            || self.settings.len() > MAX_RECORDS
            || self.wifi.len() > MAX_RECORDS
        {
            return Err(ProvenanceError::Invalid);
        }
        for setting in &mut self.settings {
            if setting.key.len() > MAX_TEXT_BYTES
                || crate::continuity::registered(&setting.key).is_none()
                || setting.alternatives.len() > MAX_ORIGINS
                || !valid_origin(&setting.winner)
                || !setting.alternatives.iter().all(valid_origin)
            {
                return Err(ProvenanceError::Invalid);
            }
            setting.alternatives.sort_by(origin_order);
            setting
                .alternatives
                .dedup_by(|a, b| a.device_id == b.device_id);
        }
        for wifi in &mut self.wifi {
            if wifi.network_id.len() != 64
                || !wifi.network_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || wifi.ssid.is_empty()
                || wifi.ssid.len() > 128
                || wifi.security.is_empty()
                || wifi.security.len() > 64
                || wifi.alternatives.len() > MAX_ORIGINS
                || !valid_origin(&wifi.winner)
                || !wifi.alternatives.iter().all(valid_origin)
            {
                return Err(ProvenanceError::Invalid);
            }
            wifi.alternatives.sort_by(origin_order);
            wifi.alternatives
                .dedup_by(|a, b| a.device_id == b.device_id);
        }
        self.settings.sort_by(|a, b| a.key.cmp(&b.key));
        self.settings.dedup_by(|a, b| a.key == b.key);
        self.wifi.sort_by(|a, b| a.network_id.cmp(&b.network_id));
        self.wifi.dedup_by(|a, b| a.network_id == b.network_id);
        Ok(())
    }
}

fn valid_origin(origin: &Origin) -> bool {
    !origin.device_id.is_empty()
        && origin.device_id.len() <= MAX_TEXT_BYTES
        && !origin.device_label.is_empty()
        && origin.device_label.len() <= MAX_TEXT_BYTES
}

fn origin_order(left: &Origin, right: &Origin) -> std::cmp::Ordering {
    right
        .counter
        .cmp(&left.counter)
        .then_with(|| left.device_id.cmp(&right.device_id))
}

#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::config_path(std::path::Path::new(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(id: &str, counter: u64) -> Origin {
        Origin {
            device_id: id.into(),
            device_label: format!("Device {id}"),
            counter,
        }
    }

    #[test]
    fn load_preserves_json_syntax_and_field_errors() {
        use std::error::Error as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        for (bytes, category, detail) in [
            (
                b"{\n  \"schema_version\": ".as_slice(),
                serde_json::error::Category::Eof,
                "line 2",
            ),
            (
                br#"{"schema_version":2,"generated_unix_ms":"yesterday","settings":[],"wifi":[]}"#
                    .as_slice(),
                serde_json::error::Category::Data,
                "expected u64",
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let error = RoamingProvenance::load_at(&path).unwrap_err();
            let cause = error
                .source()
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .unwrap();
            assert_eq!(cause.classify(), category);
            assert!(error.to_string().contains(detail), "{error}");
        }
    }

    #[test]
    fn load_preserves_io_error_kind() {
        use std::error::Error as _;

        let dir = tempfile::tempdir().unwrap();
        let error = RoamingProvenance::load_at(&dir.path().join(FILE_NAME)).unwrap_err();
        let cause = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(cause.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_atomic_publication_state() {
        use atomic_file::test_support::{Checkpoint, with_failure};
        use atomic_file::{PublicationState, publication_state};
        use std::error::Error as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        for (checkpoint, expected) in [
            (Checkpoint::BeforePublish, PublicationState::NotPublished),
            (
                Checkpoint::AfterPublish,
                PublicationState::PublicationUnknown,
            ),
            (
                Checkpoint::BeforeCommit,
                PublicationState::PublishedDurabilityUnknown,
            ),
        ] {
            let mut projection = RoamingProvenance::default();
            let error = with_failure(checkpoint, || projection.save_at(&path)).unwrap_err();
            let cause = error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap();
            assert_eq!(publication_state(cause), Some(expected));
        }
    }

    #[test]
    fn roaming_provenance_migrates_round_trips_and_fails_closed() {
        // Phase `v1_migrates_without_inventing_wifi_or_conflicts`.
        {
            let decoded = RoamingProvenance::from_slice(
                br#"{"schema_version":1,"generated_unix_ms":7,"settings":[{"key":"tilo.appearance.mode","device_id":"laptop","counter":4}]}"#,
            )
            .unwrap();
            assert_eq!(decoded.schema_version, SCHEMA_VERSION);
            assert_eq!(decoded.generated_unix_ms, 7);
            assert_eq!(decoded.settings[0].winner.device_label, "laptop");
            assert!(decoded.settings[0].alternatives.is_empty());
            assert!(decoded.wifi.is_empty());
        }
        // Phase `future_schema_fails_closed`.
        {
            assert!(matches!(
                RoamingProvenance::from_slice(br#"{"schema_version":99}"#),
                Err(ProvenanceError::UnsupportedSchema(99))
            ));
        }
        // Phase `round_trip_is_deterministic_bounded_and_secret_free`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE_NAME);
            let mut projection = RoamingProvenance {
                schema_version: SCHEMA_VERSION,
                generated_unix_ms: 9,
                settings: vec![SettingResolution {
                    key: "tilo.appearance.mode".into(),
                    winner: origin("b", 3),
                    alternatives: vec![origin("c", 1), origin("a", 2)],
                }],
                wifi: vec![WifiResolution {
                    network_id: "ab".repeat(32),
                    ssid: "Cafe".into(),
                    security: "wpa2".into(),
                    winner: origin("b", 3),
                    alternatives: vec![origin("a", 2)],
                }],
            };
            projection.save_at(&path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("password"));
            assert!(!text.contains("sealed_secret"));
            let loaded = RoamingProvenance::load_at(&path).unwrap();
            assert_eq!(loaded.settings[0].alternatives[0].device_id, "a");
            assert_eq!(loaded, projection);
        }
        // Phase `unknown_setting_cannot_enter_projection`.
        {
            let mut projection = RoamingProvenance {
                settings: vec![SettingResolution {
                    key: "tilo.future.secretish".into(),
                    winner: origin("a", 1),
                    alternatives: Vec::new(),
                }],
                ..RoamingProvenance::default()
            };
            let path = tempfile::tempdir().unwrap().path().join(FILE_NAME);
            assert!(matches!(
                projection.save_at(&path),
                Err(ProvenanceError::Invalid)
            ));
        }
    }
}
