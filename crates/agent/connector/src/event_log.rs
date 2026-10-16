//! The host-private log of a connector's events (bots.md §6, "Trigger
//! sources"): deduplicated by `(source, id)` before they are appended, kept
//! for 24 hours or 10 000 events, numbered so each installation can
//! acknowledge with its own cursor. An append-only JSON-lines file in an
//! owner-only directory; pruning rewrites it.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub seq: u64,
    pub source: String,
    pub id: String,
    pub at_ms: i64,
    pub received_ms: i64,
    pub payload: serde_json::Map<String, serde_json::Value>,
}

pub struct EventLog {
    path: PathBuf,
    events: VecDeque<LoggedEvent>,
    seen: HashSet<(String, String)>,
    last_seq: u64,
}

impl EventLog {
    pub const MAX_EVENTS: usize = 10_000;
    pub const WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

    /// The log kept in `dir` (created owner-only if missing).
    ///
    /// # Errors
    /// The directory or file cannot be created or read. A line that does
    /// not parse is skipped: a torn last write loses that one event, which
    /// the worker's backfill covers.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.join("events.jsonl");
        let mut log = Self {
            path,
            events: VecDeque::new(),
            seen: HashSet::new(),
            last_seq: 0,
        };
        if let Ok(file) = std::fs::File::open(&log.path) {
            for line in std::io::BufReader::new(file).lines() {
                let Ok(event) = serde_json::from_str::<LoggedEvent>(&line?) else {
                    continue;
                };
                log.last_seq = log.last_seq.max(event.seq);
                log.seen.insert((event.source.clone(), event.id.clone()));
                log.events.push_back(event);
            }
        }
        Ok(log)
    }

    /// Append an event unless `(source, id)` is already logged. Returns its
    /// sequence number, or `None` for a duplicate.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn append(
        &mut self,
        source: &str,
        id: &str,
        at_ms: i64,
        received_ms: i64,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> std::io::Result<Option<u64>> {
        let key = (source.to_owned(), id.to_owned());
        if self.seen.contains(&key) {
            return Ok(None);
        }
        let seq = self.last_seq.saturating_add(1);
        let event = LoggedEvent {
            seq,
            source: key.0.clone(),
            id: key.1.clone(),
            at_ms,
            received_ms,
            payload,
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(&line)?;
        self.last_seq = seq;
        self.seen.insert(key);
        self.events.push_back(event);
        Ok(Some(seq))
    }

    /// Events after `cursor`, received at or after `from_ms`, oldest first.
    pub fn after(&self, cursor: u64, from_ms: i64) -> impl Iterator<Item = &LoggedEvent> {
        self.events
            .iter()
            .filter(move |e| e.seq > cursor && e.received_ms >= from_ms)
    }

    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Drop events older than the window or beyond the count and rewrite the
    /// file. Dropped ids stop deduplicating, which is fine: a service only
    /// replays recent events.
    ///
    /// # Errors
    /// The file cannot be rewritten.
    pub fn prune(&mut self, now_ms: i64) -> std::io::Result<usize> {
        let before = self.events.len();
        let horizon = now_ms.saturating_sub(Self::WINDOW_MS);
        while self.events.front().is_some_and(|e| e.received_ms < horizon)
            || self.events.len() > Self::MAX_EVENTS
        {
            if let Some(event) = self.events.pop_front() {
                self.seen.remove(&(event.source, event.id));
            }
        }
        let dropped = before.saturating_sub(self.events.len());
        if dropped > 0 {
            let mut file = Vec::new();
            for event in &self.events {
                file.extend(serde_json::to_vec(event)?);
                file.push(b'\n');
            }
            tilo_files::native::atomic::write(&self.path, &file)?;
        }
        Ok(dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn payload(text: &str) -> Map<String, serde_json::Value> {
        json!({"content": text}).as_object().cloned().unwrap()
    }

    #[test]
    fn duplicate_event_ids_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(dir.path()).unwrap();
        assert_eq!(
            log.append("message-created", "m1", 1, 10, payload("a"))
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            log.append("message-created", "m1", 1, 11, payload("a"))
                .unwrap(),
            None
        );
        assert_eq!(
            log.append("message-updated", "m1", 2, 12, payload("b"))
                .unwrap(),
            Some(2),
            "ids are per source"
        );
        assert_eq!(log.after(0, 0).count(), 2);
    }

    #[test]
    fn cursors_and_floor_select_what_an_installation_has_not_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(dir.path()).unwrap();
        for (at, id) in [(0, "a"), (100, "b"), (200, "c")] {
            log.append("command", id, at, at, payload(id)).unwrap();
        }
        let ids: Vec<&str> = log.after(1, 0).map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
        let floored: Vec<&str> = log.after(0, 150).map(|e| e.id.as_str()).collect();
        assert_eq!(
            floored,
            ["c"],
            "events before the floor are never delivered"
        );
    }

    #[test]
    fn prune_keeps_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append("command", "old", 0, 0, payload("o")).unwrap();
        log.append("command", "new", 0, EventLog::WINDOW_MS + 5, payload("n"))
            .unwrap();
        assert_eq!(log.prune(EventLog::WINDOW_MS + 10).unwrap(), 1);
        let left: Vec<&str> = log.after(0, 0).map(|e| e.id.as_str()).collect();
        assert_eq!(left, ["new"]);
        assert_eq!(log.last_seq(), 2, "sequence numbers never go back");
    }

    #[test]
    fn prune_caps_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(dir.path()).unwrap();
        for i in 0..(EventLog::MAX_EVENTS + 3) {
            log.append("command", &i.to_string(), 0, 1, Map::new())
                .unwrap();
        }
        log.prune(2).unwrap();
        assert_eq!(log.after(0, 0).count(), EventLog::MAX_EVENTS);
        assert_eq!(log.after(0, 0).next().unwrap().id, "3");
    }

    #[test]
    fn reopening_restores_events_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut log = EventLog::open(dir.path()).unwrap();
            log.append("command", "x", 1, 1, payload("x")).unwrap();
        }
        let mut log = EventLog::open(dir.path()).unwrap();
        assert_eq!(
            log.append("command", "x", 1, 2, payload("x")).unwrap(),
            None
        );
        assert_eq!(
            log.append("command", "y", 1, 2, payload("y")).unwrap(),
            Some(2)
        );
    }
}
