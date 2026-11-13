//! What changed in a workspace's records, as a host's replica of them took it in: the feed a
//! watch of the whole workspace reads ([`tilo_actions::remote::ChangeFeed`]), one for every host
//! that holds a workspace's records (a desktop, a relay's workspace).
//!
//! The replica holds every record the workspace sent the host, whoever wrote it: a page's edits,
//! another device's, the host's own runs. Each time the feed is asked, each record whose revision
//! moved is read again, and the changes it holds past the heads it stood at last are kept in
//! order, each as the record's own `history` lists it ([`super::verbs::entries_after`]: who made
//! it, in which run, through which invocation). A record that arrives after the first look is
//! news whole.
//!
//! What is kept is bounded ([`KEPT`]), and lives as long as the host keeps the log: a cursor from
//! before a restart, or one whose changes were let go of, reads on from now.

use std::collections::{HashMap, VecDeque};

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::remote::{ChangePage, RecordChanged};

use super::words::HistoryWords;

/// One record of a workspace: the app whose record it is, and its id there.
pub type RecordKey = (String, String);

/// The most changes kept for followers to read.
const KEPT: usize = 4096;

/// The most changes one page answers.
const PAGE: usize = 256;

/// One workspace's changes, as its records were read.
pub struct ChangeLog {
    /// Names this log in its cursors: a cursor of another names nothing here.
    epoch: String,
    /// Each record's revision and heads when last read.
    seen: HashMap<RecordKey, ([u8; 32], Vec<ChangeHash>)>,
    /// The changes kept, each with its sequence number, oldest first.
    kept: VecDeque<(u64, RecordChanged)>,
    /// The sequence number of the last change taken in.
    last: u64,
    /// Whether the records were read once: what they held then is no news.
    primed: bool,
}

