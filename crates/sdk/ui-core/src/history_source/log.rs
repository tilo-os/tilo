//! [`HistorySource::entries`] kept across calls: a long-lived reader of a
//! growing document reads each change once.
//!
//! A change's reading (its summary, op class and text delta, whether it only
//! marked a group or run) is a function of the changes before it in the
//! document's log: the replica it is read against holds exactly those. Adding
//! changes to a document only appends to its log
//! ([`automerge::Automerge::change_hashes`]), so the readings of a log's
//! prefix stay valid and a refresh reads only what was appended. Anything
//! else (another document, a reordered log after a reload, a change of locale
//! or of the window, a replica that could not apply a change) reads the log
//! from scratch, which is what [`HistorySource::entries`] always does.

use std::collections::HashMap;

use automerge::{AutoCommit, ChangeHash};
use collab_types::OpClass;
use tilo_automerge::ApplyIncoming as _;
use tilo_doc::snapshot::DocSnapshot;

use super::{
    HistorySource, NO_VISIBLE_CHANGE, entry_id_from_hash, genesis_context, groups,
    is_store_genesis, patch_summary, runs, structural_summary, summarize_patches,
    timestamp_from_ms,
};
use crate::widget::history::{HistoryEntry, HistoryEntryId};

/// What [`HistorySource::entries_since`] has read of one document. Start
/// with [`HistoryLog::default`] and hand the same log back on every call;
/// it holds a replica of the document as of its last read.
#[derive(Default)]
pub struct HistoryLog {
    /// What the readings were made under; when it differs, they are redone.
    basis: Option<Basis>,
    /// Every change read, in the document's log order.
    changes: Vec<Logged>,
    /// Each change's position in `changes`, by its entry id; the first one
    /// on a (16-byte) collision, as [`super::version_index_for_entry`] does.
    positions: HashMap<HistoryEntryId, usize>,
    /// The frontier of `changes`.
    heads: Vec<ChangeHash>,
    /// The document as of `changes`, its diff cursor at their end.
    replica: AutoCommit,
    /// `replica`'s qdoc snapshot, the "before" of the next structural
    /// reading.
    snapshot: Option<DocSnapshot>,
    /// The replica could not apply a change; readings after it fell back to
    /// diffing the document, and the next call reads from scratch.
    failed: bool,
    /// How many changes this log has read, rebuilds included.
    reads: usize,
}

/// Everything besides the log itself that a reading depends on.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Basis {
    max_entries: usize,
    field_paths: bool,
    /// Whether the document carries the qdoc schema.
    structural: bool,
    /// Readings carry translated words.
    locale: u64,
}

/// One change of the log.
struct Logged {
    hash: ChangeHash,
    id: HistoryEntryId,
    deps: Vec<ChangeHash>,
    /// The log's frontier up to and including this change: the heads
    /// [`super::heads_for_entry`] answers for it.
    heads: Vec<ChangeHash>,
    actor: String,
    time_ms: i64,
    message: Option<String>,
    /// What the change did; `None` before the first window read, which no
    /// later window reaches.
    reading: Option<Reading>,
}

struct Reading {
    summary: String,
    op_class: OpClass,
    text_delta: Option<(usize, usize)>,
    /// The change only named a version or recorded a run: no row.
    marker: bool,
    /// The root change of a store-created document.
    store_genesis: bool,
}

/// A change's metadata, owned.
struct Meta {
    hash: ChangeHash,
    deps: Vec<ChangeHash>,
    actor: String,
    time_ms: i64,
    message: Option<String>,
}

impl Meta {
    fn of(meta: &automerge::ChangeMetadata<'_>) -> Self {
        Self {
            hash: meta.hash,
            deps: meta.deps.clone(),
            actor: meta.actor.to_hex_string(),
            time_ms: meta.timestamp.saturating_mul(1000),
            message: meta.message.as_deref().map(str::to_owned),
        }
    }
}

impl HistoryLog {
    /// How many changes this log has read so far, from scratch or appended:
    /// what a reader that should read each change once can be held to.
    #[must_use]
    pub const fn changes_read(&self) -> usize {
        self.reads
    }

    /// The hash and dependencies of the change behind `id`, when this log
    /// has read it.
    #[must_use]
    pub fn change(&self, id: &HistoryEntryId) -> Option<(ChangeHash, &[ChangeHash])> {
        let logged = self.changes.get(*self.positions.get(id)?)?;
        Some((logged.hash, logged.deps.as_slice()))
    }

