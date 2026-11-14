//! One change to a file, as history groups versions into it and says what it
//! did.
//!
//! A file's history is a chain of versions, not a chain of things a person
//! did: saving once can write several versions, an editor that saves
//! atomically writes a temporary and renames it over the original, and an
//! agent session writes as many versions as it takes. This module is the one
//! reading that turns versions into changes: it coalesces versions a writer
//! made as one change, and it describes the change from the content it read,
//! with no model involved.
//!
//! What it stores is facts ([`FileChangeFacts`]) — lines added and removed,
//! the first line added, the sizes before and after — and the words a row
//! shows are rendered from those facts per reader. A summary a model made on
//! the device is stored beside them ([`FileChange::summary`]), and never
//! blocks: the facts alone always describe the change. A person's later opt-in
//! "why" has its field ([`FileChange::why`]) and is not written yet.
//!
//! Grouping lives here, above the signer, because every reader — the desktop,
//! a paired laptop, a browser page — reads the same versions through this
//! module and must group them identically. Nothing here is authority: it says
//! what a version chain did, never who may change it.

use std::collections::BTreeMap;
use std::time::Duration;

use fs_protocol::{FileAttributionV1, FileProvenance, FileVersionId, FileVersionV1};

use crate::i18n::widget_format;

/// How long a writer must stay quiet before the next version it writes is a
/// new change. A save that writes several versions leaves them closer than
/// this; two separate edits of the same file, further apart, read as two.
pub const QUIET: Duration = Duration::from_secs(5);

/// How long after renaming a temporary over a file the content write still
/// belongs to the same atomic save.
pub const ATOMIC_SAVE: Duration = Duration::from_secs(30);

/// Most lines this module diffs. Past it the change is described by its test
/// and sizes, never by a diff nobody asked to hold in memory.
pub const MAX_DIFF_LINES: usize = 20_000;

/// The writer of a version, as the grouping key that decides what coalesces:
/// the actor the signing device recorded, or the device itself for the person
/// at it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WriterKey {
    /// `agent:claude:<session>`, `agent:<id>`, `app:<id>`, `program:<id>`.
    Actor(String),
    /// The person at the device that signed the version.
    Device(String),
}

impl WriterKey {
    /// The key of one version, from the attribution beside it.
    #[must_use]
    pub fn of(version: &FileVersionV1, attribution: Option<&FileAttributionV1>) -> Self {
        match attribution.filter(|attribution| attribution.validate().is_ok()) {
            Some(attribution) => Self::Actor(attribution.actor.clone()),
            // The person *and* the device: a version nobody was attributed
            // for is the person at that device.
            None => Self::Device(format!(
                "{}:{}",
                hex_device(&version.actor.person.0),
                hex_device(&version.actor.device.0)
            )),
        }
    }

    /// Whether this writer is a coding agent's session (`agent:<kind>:<id>`),
    /// whose whole session is one run in history.
    #[must_use]
    pub fn is_agent_session(&self) -> bool {
        let Self::Actor(actor) = self else {
            return false;
        };
        let Some(writer) = actor.strip_prefix("agent:") else {
            return false;
        };
        writer.contains(':')
    }
}

fn hex_device(device: &[u8; 32]) -> String {
    hex::encode(device)
}

/// A turn boundary an agent's run can be cut at, when the host knows one (an
/// agent hook reporting that its turn ended). A run that reports none stays
/// one change: an agent session is one run in history.
pub trait TurnBoundary {
    /// Whether `version` starts a new turn, so a session's run so far closes
    /// before it.
    fn ends_turn(&self, writer: &WriterKey, version: &FileVersionV1) -> bool;
}

/// No hook knows where an agent's turns end.
pub struct NoTurnBoundary;

impl TurnBoundary for NoTurnBoundary {
    fn ends_turn(&self, _: &WriterKey, _: &FileVersionV1) -> bool {
        false
    }
}

/// What a change did to the file's content, as facts every reader words for
/// itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Text {
        lines_added: usize,
        lines_removed: usize,
    },
    /// A file whose content is not text: only its sizes describe it.
    Binary {
        bytes_before: Option<u64>,
        bytes_after: Option<u64>,
    },
    /// Content too large or unreadable to compare: sizes only, when known.
    Uncomparable {
        bytes_before: Option<u64>,
        bytes_after: Option<u64>,
    },
}