impl Default for ChangeLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangeLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: uuid::Uuid::new_v4().simple().to_string(),
            seen: HashMap::new(),
            kept: VecDeque::new(),
            last: 0,
            primed: false,
        }
    }

    /// Take in what each of `records` holds now: `revision` says where a record stands without
    /// reading it, and `document` reads it whole, both `None` for a record the replica cannot
    /// open yet. Each change is said in `words`.
    pub fn take_in(
        &mut self,
        records: &[RecordKey],
        revision: impl Fn(&RecordKey) -> Option<[u8; 32]>,
        document: impl Fn(&RecordKey) -> Option<Vec<u8>>,
        words: &dyn HistoryWords,
    ) {
        for route in records {
            let Some(now) = revision(route) else {
                continue;
            };
            let before = self.seen.get(route);
            if before.is_some_and(|(at, _)| *at == now) {
                continue;
            }
            let Some(doc) = document(route).and_then(|bytes| AutoCommit::load(&bytes).ok()) else {
                continue;
            };
            let heads = doc.committed_document().get_heads();
            let news = match before {
                Some((_, heads)) => Some(heads.clone()),
                // A record that arrived after the first look: everything in it is news.
                None if self.primed => Some(Vec::new()),
                None => None,
            };
            if let Some(after) = news {
                for entry in super::verbs::entries_after(&doc, &after, words) {
                    self.keep(RecordChanged {
                        app: route.0.clone(),
                        record: route.1.clone(),
                        entry,
                    });
                }
            }
            self.seen.insert(route.clone(), (now, heads));
        }
        self.primed = true;
    }

    fn keep(&mut self, change: RecordChanged) {
        self.last = self.last.saturating_add(1);
        self.kept.push_back((self.last, change));
        while self.kept.len() > KEPT {
            self.kept.pop_front();
        }
    }

    /// The changes after `after`, a cursor this log gave; from now for none, or for a cursor
    /// it cannot read on from.
    #[must_use]
    pub fn page(&self, after: Option<&str>) -> ChangePage {
        let from = after.and_then(|cursor| {
            let (epoch, seq) = cursor.split_once('.')?;
            let seq = seq.parse::<u64>().ok()?;
            // A cursor past what was let go of reads on; one before it lost changes and reads
            // on from the oldest kept, which is the best this log can do.
            (epoch == self.epoch && seq <= self.last).then_some(seq)
        });
        let Some(from) = from else {
            return ChangePage {
                changes: Vec::new(),
                cursor: self.cursor(self.last),
            };
        };
        let changes: Vec<(u64, RecordChanged)> = self
            .kept
            .iter()
            .filter(|(seq, _)| *seq > from)
            .take(PAGE)
            .cloned()
            .collect();
        let last = changes.last().map_or(from, |(seq, _)| *seq);
        ChangePage {
            changes: changes.into_iter().map(|(_, change)| change).collect(),
            cursor: self.cursor(last),
        }
    }

    fn cursor(&self, seq: u64) -> String {
        format!("{}.{seq}", self.epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    fn route(record: &str) -> RecordKey {
        ("so.tilo.notes".into(), record.into())
    }

    /// A store of saved records, by route, each with a revision that moves on every save.
    #[derive(Default)]
    struct Saved(HashMap<RecordKey, ([u8; 32], Vec<u8>)>, u8);

    impl Saved {
        fn save(&mut self, at: &RecordKey, doc: &mut AutoCommit) {
            self.1 = self.1.wrapping_add(1);
            self.0.insert(at.clone(), ([self.1; 32], doc.save()));
        }

        fn take_into(&self, log: &mut ChangeLog) {
            let records: Vec<RecordKey> = self.0.keys().cloned().collect();
            log.take_in(
                &records,
                |at| self.0.get(at).map(|(revision, _)| *revision),
                |at| self.0.get(at).map(|(_, bytes)| bytes.clone()),
                &crate::history_source::TestWords,
            );
        }
    }

    fn write(doc: &mut AutoCommit, key: &str, value: &str) {
        doc.put(automerge::ROOT, key, value).unwrap();
        doc.commit();
    }

    #[test]
    fn a_follower_reads_each_change_once_from_where_it_started() {
        let mut saved = Saved::default();
        let mut log = ChangeLog::new();
        let mut notes = AutoCommit::new();
        write(&mut notes, "title", "Before anyone watched");
        saved.save(&route("notes"), &mut notes);
        saved.take_into(&mut log);
        // What the records held when the follower started is no news.
        let start = log.page(None);
        assert!(start.changes.is_empty());
        assert!(log.page(Some(&start.cursor)).changes.is_empty());

        write(&mut notes, "title", "Gooseberry jam");
        saved.save(&route("notes"), &mut notes);
        // A record that arrives later is news whole.
        let mut todo = AutoCommit::new();
        write(&mut todo, "item", "Buy jars");
        saved.save(&route("todo"), &mut todo);
        saved.take_into(&mut log);

        let page = log.page(Some(&start.cursor));
        let records: Vec<&str> = page
            .changes
            .iter()
            .map(|change| change.record.as_str())
            .collect();
        assert_eq!(records.len(), 2, "{page:?}");
        assert!(records.contains(&"notes") && records.contains(&"todo"));
        assert!(
            page.changes
                .iter()
                .all(|change| change.entry["change"].is_string())
        );
        // Read once: from the cursor it answered, nothing more.
        saved.take_into(&mut log);
        assert!(log.page(Some(&page.cursor)).changes.is_empty());
    }

    #[test]
    fn a_cursor_of_another_log_reads_on_from_now() {
        let mut saved = Saved::default();
        let mut log = ChangeLog::new();
        saved.take_into(&mut log);
        let mut notes = AutoCommit::new();
        write(&mut notes, "title", "Written before the restart");
        saved.save(&route("notes"), &mut notes);
        saved.take_into(&mut log);
        let restarted = ChangeLog::new();
        let page = restarted.page(Some(&log.page(None).cursor));
        assert!(page.changes.is_empty());
        assert!(page.cursor.starts_with(&restarted.epoch));
    }
}
