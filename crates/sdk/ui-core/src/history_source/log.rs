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

use crate::widget::EntryKind;
use automerge::{AutoCommit, ChangeHash};
use tilo_automerge::ApplyIncoming as _;
use tilo_doc::snapshot::DocSnapshot;

use super::{
    HistorySource, NO_VISIBLE_CHANGE, entry_id_from_hash, genesis_context, is_store_genesis,
    patch_summary, structural_summary, summarize_patches, timestamp_from_ms,
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
    /// `replica`'s tilo-doc snapshot, the "before" of the next structural
    /// reading.
    snapshot: Option<DocSnapshot>,
    /// The replica could not apply a change; readings after it fell back to
    /// diffing the document, and the next call reads from scratch.
    failed: bool,
    /// How many changes this log has read, rebuilds included.
    reads: usize,
    /// The positions in `changes` of those that take a place in the window:
    /// every one the source does not pass over.
    counted: Vec<usize>,
    /// Moves whenever the rows may have: on every change read that the
    /// source does not pass over, and on every read from scratch.
    rows_revision: u64,
}

/// Everything besides the log itself that a reading depends on.
#[derive(Clone, PartialEq, Eq)]
struct Basis {
    max_entries: usize,
    field_paths: bool,
    /// Whether the document carries the tilo-doc schema.
    structural: bool,
    /// Whether the source passes over some changes.
    passes_over: bool,
    /// Readings carry translated words: the active locale, and the
    /// catalogue revision, which moves whenever a catalogue is added.
    locale: String,
    catalogue: u64,
}

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
    entry_kind: EntryKind,
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
    /// Moves whenever the rows may have moved: a change read that the source
    /// does not pass over, or a read from scratch. While it holds, the
    /// changes added since were all passed over and the rows are as they
    /// were.
    #[must_use]
    pub const fn rows_revision(&self) -> u64 {
        self.rows_revision
    }

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
            passes_over: source.pass_over.is_some(),
            locale: crate::i18n::I18nManager::current_locale(),
            catalogue: crate::i18n::I18nManager::revision(),
        };
        let appended = {
            let log = doc.committed_document().change_hashes();
            let read = self.changes.len();
            let prefix = self.basis.as_ref() == Some(&basis)
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
        let max_entries = basis.max_entries;
        *self = Self {
            basis: Some(basis),
            reads: self.reads,
            rows_revision: self.rows_revision.wrapping_add(1),
            ..Self::default()
        };
        let metas: Vec<Meta> = doc.get_changes_meta(&[]).iter().map(Meta::of).collect();
        let all = doc.get_changes(&[]);
        let passed_over: Vec<bool> = all
            .iter()
            .map(|change| source.passes_over(change))
            .collect();
        // The window starts at its earliest change that takes a place: the
        // `max_entries`th from the end, counting only those.
        let start = passed_over
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, passed)| !**passed)
            .nth(max_entries.saturating_sub(1))
            .map_or(0, |(position, _)| position);
        let mut changes = all.into_iter();
        let prefix: Vec<_> = changes.by_ref().take(start).collect();
        let mut metas = metas.into_iter();
        for (meta, passed) in metas.by_ref().take(start).zip(&passed_over) {
            self.push(meta, *passed);
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
        let passed_over = source.passes_over(&change);
        let patches = if self.failed || self.replica.apply_incoming([change]).is_err() {
            self.failed = true;
            None
        } else {
            Some(self.replica.diff_incremental())
        };
        let current = self.replica_snapshot();
        if passed_over {
            // Applied, so the replica, its diff cursor and its snapshot stay
            // at the log's end; nothing is read of it.
            self.snapshot = current;
            self.push(meta, true);
            return;
        }
        self.rows_revision = self.rows_revision.wrapping_add(1);
        let before = self.heads.clone();
        let (hash, time_ms) = (meta.hash, meta.time_ms);
        self.push(meta, false);
        let after = self.heads.clone();
        let (summary, entry_kind, text_delta) = match (&self.snapshot, &current, &patches) {
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
            super::is_history_metadata(&patches)
        };
        let store_genesis = is_store_genesis(doc, time_ms, &hash);
        if let Some(logged) = self.changes.last_mut() {
            logged.reading = Some(Reading {
                summary,
                entry_kind,
                text_delta,
                marker,
                store_genesis,
            });
        }
    }

    /// Add `meta` to the log and move its frontier past it. The log is in
    /// causal order, so a change's dependencies are already in it: they stop
    /// being heads.
    fn push(&mut self, meta: Meta, passed_over: bool) {
        self.heads.retain(|head| !meta.deps.contains(head));
        self.heads.push(meta.hash);
        let id = entry_id_from_hash(&meta.hash.to_string());
        self.positions.entry(id).or_insert(self.changes.len());
        if !passed_over {
            self.counted.push(self.changes.len());
        }
        self.changes.push(Logged {
            hash: meta.hash,
            id,
            deps: meta.deps,
            heads: self.heads.clone(),
            actor: meta.actor,
            time_ms: meta.time_ms,
            message: meta.message,
            reading: None,
        });
    }

    /// The replica's tilo-doc snapshot, when the document carries that schema.
    fn replica_snapshot(&self) -> Option<DocSnapshot> {
        (self.basis.as_ref().is_some_and(|basis| basis.structural) && !self.failed)
            .then(|| tilo_doc::snapshot::of(&self.replica))
            .flatten()
    }

    /// The rows of the window, as of the last [`Self::follow`].
    fn entries(&self, source: &HistorySource, doc: &AutoCommit) -> Vec<HistoryEntry> {
        let start = self
            .counted
            .len()
            .checked_sub(source.max_entries)
            .and_then(|first| self.counted.get(first).copied())
            .unwrap_or(0);
        let genesis_actor = crate::record::genesis_actor(doc).map(|actor| actor.to_hex_string());
        let schema_genesis = tilo_doc::model::schema_genesis_actor_hex();
        let mut out: Vec<HistoryEntry> =
            Vec::with_capacity(self.changes.len().saturating_sub(start));
        // A person typing on in one field is one row ([`super::typing`]):
        // each author's open run, by the row it is.
        let mut typing = super::typing::TypingRuns::<usize>::default();
        for logged in self.changes.iter().skip(start) {
            let Some(reading) = &logged.reading else {
                typing.continues(&logged.actor, None, logged.time_ms);
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
                typing.continues(&logged.actor, None, logged.time_ms);
                continue;
            }
            let context = if genesis {
                Some(genesis_context())
            } else {
                match logged.message.as_deref() {
                    Some(message) if message.starts_with(tilo_actions::CONTINUATION_PREFIX) => {
                        tilo_actions::entry_of(doc.committed_document(), logged.hash)
                    }
                    message => message.and_then(tilo_actions::ActionContext::from_change_message),
                }
            };
            let summary = if genesis {
                crate::i18n::I18nManager::lookup_plain("history-genesis")
            } else {
                context
                    .as_ref()
                    .and_then(super::sentence::step_summary)
                    .unwrap_or_else(|| reading.summary.clone())
            };
            let typed = context
                .as_ref()
                .filter(|_| !genesis)
                .and_then(super::typing::TypedField::of);
            if let Some(&mut row) = typing.continues(&logged.actor, typed.as_ref(), logged.time_ms)
            {
                // The row is the run as of its newest change: what it now
                // reads, when, and the version it restores to. It moves to
                // where that change is in the log.
                let mut run = out.remove(row);
                run.id = logged.id;
                run.timestamp = timestamp_from_ms(logged.time_ms);
                run.summary = summary;
                run.entry_kind = reading.entry_kind;
                run.text_delta = run.text_delta.zip(reading.text_delta).map(
                    |((added, removed), (more, fewer))| {
                        (added.saturating_add(more), removed.saturating_add(fewer))
                    },
                );
                run.context = context;
                out.push(run);
                let moved = out.len().saturating_sub(1);
                for place in typing.places_mut() {
                    if *place == row {
                        *place = moved;
                    } else if *place > row {
                        *place = place.saturating_sub(1);
                    }
                }
                continue;
            }
            if let Some(field) = typed {
                typing.start(&logged.actor, field, logged.time_ms, out.len());
            }
            out.push(HistoryEntry {
                id: logged.id,
                author: source
                    .authors
                    .resolve_entry(&logged.actor, context.as_ref()),
                entry_kind: reading.entry_kind,
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
    /// Bring `log` up to `doc`'s committed log without listing the rows:
    /// [`HistoryLog::rows_revision`] then says whether they moved.
    pub fn follow(&self, doc: &mut AutoCommit, log: &mut HistoryLog) {
        super::with_committed_history(doc, |doc| log.follow(self, doc));
    }

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