    /// The document's heads as of the change behind `id`: what
    /// [`super::heads_for_entry`] answers for the document last read.
    #[must_use]
    pub fn heads_for_entry(&self, id: &HistoryEntryId) -> Option<Vec<ChangeHash>> {
        let logged = self.changes.get(*self.positions.get(id)?)?;
        Some(logged.heads.clone())
    }

    /// Bring the log up to `doc`'s committed log: read what was appended,
    /// or everything when the log no longer is a prefix of it.
    fn follow(&mut self, source: &HistorySource, doc: &mut AutoCommit) {
        let basis = Basis {
            max_entries: source.max_entries,
            field_paths: source.field_paths,
            // What `tilo_doc::snapshot::of` checks before it materialises
            // the whole document.
            structural: tilo_doc::model::open(doc, &automerge::ROOT).is_ok(),
            locale: crate::i18n::I18nManager::revision(),
        };
        let appended = {
            let log = doc.committed_document().change_hashes();
            let read = self.changes.len();
            let prefix = self.basis == Some(basis)
                && !self.failed
                && log.len() >= read
                && log
                    .iter()
                    .zip(&self.changes)
                    .all(|(hash, logged)| *hash == logged.hash);
            prefix.then(|| log.get(read..).unwrap_or_default().to_vec())
        };
        match appended {
            Some(appended) if appended.is_empty() => {}
            Some(appended) => {
                if !self.append(source, doc, &appended) {
                    self.rebuild(source, doc, basis);
                }
            }
            None => self.rebuild(source, doc, basis),
        }
    }

    /// Read `doc`'s whole log as [`HistorySource::entries`] always did: the
    /// changes before the window are applied to the replica in one batch,
    /// and only the window is read.
    fn rebuild(&mut self, source: &HistorySource, doc: &mut AutoCommit, basis: Basis) {
        *self = Self {
            basis: Some(basis),
            reads: self.reads,
            ..Self::default()
        };
        let metas: Vec<Meta> = doc.get_changes_meta(&[]).iter().map(Meta::of).collect();
        let start = metas.len().saturating_sub(basis.max_entries);
        let mut changes = doc.get_changes(&[]).into_iter();
        let prefix: Vec<_> = changes.by_ref().take(start).collect();
        let mut metas = metas.into_iter();
        for meta in metas.by_ref().take(start) {
            self.push(meta, None);
        }
        self.failed = !prefix.is_empty() && self.replica.apply_incoming(prefix).is_err();
        // Every later change is then indexed as it applies, so each one's
        // patches are read without diffing the whole document.
        self.replica.update_diff_cursor();
        self.snapshot = if start == 0 {
            Some(DocSnapshot::default())
        } else {
            self.replica_snapshot()
        };
        for (meta, change) in metas.zip(changes) {
            self.read(source, doc, meta, change);
        }
    }

    /// Read the changes `appended` names, which `doc` added after the log.
    /// `false` when the document does not hand them back as named.
    fn append(
        &mut self,
        source: &HistorySource,
        doc: &mut AutoCommit,
        appended: &[ChangeHash],
    ) -> bool {
        let metas: Vec<Meta> = doc
            .get_changes_meta(&self.heads)
            .iter()
            .map(Meta::of)
            .collect();
        let changes = doc.get_changes(&self.heads);
        let named = metas
            .iter()
            .map(|meta| meta.hash)
            .eq(appended.iter().copied())
            && changes
                .iter()
                .map(automerge::Change::hash)
                .eq(appended.iter().copied());
        if !named {
            return false;
        }
        for (meta, change) in metas.into_iter().zip(changes) {
            self.read(source, doc, meta, change);
        }
        true
    }

    /// Apply the next change of the log to the replica and read what it did.
    fn read(
        &mut self,
        source: &HistorySource,
        doc: &mut AutoCommit,
        meta: Meta,
        change: automerge::Change,
    ) {
        self.reads = self.reads.saturating_add(1);
        let patches = if self.failed || self.replica.apply_incoming([change]).is_err() {
            self.failed = true;
            None
        } else {
            Some(self.replica.diff_incremental())
        };
        let current = self.replica_snapshot();
        let before = self.heads.clone();
        let (hash, time_ms) = (meta.hash, meta.time_ms);
        self.push(meta, None);
        let after = self.heads.clone();
        let (summary, op_class, text_delta) = match (&self.snapshot, &current, &patches) {
            (Some(prev), Some(cur), _) => structural_summary(prev, cur),
            (_, _, Some(patches)) => summarize_patches(&self.replica, patches, source.field_paths),
            _ => patch_summary(doc, &before, &after, source.field_paths),
        };
        self.snapshot = current;
        // Naming a version (or recording a cross-app run) is a change but
        // not a row — the name is already on screen as the band's title.
        // Checked only when there's no other summary, so the common path
        // pays no extra diff.
        let marker = summary == NO_VISIBLE_CHANGE && {
            let patches = patches.unwrap_or_else(|| doc.diff(&before, &after));
            groups::is_group_marker(&patches) || runs::is_run_marker(&patches)
        };
        let store_genesis = is_store_genesis(doc, time_ms, &hash);
        if let Some(logged) = self.changes.last_mut() {
            logged.reading = Some(Reading {
                summary,
                op_class,
                text_delta,
                marker,
                store_genesis,
            });
        }
    }