/// Structured facts about one change, stored so the words a row shows are
/// rendered per reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChangeFacts {
    pub kind: ChangeKind,
    /// The first line the change added, when it added one: the row shows it
    /// after the sentence.
    pub first_added_line: Option<String>,
}

impl FileChangeFacts {
    /// The facts of a change between two contents, `before` absent for a file
    /// the change created.
    #[must_use]
    pub fn from_contents(before: Option<&[u8]>, after: Option<&[u8]>) -> Self {
        let sizes = (
            before.map(|bytes| bytes.len() as u64),
            after.map(|bytes| bytes.len() as u64),
        );
        let (Some(before), Some(after)) = (before, after) else {
            // A creation or a deletion with no other side: sizes alone.
            return Self {
                kind: ChangeKind::Binary {
                    bytes_before: sizes.0,
                    bytes_after: sizes.1,
                },
                first_added_line: None,
            };
        };
        let (Some(before_text), Some(after_text)) = (text_of(before), text_of(after)) else {
            return Self {
                kind: ChangeKind::Binary {
                    bytes_before: sizes.0,
                    bytes_after: sizes.1,
                },
                first_added_line: None,
            };
        };
        Self::from_text(before_text, after_text, sizes)
    }

    fn from_text(before: &str, after: &str, sizes: (Option<u64>, Option<u64>)) -> Self {
        let before_lines: Vec<&str> = before.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        if before_lines.len() > MAX_DIFF_LINES || after_lines.len() > MAX_DIFF_LINES {
            return Self {
                kind: ChangeKind::Uncomparable {
                    bytes_before: sizes.0,
                    bytes_after: sizes.1,
                },
                first_added_line: None,
            };
        }
        let prefix = before_lines
            .iter()
            .zip(&after_lines)
            .take_while(|(was, is)| was == is)
            .count();
        let suffix = before_lines
            .iter()
            .rev()
            .zip(after_lines.iter().rev())
            .take_while(|(was, is)| was == is)
            .count()
            .min(before_lines.len().saturating_sub(prefix))
            .min(after_lines.len().saturating_sub(prefix));
        let removed = before_lines.len() - prefix - suffix;
        let added = after_lines.len() - prefix - suffix;
        let first_added_line = (added > 0)
            .then(|| after_lines.get(prefix).map(|line| (*line).to_owned()))
            .flatten();
        Self {
            kind: ChangeKind::Text {
                lines_added: added,
                lines_removed: removed,
            },
            first_added_line,
        }
    }

    /// What the change did, in this thread's words: "Added a line to
    /// some-doc.md: the line", "Changed 2 lines in some-doc.md", "Replaced
    /// photo.jpg, 2.1 MB → 2.4 MB". `None` when the facts cannot say.
    #[must_use]
    pub fn sentence(&self, file: &str) -> Option<String> {
        let file_arg = || crate::i18n::ArgValue::from(file);
        let sentence = match self.kind {
            ChangeKind::Text {
                lines_added: 0,
                lines_removed: 0,
            } => return None,
            ChangeKind::Text {
                lines_added: 0,
                lines_removed,
            } => one_or_many(
                lines_removed,
                "history-file-removed-line",
                "history-file-removed-lines",
                "lines",
                file,
            ),
            ChangeKind::Text {
                lines_added,
                lines_removed: 0,
            } => one_or_many(
                lines_added,
                "history-file-added-line",
                "history-file-added-lines",
                "lines",
                file,
            ),
            ChangeKind::Text {
                lines_added,
                lines_removed: _,
            } => {
                let lines = lines_added.max(1);
                if lines == 1 {
                    widget_format("history-file-changed-line", &[("file", file_arg())])
                } else {
                    widget_format(
                        "history-file-changed-lines",
                        &[("file", file_arg()), ("lines", lines.to_string().into())],
                    )
                }
            }
            ChangeKind::Binary {
                bytes_before: None,
                bytes_after: Some(after),
            } => widget_format(
                "history-file-added-binary",
                &[
                    ("file", file_arg()),
                    ("size", crate::format::size(after).into()),
                ],
            ),
            ChangeKind::Binary {
                bytes_before: Some(before),
                bytes_after: None,
            } => widget_format(
                "history-file-removed-binary",
                &[
                    ("file", file_arg()),
                    ("size", crate::format::size(before).into()),
                ],
            ),
            ChangeKind::Binary {
                bytes_before: Some(before),
                bytes_after: Some(after),
            } => widget_format(
                "history-file-replaced-binary",
                &[
                    ("file", file_arg()),
                    ("before", crate::format::size(before).into()),
                    ("after", crate::format::size(after).into()),
                ],
            ),
            ChangeKind::Binary {
                bytes_before: None,
                bytes_after: None,
            }
            | ChangeKind::Uncomparable { .. } => {
                widget_format("history-file-changed", &[("file", file_arg())])
            }
        };
        Some(match (&self.first_added_line, &self.kind) {
            (Some(line), ChangeKind::Text { .. }) => widget_format(
                "history-file-added-line-quote",
                &[
                    ("sentence", sentence.into()),
                    ("line", line.as_str().into()),
                ],
            ),
            _ => sentence,
        })
    }
}

