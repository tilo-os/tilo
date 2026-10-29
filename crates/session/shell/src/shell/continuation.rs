//! How a window resumes on another of its person's devices.
//!
//! Every workspace is on every one of a person's devices, and each device
//! writes its own part of the workspace's layout record. A window's
//! continuation sits in that record beside the window
//! ([`tilo_desktop_model::shared_record::continuation`]): the device that
//! last used the window notes how to resume it, and the person's other
//! devices read it when they open the window there. Each device's focus sits
//! there too, which is what "continue on this device" offers
//! ([`crate::shell::continuity_suggestion`]).
//!
//! The continuation carries logical app and file-record references only. It
//! never carries Wayland surface ids, local paths, file contents, thumbnails,
//! or credentials. Apps may attach a small, explicitly syncable opaque resume
//! payload; applications remain responsible for encoding a path-free state
//! such as a URL, record id, page number, or playback position.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use thiserror::Error;

use crate::shell::state::{Tab, TabId, TileContent};
use tilo_network_actor::fabric::{DeviceId, InvalidDeviceId, valid_identifier};

/// Resume data is intentionally small: it is metadata, never an app backup.
pub const MAX_RESUME_BYTES: usize = 32 * 1024;
const MAX_APP_ID_BYTES: usize = 256;
const MAX_RECORD_ID_BYTES: usize = 256;
/// A record text longer than this is not a continuation this build wrote.
const MAX_RECORD_TEXT_BYTES: usize = 2 * (MAX_RESUME_BYTES + 4 * 1024);

/// Stable logical identity for an app instance. A shell tab id is already
/// durable and peer-salted, so it is suitable for scratch-record continuity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppInstanceId(pub u64);

/// A monotonically increasing device-authored revision. Device id is part of
/// the ordering so independently captured states always have a deterministic
/// winner, even when their counters coincide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityStamp {
    pub counter: u64,
    pub device: DeviceId,
}

impl ContinuityStamp {
    fn ordering_key(&self) -> (u64, &str) {
        (self.counter, self.device.as_str())
    }

    /// Advance a local Lamport clock after observing this stamp. Device ids
    /// only break a true concurrent tie; they must never compensate for a
    /// freshly-started device having a smaller private counter.
    pub fn advance_clock(&self, local_counter: &mut u64) {
        *local_counter = (*local_counter).max(self.counter).saturating_add(1);
    }

    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.ordering_key() > other.ordering_key()
    }
}

/// A path-free logical launch target.
///
/// A file target names a durable record and optional hashes for its current
/// metadata/preview. There is deliberately no local path, content manifest,
/// or thumbnail payload: Files can show a projected preview and hydrate bytes
/// only when the person opens the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchIntent {
    App {
        app_id: String,
    },
    FileReference {
        record_id: String,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    },
}

impl LaunchIntent {
    pub fn app(app_id: impl Into<String>) -> Result<Self, ContinuityError> {
        let intent = Self::App {
            app_id: app_id.into(),
        };
        intent.validate()?;
        Ok(intent)
    }

    pub fn file_reference(
        record_id: impl Into<String>,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    ) -> Result<Self, ContinuityError> {
        let intent = Self::FileReference {
            record_id: record_id.into(),
            content_hash,
            thumbnail_hash,
        };
        intent.validate()?;
        Ok(intent)
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        match self {
            Self::App { app_id } if valid_app_id(app_id) => Ok(()),
            Self::FileReference { record_id, .. }
                if valid_identifier(record_id, MAX_RECORD_ID_BYTES) =>
            {
                Ok(())
            }
            Self::App { .. } => Err(ContinuityError::InvalidAppId),
            Self::FileReference { .. } => Err(ContinuityError::InvalidRecordReference),
        }
    }
}

#[derive(Serialize, Deserialize)]
// Externally tagged so the same validated representation works with both
// human-readable JSON and non-self-describing Postcard checkpoints.
#[serde(rename_all = "snake_case")]
enum LaunchIntentWire {
    App {
        app_id: String,
    },
    FileReference {
        record_id: String,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    },
}

impl Serialize for LaunchIntent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        match self {
            Self::App { app_id } => LaunchIntentWire::App {
                app_id: app_id.clone(),
            },
            Self::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            } => LaunchIntentWire::FileReference {
                record_id: record_id.clone(),
                content_hash: *content_hash,
                thumbnail_hash: *thumbnail_hash,
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LaunchIntent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let intent = match LaunchIntentWire::deserialize(deserializer)? {
            LaunchIntentWire::App { app_id } => Self::App { app_id },
            LaunchIntentWire::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            } => Self::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            },
        };
        intent.validate().map_err(D::Error::custom)?;
        Ok(intent)
    }
}

/// Whether an app explicitly permits a resume state to leave this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeSensitivity {
    Syncable,
    Sensitive,
}

/// Versioned opaque bytes supplied by an app's explicit safe-resume contract.
///
/// `Sensitive` state is rejected rather than redacted: silently publishing a
/// partial credential/cookie/session blob creates a misleading recovery path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeMetadata {
    schema_version: u16,
    bytes: Vec<u8>,
}

