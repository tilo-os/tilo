//! Parser-free administrative read protocol. Serializable messages confer no device authority.
use serde::{Deserialize, Deserializer, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub const SCHEMA: u32 = 3;
pub const FRAME_BYTES: usize = 65_536;
pub const CHUNK_BYTES: u32 = 1_048_576;
pub const HASH_BYTES: u64 = 1_073_741_824;
pub type NativePath = Vec<Vec<u16>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyTimes {
    /// Signed 100 ns intervals since 1601-01-01 00:00:00 UTC.
    pub last_access_100ns: i64,
    /// Signed 100 ns intervals since 1601-01-01 00:00:00 UTC.
    pub last_data_change_100ns: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyTimesUnavailableReason {
    MissingStandardInformation,
    DuplicateStandardInformation,
    UnsupportedStandardInformationLayout,
    InvalidStandardInformationCoverage,
}
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
/// Converts signed NTFS 100 ns units to a host time without rounding or clamping.
#[must_use]
pub fn ntfs_time_to_system_time(value: i64) -> Option<SystemTime> {
    const NTFS_UNIX_EPOCH_100NS: i128 = 116_444_736_000_000_000;
    const UNITS_PER_SECOND: i128 = 10_000_000;
    let from_unix_epoch = i128::from(value).checked_sub(NTFS_UNIX_EPOCH_100NS)?;
    let (after_epoch, magnitude) = if from_unix_epoch < 0 {
        (false, from_unix_epoch.checked_neg()?)
    } else {
        (true, from_unix_epoch)
    };
    let seconds = u64::try_from(magnitude / UNITS_PER_SECOND).ok()?;
    let nanos = u32::try_from((magnitude % UNITS_PER_SECOND).checked_mul(100)?).ok()?;
    let duration = Duration::new(seconds, nanos);
    if after_epoch {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub volume: String,
    pub root: String,
    pub upcase: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub token: String,
    pub path: NativePath,
    pub key: NativePath,
    pub record: u64,
    pub sequence: u16,
    pub size: u64,
    pub directory: bool,
    pub metadata_digest: String,
    pub content_readable: bool,
    pub enumeration_complete: bool,
    #[serde(deserialize_with = "required_option")]
    pub copy_times: Option<CopyTimes>,
    #[serde(deserialize_with = "required_option")]
    pub copy_times_unavailable_reason: Option<CopyTimesUnavailableReason>,
    pub fidelity_pending: Vec<String>,
}
impl Entry {
    #[must_use]
    pub fn copy_times_valid(&self) -> bool {
        self.copy_times.is_some() != self.copy_times_unavailable_reason.is_some()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub path: Option<NativePath>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub nonce: String,
    pub id: u64,
    pub operation: Operation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Operation {
    Hello,
    BeginObservation,
    NextManifestPage,
    ReadChunk {
        token: String,
        offset: u64,
        length: u32,
    },
    FinishObservation,
    Cancel,
    Close,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub schema: u32,
    pub nonce: String,
    pub id: u64,
    pub result: Response,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", deny_unknown_fields)]
pub enum Response {
    Hello {
        identity: Identity,
        client_uid: u32,
        backend: String,
        acceptance: String,
    },
    Begun {
        generation: u64,
    },
    Page {
        entries: Vec<Entry>,
        issues: Vec<Issue>,
        upcase: Vec<u16>,
        done: bool,
    },
    Data {
        offset: u32,
        bytes: Vec<u8>,
    },
    ReadDone {
        length: u32,
        sha256: String,
    },
    Finished {
        hashes: Vec<(String, String)>,
        issues: Vec<Issue>,
        done: bool,
    },
    Closed,
}
#[derive(Clone, Debug)]
pub struct Manifest {
    pub generation: u64,
    pub entries: Vec<Entry>,
    pub issues: Vec<Issue>,
    pub upcase: Vec<u16>,
}
#[cfg(target_os = "linux")]
pub mod client;
#[cfg(target_os = "linux")]
pub mod socket;
#[derive(Default)]
pub struct Sequence {
    next: u64,
    invalid: bool,
}
impl Sequence {
    /// # Errors
    /// Permanently invalidates stale, skipped or wrong-session requests.
    pub fn accept(&mut self, r: &Request, nonce: &str) -> std::io::Result<()> {
        if self.invalid
            || r.schema != SCHEMA
            || r.nonce != nonce
            || r.id != self.next
            || self.next == u64::MAX
        {
            self.invalid = true;
            return Err(std::io::Error::other("session sequence invalidated"));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("request overflow"))?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn protocol_times_and_entries_are_typed_and_sessions_refuse_replay() {
        // Phase `ntfs_time_conversion_preserves_pre_epoch_and_fractional_100ns_values`.
        {
            assert_eq!(
                UNIX_EPOCH
                    .duration_since(ntfs_time_to_system_time(116_444_735_999_999_999).unwrap())
                    .unwrap(),
                Duration::new(0, 100)
            );
            assert_eq!(
                ntfs_time_to_system_time(116_444_736_012_345_678)
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap(),
                Duration::new(1, 234_567_800)
            );
        }
        // Phase `entry_wire_requires_typed_copy_time_availability`.
        {
            let missing = serde_json::json!({
                "token":"", "path":[], "key":[], "record":5, "sequence":1,
                "size":0, "directory":true, "metadata_digest":"digest",
                "content_readable":false, "enumeration_complete":true,
                "fidelity_pending":[]
            });
            assert!(serde_json::from_value::<Entry>(missing).is_err());

            let available = serde_json::json!({
                "token":"", "path":[], "key":[], "record":5, "sequence":1,
                "size":0, "directory":true, "metadata_digest":"digest",
                "content_readable":false, "enumeration_complete":true,
                "copy_times": {
                    "last_access_100ns": 116_444_736_012_345_678_i64,
                    "last_data_change_100ns": 116_444_735_999_999_999_i64
                },
                "copy_times_unavailable_reason": null,
                "fidelity_pending":[]
            });
            let entry: Entry = serde_json::from_value(available).unwrap();
            assert!(entry.copy_times_valid());
            let mut neither = entry.clone();
            neither.copy_times = None;
            assert!(!neither.copy_times_valid());
            let mut both = entry;
            both.copy_times_unavailable_reason =
                Some(CopyTimesUnavailableReason::InvalidStandardInformationCoverage);
            assert!(!both.copy_times_valid());
        }
        // Phase `wire_schema_two_is_permanently_refused`.
        {
            let mut sequence = Sequence::default();
            let old = Request {
                schema: 2,
                nonce: "selected".into(),
                id: 0,
                operation: Operation::Hello,
            };
            assert!(sequence.accept(&old, "selected").is_err());
            let current = Request {
                schema: SCHEMA,
                ..old
            };
            assert!(sequence.accept(&current, "selected").is_err());
        }
        // Phase `duplicate_request_permanently_invalidates_session`.
        {
            let mut s = Sequence::default();
            let r = Request {
                schema: SCHEMA,
                nonce: "selected".into(),
                id: 0,
                operation: Operation::Hello,
            };
            s.accept(&r, "selected").unwrap();
            assert!(s.accept(&r, "selected").is_err());
            let r = Request { id: 1, ..r };
            assert!(s.accept(&r, "selected").is_err());
        }
        // Phase `foreign_nonce_cannot_authorize`.
        {
            let mut s = Sequence::default();
            let r = Request {
                schema: SCHEMA,
                nonce: "other".into(),
                id: 0,
                operation: Operation::Hello,
            };
            assert!(s.accept(&r, "selected").is_err());
        }
    }
}