fn one_or_many(count: usize, one: &str, many: &str, arg: &str, file: &str) -> String {
    if count == 1 {
        widget_format(one, &[("file", file.into())])
    } else {
        widget_format(
            many,
            &[("file", file.into()), (arg, count.to_string().into())],
        )
    }
}

fn text_of(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then_some(text)
}

/// One change: the versions a writer made of a file as one thing a person did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// The attribution the coalesced versions shared, when they shared one.
    pub writer: Option<FileAttributionV1>,
    /// The versions this change wrote, oldest first.
    pub versions: Vec<FileVersionV1>,
    /// What it did to the content, when the content was read.
    pub facts: Option<FileChangeFacts>,
    /// A summary a model made on the device, when one was made. Never
    /// required: [`Self::sentence`] falls back to the facts.
    pub summary: Option<String>,
    /// A person's own account of why, when they choose to give one.
    pub why: Option<String>,
}

impl FileChange {
    /// The first version of the change.
    #[must_use]
    pub fn first(&self) -> &FileVersionV1 {
        &self.versions[0]
    }

    /// The last version of the change.
    #[must_use]
    pub fn last(&self) -> &FileVersionV1 {
        &self.versions[self.versions.len() - 1]
    }

    /// The version this change replaced: what to restore to take it back.
    /// `None` when the change created the file.
    #[must_use]
    pub fn parent_before(&self) -> Option<FileVersionId> {
        self.first().parents.first().copied()
    }

    /// Whether this change is a save that renamed a temporary over the file.
    #[must_use]
    pub fn is_atomic_save(&self) -> bool {
        self.versions.len() > 1
            && matches!(
                self.first().provenance,
                FileProvenance::Rename | FileProvenance::Move
            )
            && self
                .versions
                .iter()
                .skip(1)
                .any(|version| version.provenance == FileProvenance::Edit)
    }

    /// What the change did, as a row says it: the device's summary when there
    /// is one, else the facts in this reader's words.
    #[must_use]
    pub fn sentence(&self, file: &str) -> Option<String> {
        self.summary
            .clone()
            .or_else(|| self.facts.as_ref().and_then(|facts| facts.sentence(file)))
    }

    /// The versions this change wrote, as the ids an undo names.
    #[must_use]
    pub fn version_ids(&self) -> Vec<FileVersionId> {
        self.versions.iter().map(|version| version.id).collect()
    }
}