impl ResumeMetadata {
    /// # Errors
    ///
    /// Returns an error for sensitive, oversized, or otherwise invalid resume
    /// metadata.
    pub fn new(
        schema_version: u16,
        sensitivity: ResumeSensitivity,
        bytes: Vec<u8>,
    ) -> Result<Self, ContinuityError> {
        if sensitivity != ResumeSensitivity::Syncable {
            return Err(ContinuityError::SensitiveResumeState);
        }
        let value = Self {
            schema_version,
            bytes,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        if self.schema_version == 0 {
            return Err(ContinuityError::InvalidResumeSchema);
        }
        if self.bytes.len() > MAX_RESUME_BYTES {
            return Err(ContinuityError::ResumeTooLarge);
        }
        if has_local_path_marker(&self.bytes) {
            return Err(ContinuityError::LocalPathInResumeState);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ResumeMetadataWire {
    schema_version: u16,
    bytes: Vec<u8>,
}

impl Serialize for ResumeMetadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        ResumeMetadataWire {
            schema_version: self.schema_version,
            bytes: self.bytes.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResumeMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ResumeMetadataWire::deserialize(deserializer)?;
        let value = Self {
            schema_version: wire.schema_version,
            bytes: wire.bytes,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

/// How one window resumes: what to launch, the app's resume state, the
/// record it shows, and which device noted it when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInstanceState {
    pub id: AppInstanceId,
    pub launch: LaunchIntent,
    pub resume: Option<ResumeMetadata>,
    /// The app's collaboration record identity.  This is distinct from the
    /// durable shell tab id while a normal launch is waiting for toplevel
    /// adoption; carrying it lets a restored process join the source CRDT
    /// record instead of creating a lookalike empty document.
    pub record_instance: Option<AppInstanceId>,
    pub stamp: ContinuityStamp,
}

#[derive(Serialize, Deserialize)]
struct AppInstanceStateWire {
    launch: LaunchIntent,
    resume: Option<ResumeMetadata>,
    record_instance: Option<AppInstanceId>,
    stamp: ContinuityStamp,
}

impl AppInstanceState {
    pub fn new(
        id: AppInstanceId,
        launch: LaunchIntent,
        resume: Option<ResumeMetadata>,
        stamp: ContinuityStamp,
    ) -> Self {
        Self {
            id,
            launch,
            resume,
            record_instance: None,
            stamp,
        }
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        self.launch.validate()?;
        if let Some(resume) = &self.resume {
            resume.validate()?;
            let LaunchIntent::App { app_id } = &self.launch else {
                return Err(ContinuityError::InvalidResumePayload);
            };
            if !crate::shell::continuation_registry::validate_app_metadata(
                app_id,
                resume.schema_version(),
                resume.bytes(),
            ) {
                return Err(ContinuityError::InvalidResumePayload);
            }
        }
        Ok(())
    }

    /// The text the layout record keeps for this window: everything but
    /// the window's own id, which is the record entry's key.
    #[must_use]
    pub fn to_record(&self) -> Option<String> {
        self.validate().ok()?;
        let bytes = postcard::to_allocvec(&AppInstanceStateWire {
            launch: self.launch.clone(),
            resume: self.resume.clone(),
            record_instance: self.record_instance,
            stamp: self.stamp.clone(),
        })
        .ok()?;
        Some(hex::encode(bytes))
    }

    /// The continuation the layout record keeps for window `id`, when it is
    /// one this build reads and safe to act on.
    #[must_use]
    pub fn from_record(id: TabId, text: &str) -> Option<Self> {
        if text.len() > MAX_RECORD_TEXT_BYTES {
            return None;
        }
        let wire: AppInstanceStateWire = postcard::from_bytes(&hex::decode(text).ok()?).ok()?;
        let state = Self {
            id: AppInstanceId(id),
            launch: wire.launch,
            resume: wire.resume,
            record_instance: wire.record_instance,
            stamp: wire.stamp,
        };
        state.validate().ok()?;
        Some(state)
    }

    /// Whether this is the same continuation as `other`, whoever noted it.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        self.launch == other.launch
            && self.resume == other.resume
            && self.record_instance == other.record_instance
    }
}

/// What a tab this device runs launches on another device, when it is safe
/// and portable to say: an app by its id, a document by its file record.
/// A peer's window (another person's, or one not opened here yet) and a
/// window that runs only on its host are nothing this device resumes.
pub(crate) fn launch_for_tab(
    tab: &Tab,
    file_reference: impl Fn(u64) -> Option<LaunchIntent>,
) -> Option<LaunchIntent> {
    match &tab.tile {
        TileContent::HostedWindow { .. } | TileContent::PeerWindow { .. } => None,
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            LaunchIntent::app(kind.app_id()).ok()
        }
        // A locally-adopted foreign app is resumable by its declared app id,
        // but its compositor surface id stays strictly local.
        TileContent::Foreign {
            app_id, surface_id, ..
        } => file_reference(*surface_id).or_else(|| LaunchIntent::app(app_id.clone()).ok()),
        TileContent::LocalApp { app_id, .. } => LaunchIntent::app(app_id.clone()).ok(),
    }
}

/// Errors are deliberately explicit so a caller can report a safe redaction
/// rather than silently keeping an unsafe value.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ContinuityError {
    #[error("device id is invalid")]
    InvalidDeviceId,
    #[error("app id is invalid or looks like a local path")]
    InvalidAppId,
    #[error("file record reference is invalid or looks like a local path")]
    InvalidRecordReference,
    #[error("sensitive app state cannot be synced")]
    SensitiveResumeState,
    #[error("resume state has no schema version")]
    InvalidResumeSchema,
    #[error("resume state exceeds {MAX_RESUME_BYTES} bytes")]
    ResumeTooLarge,
    #[error("resume state contains a raw local path marker")]
    LocalPathInResumeState,
    #[error("resume state does not match a host-owned portable schema")]
    InvalidResumePayload,
}

impl From<InvalidDeviceId> for ContinuityError {
    fn from(InvalidDeviceId: InvalidDeviceId) -> Self {
        Self::InvalidDeviceId
    }
}

fn valid_app_id(value: &str) -> bool {
    valid_identifier(value, MAX_APP_ID_BYTES) && value.contains('.')
}

fn has_local_path_marker(bytes: &[u8]) -> bool {
    // This guards accidental JSON/text payloads. It cannot prove arbitrary
    // opaque binary data harmless; that is why apps must explicitly declare
    // the payload Syncable before it reaches this model.
    const MARKERS: [&[u8]; 7] = [
        b"/home/", b"/Users/", b"file://", b"C:\\", b"D:\\", b"\\\\", b"~/.",
    ];
    MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|window| window == *marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    #[test]
    fn continuations_carry_no_paths_surfaces_or_sensitive_state() {
        assert_eq!(
            ResumeMetadata::new(1, ResumeSensitivity::Sensitive, vec![]),
            Err(ContinuityError::SensitiveResumeState)
        );
        assert_eq!(
            ResumeMetadata::new(0, ResumeSensitivity::Syncable, vec![]),
            Err(ContinuityError::InvalidResumeSchema)
        );
        assert_eq!(
            ResumeMetadata::new(
                1,
                ResumeSensitivity::Syncable,
                b"{\"path\":\"/home/ada/secret\"}".to_vec(),
            ),
            Err(ContinuityError::LocalPathInResumeState)
        );
        assert!(LaunchIntent::app("/usr/bin/browser").is_err());
        assert!(LaunchIntent::file_reference("/home/ada/book.epub", None, None).is_err());
        assert!(
            serde_json::to_string(&LaunchIntent::App {
                app_id: "/home/ada/browser".into(),
            })
            .is_err(),
            "even a manually-built bad enum must be rejected at serialization"
        );

        let intent =
            LaunchIntent::file_reference("record:book-42", Some([3; 32]), Some([4; 32])).unwrap();
        let encoded = serde_json::to_string(&intent).unwrap();
        assert!(encoded.contains("record:book-42"));
        assert!(encoded.contains("thumbnail_hash"));
        assert!(!encoded.contains("/home/"));

        // A foreign surface resumes by its app id; its surface id never
        // leaves the device.
        let foreign = Tab {
            id: 12,
            title: "/home/ada/private.pdf".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.Foreign".into(),
                surface_id: 0xfeed_beef,
            },
            app_title: None,
        };
        let launch = launch_for_tab(&foreign, |_| None).unwrap();
        assert_eq!(
            launch,
            LaunchIntent::App {
                app_id: "org.example.Foreign".into()
            }
        );
        let state = AppInstanceState::new(
            AppInstanceId(12),
            launch,
            Some(
                ResumeMetadata::new(
                    ui::collaboration::semantic_schema::WEB_LOCATION_V1,
                    ResumeSensitivity::Syncable,
                    br#"{"url":"https://example.test/page/4"}"#.to_vec(),
                )
                .unwrap(),
            ),
            ContinuityStamp {
                counter: 3,
                device: device("laptop-01"),
            },
        );
        let text = state.to_record().unwrap();
        assert!(!text.contains(&hex::encode(0xfeed_beef_u64.to_le_bytes())));
        assert!(
            !hex::decode(&text)
                .unwrap()
                .windows(6)
                .any(|w| w == b"/home/")
        );
        assert_eq!(AppInstanceState::from_record(12, &text), Some(state));
        assert_eq!(AppInstanceState::from_record(12, "zz"), None);

        // Another person's window, or one waiting to open here, is not this
        // device's to resume.
        let peer = Tab {
            tile: TileContent::PeerWindow {
                app_id: "org.example.Foreign".into(),
                label: String::new(),
                note: String::new(),
                missing: None,
            },
            ..foreign
        };
        assert_eq!(launch_for_tab(&peer, |_| None), None);
    }
}
