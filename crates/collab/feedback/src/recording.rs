//! A recording a report carries: a stretch of the history of the record the
//! report's window shows (`docs/architecture/session-replay.md`).
//!
//! The recording itself is a document built by
//! [`tilo_resource::recording::record`]; the report carries only its
//! reference, and its bytes travel as the report's content, like its picture.

use serde::{Deserialize, Serialize};
use tilo_resource::recording::{Limits, Recording};

use crate::Error;

/// How far back a recording reaches from the moment it is made.
pub const RECORDING_WINDOW_MS: i64 = 10 * 60 * 1000;

/// How large a recording a report carries may be.
pub const LIMITS: Limits = Limits {
    steps: 2_000,
    bytes: 16 * 1024 * 1024,
};

/// The recording a report carries, as the report names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct RecordingRef {
    /// BLAKE3 of the recording's bytes.
    pub hash: String,
    /// The recorded app's id, which opens the recording.
    pub app: String,
    /// The recorded app's display name.
    pub app_name: String,
    /// The kind of record it recorded.
    pub kind: String,
    /// Milliseconds since the Unix epoch of its start state.
    pub from_ms: i64,
    /// Milliseconds since the Unix epoch of its last step.
    pub to_ms: i64,
    /// Steps after the start state.
    pub steps: u32,
    /// Size of its bytes.
    pub bytes: u32,
}

impl RecordingRef {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let short = |value: &str| {
            !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        let steps = usize::try_from(self.steps).unwrap_or(usize::MAX);
        let bytes = usize::try_from(self.bytes).unwrap_or(usize::MAX);
        if self.hash.len() != 64
            || !self.hash.bytes().all(|c| c.is_ascii_hexdigit())
            || !short(&self.app)
            || !short(&self.app_name)
            || !short(&self.kind)
            || self.from_ms > self.to_ms
            || steps > LIMITS.steps
            || bytes == 0
            || bytes > LIMITS.bytes
        {
            return Err(Error::Invalid("recording"));
        }
        Ok(())
    }
}

/// What a recording of the report's window would hold, shown before the
/// person chooses to include it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingScope {
    /// The recorded app's display name.
    pub app_name: String,
    /// Milliseconds since the Unix epoch of its start state.
    pub from_ms: i64,
    /// Milliseconds since the Unix epoch of its last step.
    pub to_ms: i64,
    /// Steps after the start state.
    pub steps: usize,
}

/// A recording made on this device: its reference and its bytes. The bytes
/// leave the device only once the report that names them is queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    reference: RecordingRef,
    bytes: Vec<u8>,
}

impl Recorded {
    /// `recording` of a record of kind `kind` in the app `app`, called
    /// `app_name`.
    ///
    /// # Errors
    /// Refuses a recording beyond [`LIMITS`] or with invalid names.
    pub fn new(
        app: &str,
        app_name: &str,
        kind: &str,
        recording: &Recording,
    ) -> Result<Self, Error> {
        let bytes = recording.bytes();
        let reference = RecordingRef {
            hash: blake3::hash(&bytes).to_hex().to_string(),
            app: app.to_owned(),
            app_name: app_name.to_owned(),
            kind: kind.to_owned(),
            from_ms: recording.from_ms,
            to_ms: recording.to_ms,
            steps: u32::try_from(recording.step_count())
                .map_err(|_| Error::Invalid("recording"))?,
            bytes: u32::try_from(bytes.len()).map_err(|_| Error::Invalid("recording"))?,
        };
        reference.validate()?;
        Ok(Self { reference, bytes })
    }

    #[must_use]
    pub fn reference(&self) -> &RecordingRef {
        &self.reference
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// What the recording holds, as the composer shows it.
    #[must_use]
    pub fn scope(&self) -> RecordingScope {
        RecordingScope {
            app_name: self.reference.app_name.clone(),
            from_ms: self.reference.from_ms,
            to_ms: self.reference.to_ms,
            steps: usize::try_from(self.reference.steps).unwrap_or(usize::MAX),
        }
    }
}

/// The recording `bytes` that `reference` names, read for replay.
///
/// # Errors
/// Refuses bytes that do not hash to the reference, or that are no
/// recording within [`LIMITS`].
pub fn open(reference: &RecordingRef, bytes: &[u8]) -> Result<Recording, Error> {
    reference.validate()?;
    if blake3::hash(bytes).to_hex().as_str() != reference.hash {
        return Err(Error::Invalid("recording content"));
    }
    Recording::load(bytes, &LIMITS).map_err(|_| Error::Invalid("recording content"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::transaction::Transactable as _;
    use tilo_resource::history::commit_stamped_at;
    use tilo_resource::model::Doc;

    /// A note edited before and during the last ten minutes before
    /// second 1,000.
    pub(crate) fn note() -> Doc {
        let mut doc = Doc::new();
        for (second, body) in [(10, "a"), (700, "ab"), (800, "abc"), (900, "abcd")] {
            doc.put(ROOT, "body", body).unwrap();
            commit_stamped_at(&mut doc, second).unwrap();
        }
        doc
    }

    pub(crate) fn recorded() -> Recorded {
        let mut doc = note();
        let heads = doc.get_heads();
        let recording = tilo_resource::recording::record(
            &doc,
            1_000_000 - RECORDING_WINDOW_MS,
            &heads,
            &LIMITS,
        )
        .unwrap();
        Recorded::new("so.tilo.notes", "Notes", "notes.note", &recording).unwrap()
    }

    #[test]
    fn a_recording_is_named_by_its_bytes_and_opens_only_from_them() {
        let recorded = recorded();
        let reference = recorded.reference();
        assert_eq!(reference.steps, 3, "the edits of the last ten minutes");
        assert_eq!((reference.from_ms, reference.to_ms), (400_000, 900_000));
        assert_eq!(
            recorded.scope(),
            RecordingScope {
                app_name: "Notes".into(),
                from_ms: 400_000,
                to_ms: 900_000,
                steps: 3,
            }
        );
        let opened = open(reference, recorded.bytes()).unwrap();
        assert_eq!(opened.step_count(), 3);
        let mut other = recorded.bytes().to_vec();
        other.push(0);
        assert!(open(reference, &other).is_err());
        let mut forged = reference.clone();
        forged.steps = u32::MAX;
        assert!(open(&forged, recorded.bytes()).is_err());
    }
}