    /// Add `meta` to the log and move its frontier past it. The log is in
    /// causal order, so a change's dependencies are already in it: they stop
    /// being heads.
    fn push(&mut self, meta: Meta, reading: Option<Reading>) {
        self.heads.retain(|head| !meta.deps.contains(head));
        self.heads.push(meta.hash);
        let id = entry_id_from_hash(&meta.hash.to_string());
        self.positions.entry(id).or_insert(self.changes.len());
        self.changes.push(Logged {
            hash: meta.hash,
            id,
            deps: meta.deps,
            heads: self.heads.clone(),
            actor: meta.actor,
            time_ms: meta.time_ms,
            message: meta.message,
            reading,
        });
    }

    /// The replica's qdoc snapshot, when the document carries that schema.
    fn replica_snapshot(&self) -> Option<DocSnapshot> {
        (self.basis.is_some_and(|basis| basis.structural) && !self.failed)
            .then(|| tilo_doc::snapshot::of(&self.replica))
            .flatten()
    }

    /// The rows of the window, as of the last [`Self::follow`].
    fn entries(&self, source: &HistorySource, doc: &AutoCommit) -> Vec<HistoryEntry> {
        let start = self.changes.len().saturating_sub(source.max_entries);
        let genesis_actor = crate::record::genesis_actor(doc).map(|actor| actor.to_hex_string());
        let schema_genesis = tilo_doc::model::schema_genesis_actor_hex();
        let mut out = Vec::with_capacity(self.changes.len().saturating_sub(start));
        for logged in self.changes.iter().skip(start) {
            let Some(reading) = &logged.reading else {
                continue;
            };
            // The schema genesis is a change nobody made — its containers are
            // written by a fixed actor so two independently created documents
            // merge instead of shadowing each other. Listing it would attribute
            // a revision to a nonexistent person.
            if logged.actor == schema_genesis {
                continue;
            }
            // A record's genesis is the same kind of change; so is a
            // package-primary document's, which holds no identity to derive
            // its actor from. The document's own history keeps it, marked,
            // as the version to restore to.
            let genesis =
                genesis_actor.as_deref() == Some(logged.actor.as_str()) || reading.store_genesis;
            if (genesis && !source.record_genesis) || reading.marker {
                continue;
            }
            let context = if genesis {
                Some(genesis_context())
            } else {
                logged
                    .message
                    .as_deref()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            };
            let summary = context
                .as_ref()
                .and_then(tilo_actions::ActionContext::summary)
                .unwrap_or_else(|| reading.summary.clone());
            out.push(HistoryEntry {
                id: logged.id,
                author: source.authors.resolve(&logged.actor),
                op_class: reading.op_class,
                artifact_kind: source.artifact_kind,
                timestamp: timestamp_from_ms(logged.time_ms),
                summary,
                text_delta: reading.text_delta,
                // The scrubber re-derives state via `materialize_at` /
                // `heads_for_entry` rather than cracking open a payload.
                diff_payload: None,
                // Projected from the broker's redaction log, keyed by
                // artifact and not reachable from a bare document.
                redaction: None,
                context,
            });
        }
        out
    }
}

impl HistorySource {
    /// [`Self::entries`], reading only the changes `log` has not read yet.
    /// The rows are the same as [`Self::entries`]' for the same document.
    pub fn entries_since(&self, doc: &mut AutoCommit, log: &mut HistoryLog) -> Vec<HistoryEntry> {
        super::with_committed_history(doc, |doc| {
            log.follow(self, doc);
            log.entries(self, doc)
        })
    }

    pub(super) fn committed_entries(&self, doc: &mut AutoCommit) -> Vec<HistoryEntry> {
        let mut log = HistoryLog::default();
        log.follow(self, doc);
        log.entries(self, doc)
    }
}
