//! A record carried beside a remark — a recording of what happened, or a
//! report — by reference, never by copy.
//!
//! A remark says something about a thing; a report often needs to show what
//! happened as well. That evidence is a record in the reporter's own room (a
//! `session` recording, a report record), and rooms hold references rather
//! than copies (`docs/architecture/rooms.md`). So an attachment names the
//! record and where it lives, and posting it brings the record into the
//! destination room through the ordinary sharing verb. Retracting removes the
//! reference; it cannot recall what a peer already synced, and nothing that
//! renders a [`RecordAttachment`] may suggest otherwise.
//!
//! The one thing copied is a bounded summary, for the same reason an image
//! attachment carries its pixel size: a reader decides whether to open the
//! record before its document has reached them.

use serde::{Deserialize, Serialize};

/// Record kind of a recorded session, as the recorder stamps it.
pub const SESSION_RECORD_KIND: &str = "session";
/// Record kind of a written-up report that is not itself a recording.
pub const REPORT_RECORD_KIND: &str = "report";

/// Bounds on collaborator-supplied attachment fields, checked by
/// [`RecordAttachment::is_valid`] before a row is written.
const MAX_ID_BYTES: usize = 1024;
const MAX_KIND_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 256;
const MAX_APPS: usize = 16;

/// A record attached to a message by reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordAttachment {
    /// The record's id, in `ui::record::RecordId`'s encoding.
    pub record: String,
    /// The record's kind — [`SESSION_RECORD_KIND`], [`REPORT_RECORD_KIND`] or
    /// an app's own.
    pub kind: String,
    /// Human label for the card. Presentation only; never the address.
    pub label: String,
    /// The room the record lives in, so a reader knows whose room it came
    /// from and the host can resolve its provenance.
    pub home: String,
    /// What the recording shows, for a `session` record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSummary>,
}

impl RecordAttachment {
    pub fn new(
        record: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            record: record.into(),
            kind: kind.into(),
            label: label.into(),
            home: home.into(),
            session: None,
        }
    }

    /// A `session` record described by `summary`.
    pub fn session(
        record: impl Into<String>,
        label: impl Into<String>,
        home: impl Into<String>,
        summary: SessionSummary,
    ) -> Self {
        Self {
            session: Some(summary),
            ..Self::new(record, SESSION_RECORD_KIND, label, home)
        }
    }

    pub fn is_session(&self) -> bool {
        self.kind == SESSION_RECORD_KIND
    }

    /// Whether every field fits the bounds a replicated row may carry. The
    /// record id is checked for shape here and again by the room index that
    /// the host writes it into.
    pub fn is_valid(&self) -> bool {
        let bounded = |s: &str, max: usize| !s.trim().is_empty() && s.len() <= max;
        bounded(&self.record, MAX_ID_BYTES)
            && self.record.contains(':')
            && bounded(&self.kind, MAX_KIND_BYTES)
            && self
                .kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            && self.label.len() <= MAX_LABEL_BYTES
            && bounded(&self.home, MAX_ID_BYTES)
            && self.session.as_ref().is_none_or(SessionSummary::is_valid)
            && (self.session.is_none() || self.is_session())
    }
}

/// Which layer a recorded problem is in, decided by replaying the trace and
/// comparing its frames with the captured ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLayer {
    /// The replayed frames match: the problem is one of logic or UX, and the
    /// trace is the report.
    Logic,
    /// The frames diverge, or the region has no semantic node: the problem is
    /// below the toolkit (renderer, fonts, driver, scaling, compositor).
    BelowToolkit,
}

/// What review took out of a recording before it left the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redactions {
    /// Text runs replaced with shape-preserving placeholders.
    pub text: u32,
    /// Screen regions masked in attached frames.
    pub regions: u32,
}

impl Redactions {
    pub fn total(&self) -> u32 {
        self.text.saturating_add(self.regions)
    }
}

/// The bounded summary a `session` card shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Ids of the apps the recording covers (`so.tilo.mail`), sorted. A reader
    /// derives the label it shows.
    pub apps: Vec<String>,
    pub duration_ms: u64,
    /// Reviewed steps in the walk-through.
    pub steps: u32,
    pub layer: SessionLayer,
    #[serde(default)]
    pub redactions: Redactions,
    /// The reporter's image generation, which a "fixed in generation N"
    /// status is matched against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

impl SessionSummary {
    fn is_valid(&self) -> bool {
        self.apps.len() <= MAX_APPS
            && self
                .apps
                .iter()
                .all(|app| !app.trim().is_empty() && app.len() <= MAX_LABEL_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> SessionSummary {
        SessionSummary {
            apps: vec!["Mail".into()],
            duration_ms: 42_000,
            steps: 5,
            layer: SessionLayer::Logic,
            redactions: Redactions {
                text: 2,
                regions: 1,
            },
            generation: Some(118),
        }
    }

    #[test]
    fn a_session_attachment_round_trips_and_validates() {
        let a = RecordAttachment::session("uuid:1234", "Send fails", "room-a", summary());
        assert!(a.is_valid());
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"layer\":\"logic\""), "{json}");
        let back: RecordAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.session.unwrap().redactions.total(), 3);
    }

    #[test]
    fn an_attachment_that_does_not_name_a_record_is_invalid() {
        assert!(!RecordAttachment::new("", "session", "x", "room").is_valid());
        assert!(!RecordAttachment::new("no-prefix", "report", "x", "room").is_valid());
        assert!(!RecordAttachment::new("uuid:1", "bad kind", "x", "room").is_valid());
        assert!(!RecordAttachment::new("uuid:1", "report", "x", "").is_valid());
        // A summary claims a recording; on another kind it is a lie.
        let mut report = RecordAttachment::new("uuid:1", "report", "x", "room");
        report.session = Some(summary());
        assert!(!report.is_valid());
    }
}