/// Coalesce a file's versions, oldest first, into the changes a history shows:
/// consecutive versions by one writer, closer than `quiet`, as one change; an
/// agent session's versions as one run until a turn boundary closes it. A
/// save that renames a temporary over the file and then writes its content is
/// one change whatever the gap between the rename and the write.
#[must_use]
pub fn coalesce(
    versions: &[FileVersionV1],
    attributions: &BTreeMap<FileVersionId, FileAttributionV1>,
    quiet: Duration,
    boundary: &dyn TurnBoundary,
) -> Vec<FileChange> {
    let mut ordered: Vec<&FileVersionV1> = versions.iter().collect();
    ordered.sort_by_key(|version| (version.timestamp_ms, version.id));
    let mut changes: Vec<FileChange> = Vec::new();
    let mut open: Option<WriterKey> = None;
    for version in ordered {
        let attribution = attributions.get(&version.id);
        let writer = WriterKey::of(version, attribution);
        let continues = changes.last().is_some_and(|change| {
            if open.as_ref() != Some(&writer) {
                return false;
            }
            let last = change.last();
            let elapsed =
                Duration::from_millis(version.timestamp_ms.saturating_sub(last.timestamp_ms));
            if boundary.ends_turn(&writer, version) {
                return false;
            }
            if writer.is_agent_session() {
                // One run: the session's versions belong together however long
                // it works between them.
                return true;
            }
            elapsed <= quiet
                || (elapsed <= ATOMIC_SAVE
                    && matches!(
                        last.provenance,
                        FileProvenance::Rename | FileProvenance::Move
                    )
                    && version.provenance == FileProvenance::Edit)
        });
        if continues && let Some(change) = changes.last_mut() {
            change.versions.push(version.clone());
        } else {
            changes.push(FileChange {
                writer: attribution.filter(|a| a.validate().is_ok()).cloned(),
                versions: vec![version.clone()],
                facts: None,
                summary: None,
                why: None,
            });
        }
        open = Some(writer);
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::{ContentHash, FileActor, FileVersionId};
    use uuid::Uuid;

    fn version(
        id: u128,
        timestamp_ms: u64,
        device: [u8; 32],
        provenance: FileProvenance,
    ) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId(Uuid::from_u128(id)),
            record_id: fs_protocol::FileRecordId(Uuid::from_u128(99)),
            parents: Vec::new(),
            actor: FileActor {
                person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                device: collab_types::Ed25519PublicKey::from_bytes(device),
            },
            timestamp_ms,
            content_hash: Some(ContentHash([2; 32])),
            metadata_hash: ContentHash([3; 32]),
            provenance,
        }
    }

    fn attribution(actor: &str, label: &str) -> FileAttributionV1 {
        FileAttributionV1 {
            actor: actor.into(),
            label: label.into(),
            color: [10, 20, 30],
        }
    }

    /// Versions by one writer within the quiet window are one change; further
    /// apart they are two; another writer always starts a new one.
    #[test]
    fn coalescing_groups_one_writer_within_the_quiet_window() {
        let device = [7; 32];
        let versions = vec![
            version(1, 1_000, device, FileProvenance::Edit),
            version(2, 2_000, device, FileProvenance::Edit),
            version(3, 20_000, device, FileProvenance::Edit),
        ];
        let changes = coalesce(&versions, &BTreeMap::new(), QUIET, &NoTurnBoundary);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].versions.len(), 2, "the save's two versions");
        assert_eq!(changes[1].versions.len(), 1, "a later edit");

        let mut attributions = BTreeMap::new();
        attributions.insert(versions[1].id, attribution("app:so.tilo.notes", "Notes"));
        let changes = coalesce(&versions, &attributions, QUIET, &NoTurnBoundary);
        assert_eq!(changes.len(), 3, "another writer split the save");
        assert_eq!(
            changes[1].writer.as_ref().map(|a| a.actor.as_str()),
            Some("app:so.tilo.notes")
        );
        assert_eq!(changes[1].versions.len(), 1);
    }

    /// An atomic save — a temporary renamed over the file, then its content
    /// written — is one change even when the content write comes later.
    #[test]
    fn an_atomic_save_is_one_change() {
        let device = [7; 32];
        let versions = vec![
            version(1, 1_000, device, FileProvenance::Rename),
            version(2, 20_000, device, FileProvenance::Edit),
        ];
        let changes = coalesce(&versions, &BTreeMap::new(), QUIET, &NoTurnBoundary);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].is_atomic_save());
        assert_eq!(changes[0].versions.len(), 2);

        let untouched = vec![
            version(1, 1_000, device, FileProvenance::Rename),
            version(2, 60_000, device, FileProvenance::Edit),
        ];
        let changes = coalesce(&untouched, &BTreeMap::new(), QUIET, &NoTurnBoundary);
        assert_eq!(changes.len(), 2, "past the atomic-save window");
        assert!(!changes[0].is_atomic_save());
    }

    /// An agent session's versions are one run however long it works, until a
    /// turn boundary closes it.
    #[test]
    fn an_agent_session_is_one_run_until_a_turn_boundary() {
        let device = [7; 32];
        let versions = vec![
            version(1, 1_000, device, FileProvenance::Edit),
            version(2, 600_000, device, FileProvenance::Edit),
            version(3, 1_200_000, device, FileProvenance::Edit),
        ];
        let mut attributions = BTreeMap::new();
        for version in &versions {
            attributions.insert(
                version.id,
                attribution("agent:claude:session-1", "Report work"),
            );
        }
        let changes = coalesce(&versions, &attributions, QUIET, &NoTurnBoundary);
        assert_eq!(changes.len(), 1, "one run");
        assert_eq!(changes[0].versions.len(), 3);
        assert_eq!(changes[0].version_ids().len(), 3);

        struct Turns;
        impl TurnBoundary for Turns {
            fn ends_turn(&self, _: &WriterKey, version: &FileVersionV1) -> bool {
                version.id == FileVersionId(Uuid::from_u128(3))
            }
        }
        let changes = coalesce(&versions, &attributions, QUIET, &Turns);
        assert_eq!(changes.len(), 2, "the hook closes the first turn");
        assert_eq!(changes[0].versions.len(), 2);
    }

    /// A few changed text lines describe themselves, with the first line the
    /// change added; binary files get their sizes.
    #[test]
    fn a_change_describes_itself_from_its_content() {
        let added = FileChangeFacts::from_contents(Some(b"one\ntwo\n"), Some(b"one\ntwo\nthree\n"));
        assert_eq!(
            added.kind,
            ChangeKind::Text {
                lines_added: 1,
                lines_removed: 0
            }
        );
        assert_eq!(added.first_added_line.as_deref(), Some("three"));
        assert_eq!(
            added.sentence("some-doc.md").as_deref(),
            Some("Added a line to some-doc.md: three")
        );

        let changed =
            FileChangeFacts::from_contents(Some(b"one\ntwo\nthree\n"), Some(b"one\nTWO\nTHREE\n"));
        assert_eq!(
            changed.kind,
            ChangeKind::Text {
                lines_added: 2,
                lines_removed: 2
            }
        );
        assert_eq!(
            changed.sentence("some-doc.md").as_deref(),
            Some("Changed 2 lines in some-doc.md: TWO")
        );

        let removed = FileChangeFacts::from_contents(Some(b"one\ntwo\nthree\n"), Some(b"one\n"));
        assert_eq!(
            removed.sentence("some-doc.md").as_deref(),
            Some("Removed 2 lines from some-doc.md")
        );

        let png = [0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        let binary = FileChangeFacts::from_contents(Some(&png[..4]), Some(&png));
        assert_eq!(
            binary.kind,
            ChangeKind::Binary {
                bytes_before: Some(4),
                bytes_after: Some(8)
            }
        );
        assert_eq!(
            binary.sentence("photo.jpg").as_deref(),
            Some("Replaced photo.jpg, 4 B → 8 B")
        );
        assert_eq!(
            FileChangeFacts::from_contents(None, Some(&[0u8; 150_000]))
                .sentence("video.mkv")
                .as_deref(),
            Some("Added video.mkv, 150 kB")
        );
    }

    /// A change with a device summary says that, and falls back to its facts
    /// when no summary was made: a summary never blocks a row.
    #[test]
    fn a_device_summary_wins_and_falls_back_to_the_facts() {
        let mut change = FileChange {
            writer: None,
            versions: vec![version(1, 1_000, [7; 32], FileProvenance::Edit)],
            facts: Some(FileChangeFacts::from_contents(
                Some(b"one\n"),
                Some(b"one\ntwo\n"),
            )),
            summary: None,
            why: None,
        };
        assert_eq!(
            change.sentence("notes.md").as_deref(),
            Some("Added a line to notes.md: two")
        );
        change.summary = Some("Drafted the opening paragraph".into());
        assert_eq!(
            change.sentence("notes.md").as_deref(),
            Some("Drafted the opening paragraph")
        );
        assert_eq!(change.why, None, "room for a later opt-in why");
    }
}
