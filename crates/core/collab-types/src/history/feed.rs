//! What changed in a workspace's records, as a host's replica of them took it in: the feed a
//! watch of the whole workspace reads ([`tilo_actions::remote::ChangeFeed`]), one for every host
//! that holds a workspace's records (a desktop, a server's workspace).
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

use std::collections::{HashMap, HashSet, VecDeque};

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::remote::{ChangePage, ResourceChanged};

use super::words::HistoryWords;

/// One record of a workspace: the app whose record it is, and its id there.
pub type DocumentKey = (String, String);

/// The most changes kept for followers to read.
const KEPT: usize = 4096;

/// The most changes one page answers.
const PAGE: usize = 256;

/// One workspace's changes, as its records were read.
pub struct ChangeLog {
    /// Names this log in its cursors: a cursor of another names nothing here.
    epoch: String,
    /// Each record's revision and heads when last read.
    seen: HashMap<DocumentKey, ([u8; 32], Vec<ChangeHash>)>,
    /// Each record kept in a resource store, by the versions of it already taken in: each by
    /// its change and the resource it is a version of. A change reaching this host late is
    /// still news, wherever it falls in the record's order.
    versions_seen: HashMap<DocumentKey, HashSet<(String, String)>>,
    /// The changes kept, each with its sequence number, oldest first.
    kept: VecDeque<(u64, ResourceChanged)>,
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
            versions_seen: HashMap::new(),
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
        records: &[DocumentKey],
        revision: impl Fn(&DocumentKey) -> Option<[u8; 32]>,
        document: impl Fn(&DocumentKey) -> Option<Vec<u8>>,
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
                for entry in super::verbs::entries_after((&doc, &route.1), &after, words) {
                    self.keep(ResourceChanged {
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

    /// Take in `record`, kept in a resource store, as its `versions` stand now: each as the
    /// record's history lists it, oldest first, with its `change` and its `resource`. A
    /// version not yet committed names no change, and is taken in once it does. Call it before
    /// [`Self::take_in`], which ends the first look.
    pub fn take_in_versions(&mut self, record: &DocumentKey, versions: Vec<serde_json::Value>) {
        let primed = self.primed;
        let known = self.versions_seen.contains_key(record);
        let mut news = Vec::new();
        let seen = self.versions_seen.entry(record.clone()).or_default();
        for entry in versions {
            let key = |name: &str| {
                entry
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let (change, resource) = (key("change"), key("resource"));
            if change.is_empty() || !seen.insert((change, resource)) {
                continue;
            }
            // A record that arrived after the first look is news whole.
            if known || primed {
                news.push(entry);
            }
        }
        for entry in news {
            self.keep(ResourceChanged {
                app: record.0.clone(),
                record: record.1.clone(),
                entry,
            });
        }
    }

    /// Take in the changes of a workspace's members its governance names (`named`: each change's
    /// hash, the message naming its action and who decided it, and its time in Unix seconds,
    /// oldest first) as changes of `record`, its members' resource: each action listed once, with
    /// its verb, who decided it and when, as a record's own history lists a change. The changes
    /// one administration made name the same invocation, and are that one action (a ban is a
    /// removal and the block that keeps the person out); a change whose message names no action
    /// is no member change, and is left out. Call it before [`Self::take_in`], which ends the
    /// first look.
    pub fn take_in_named(&mut self, record: &DocumentKey, named: &[([u8; 32], String, i64)]) {
        let mut versions: Vec<serde_json::Value> = Vec::new();
        let mut last_invocation: Option<String> = None;
        for (change, message, time) in named {
            let Some(stamp) = crate::Stamp::of_message(message) else {
                continue;
            };
            if stamp.verb.is_empty() {
                continue;
            }
            let invocation = (!stamp.invocation.is_empty()).then(|| stamp.invocation.clone());
            if invocation.is_some() && invocation == last_invocation {
                continue;
            }
            last_invocation = invocation;
            versions.push(serde_json::json!({
                "change": super::steps::short(&ChangeHash(*change)),
                "resource": record.1,
                "verb": stamp.verb,
                "sentence": stamp.verb,
                "by": stamp.invoker,
                "time_ms": time.saturating_mul(1000),
                "happened": tilo_actions::remote::What::Edited.word(),
            }));
        }
        self.take_in_versions(record, versions);
    }

    /// The heads `record` stood at when it was last read whole: what a reader asks it since, to
    /// be told it stands there still without reading it again.
    #[must_use]
    pub fn heads_of(&self, record: &DocumentKey) -> Option<Vec<[u8; 32]>> {
        self.seen
            .get(record)
            .map(|(_, heads)| heads.iter().map(|head| head.0).collect())
    }

    fn keep(&mut self, change: ResourceChanged) {
        self.last = self.last.saturating_add(1);
        self.kept.push_back((self.last, change));
        while self.kept.len() > KEPT {
            self.kept.pop_front();
        }
    }

    /// The changes after `after`, a cursor this log gave, that `reader` reads
    /// ([`super::verbs::reads_entry`]); from now for none, or for a cursor it cannot read on
    /// from.
    #[must_use]
    pub fn page(&self, after: Option<&str>, reader: Option<&tilo_actions::MemberId>) -> ChangePage {
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
        let changes: Vec<(u64, ResourceChanged)> = self
            .kept
            .iter()
            .filter(|(seq, _)| *seq > from)
            .take(PAGE)
            .cloned()
            .collect();
        let last = changes.last().map_or(from, |(seq, _)| *seq);
        ChangePage {
            changes: changes
                .into_iter()
                .map(|(_, change)| change)
                .filter(|change| super::verbs::reads_entry(&change.entry, reader))
                .collect(),
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

    fn route(record: &str) -> DocumentKey {
        ("so.tilo.notes".into(), record.into())
    }

    /// A store of saved records, by route, each with a revision that moves on every save.
    #[derive(Default)]
    struct Saved(HashMap<DocumentKey, ([u8; 32], Vec<u8>)>, u8);

    impl Saved {
        fn save(&mut self, at: &DocumentKey, doc: &mut AutoCommit) {
            self.1 = self.1.wrapping_add(1);
            self.0.insert(at.clone(), ([self.1; 32], doc.save()));
        }

        fn take_into(&self, log: &mut ChangeLog) {
            let records: Vec<DocumentKey> = self.0.keys().cloned().collect();
            log.take_in(
                &records,
                |at| self.0.get(at).map(|(revision, _)| *revision),
                |at| self.0.get(at).map(|(_, bytes)| bytes.clone()),
                &crate::history::TestWords,
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
        let start = log.page(None, None);
        assert!(start.changes.is_empty());
        assert!(log.page(Some(&start.cursor), None).changes.is_empty());

        write(&mut notes, "title", "Gooseberry jam");
        saved.save(&route("notes"), &mut notes);
        // A record that arrives later is news whole.
        let mut todo = AutoCommit::new();
        write(&mut todo, "item", "Buy jars");
        saved.save(&route("todo"), &mut todo);
        saved.take_into(&mut log);

        let page = log.page(Some(&start.cursor), None);
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
        assert!(log.page(Some(&page.cursor), None).changes.is_empty());
    }

    /// A record's content is read for the record the feed lists it under: the document it reads
    /// is the record's content, and each change says what it did to the record — the first made
    /// it, the next, made by an action whose context names no item, edited it, both naming it.
    #[test]
    fn a_records_content_says_what_each_change_did() {
        let mut saved = Saved::default();
        let mut log = ChangeLog::new();
        saved.take_into(&mut log);
        let start = log.page(None, None);
        assert!(start.changes.is_empty());

        let record = route("note1");
        let mut content = AutoCommit::new();
        write(&mut content, "title", "Plan");
        content.put(automerge::ROOT, "title", "Plan B").unwrap();
        let action = tilo_actions::stamp::Stamp {
            verb: "edit".into(),
            invocation: "inv-1".into(),
            ..tilo_actions::stamp::Stamp::default()
        };
        content.commit_with(
            automerge::transaction::CommitOptions::default().with_message(action.message()),
        );
        saved.save(&record, &mut content);
        saved.take_into(&mut log);

        let page = log.page(Some(&start.cursor), None);
        assert_eq!(page.changes.len(), 2, "{page:?}");
        let told: Vec<(&str, &str)> = page
            .changes
            .iter()
            .map(|change| {
                (
                    change.entry["happened"].as_str().unwrap_or_default(),
                    change.entry["item"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            told,
            [("created", "note1"), ("edited", "note1")],
            "{page:?}"
        );
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
        let page = restarted.page(Some(&log.page(None, None).cursor), None);
        assert!(page.changes.is_empty());
        assert!(page.cursor.starts_with(&restarted.epoch));
    }

    /// A version of a record kept in a resource store, as its history lists it.
    fn version(change: &str, resource: &str) -> serde_json::Value {
        serde_json::json!({ "change": change, "resource": resource })
    }

    #[test]
    fn a_store_record_tells_each_version_once_late_ones_too() {
        let mut log = ChangeLog::new();
        let chat = route("chat");
        log.take_in_versions(&chat, vec![version("a", "m1")]);
        log.take_in(&[], |_| None, |_| None, &crate::history::TestWords);
        // What the record held when the follower started is no news.
        let start = log.page(None, None);
        assert!(start.changes.is_empty());

        // A version made later, one that reached this host late though made before it, and one
        // not committed yet.
        let now = vec![
            version("a", "m1"),
            version("late", "m2"),
            version("b", "m1"),
            version("", "m3"),
        ];
        log.take_in_versions(&chat, now.clone());
        let page = log.page(Some(&start.cursor), None);
        let changes: Vec<&str> = page
            .changes
            .iter()
            .map(|change| change.entry["change"].as_str().unwrap())
            .collect();
        assert_eq!(changes, ["late", "b"], "{page:?}");

        // Taken in again, nothing is news; the committed version is, once it names its change.
        log.take_in_versions(&chat, now);
        log.take_in_versions(&chat, vec![version("c", "m3")]);
        let page = log.page(Some(&page.cursor), None);
        assert_eq!(page.changes.len(), 1, "{page:?}");
        assert_eq!(page.changes[0].entry["change"], "c");

        // A store record that arrives after the first look is news whole.
        log.take_in_versions(&route("tasks"), vec![version("t", "task1")]);
        let page = log.page(Some(&page.cursor), None);
        assert_eq!(page.changes.len(), 1, "{page:?}");
    }
}
