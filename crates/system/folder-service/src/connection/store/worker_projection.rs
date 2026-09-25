//! Bounded, read-only service projection; no receipt or native handle construction.
use super::*;
impl ConnectionStore {
    pub(in crate::connection) fn worker_recovery_entry(
        &self,
        id: RecoveryId,
    ) -> Result<RecoveryEntry, StoreError> {
        self.load_connection(id.operation.connection)?;
        match self.row(&recovery::recovery_key(id))? {
            Some(Row::Recovery(entry)) => entry.decode(),
            _ => Err(StoreError::RecoveryRequired),
        }
    }
}

// Page parsing already caps each stored page at 128 KiB, 128 rows and 8 levels.
// The facade also caps aggregate work, including point joins and CURRENT reads.
const PAGE_CAP: usize = 128;
const READ_CAP: usize = 8 * 1024 * 1024;
const WORK_CAP: usize = 131_072;

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::connection) struct ReadStats {
    pub pages: usize,
    pub requests: usize,
    pub bytes: usize,
    pub work: usize,
}
pub(super) struct Budget {
    root: Option<[u8; 32]>,
    generation: u64,
    sequence: u64,
    pages: BTreeMap<[u8; 32], Arc<format::PageNode>>,
    stats: ReadStats,
    read_cap: usize,
}

pub(in crate::connection) struct Projection<'a>(&'a ConnectionStore);
impl Projection<'_> {
    pub(in crate::connection) fn finish(self) -> Result<(), StoreError> {
        self.0.worker_projection_validate()
    }
}
impl Drop for Projection<'_> {
    fn drop(&mut self) {
        let budget = self.0.projection.borrow_mut().take();
        #[cfg(test)]
        if let Some(budget) = budget {
            test_support::record(&self.0.storage.path, budget.stats);
        }
        #[cfg(not(test))]
        drop(budget);
    }
}

impl ConnectionStore {
    pub(in crate::connection) fn worker_projection(&self) -> Result<Projection<'_>, StoreError> {
        if self.projection.borrow().is_some() {
            return Err(StoreError::Busy);
        }
        *self.projection.borrow_mut() = Some(Budget {
            root: self.current.root,
            generation: self.current.generation,
            sequence: self.sequence,
            pages: BTreeMap::new(),
            stats: ReadStats::default(),
            #[cfg(test)]
            read_cap: test_support::read_cap(&self.storage.path),
            #[cfg(not(test))]
            read_cap: READ_CAP,
        });
        let scope = Projection(self);
        self.worker_projection_validate()?;
        Ok(scope)
    }
    fn projection_work(&self, amount: usize) -> Result<(), StoreError> {
        let mut state = self.projection.borrow_mut();
        let budget = state.as_mut().ok_or(StoreError::RecoveryRequired)?;
        budget.stats.work = budget
            .stats
            .work
            .checked_add(amount)
            .filter(|work| *work <= WORK_CAP)
            .ok_or(StoreError::LimitExceeded)?;
        Ok(())
    }
    fn projection_read(&self, requested: usize) -> Result<(), StoreError> {
        #[cfg(test)]
        test_support::before_read(&self.storage.path)?;
        let mut state = self.projection.borrow_mut();
        let budget = state.as_mut().ok_or(StoreError::RecoveryRequired)?;
        budget.stats.bytes = budget
            .stats
            .bytes
            .checked_add(requested)
            .filter(|bytes| *bytes <= budget.read_cap)
            .ok_or(StoreError::LimitExceeded)?;
        budget.stats.requests += 1;
        Ok(())
    }
    pub(super) fn worker_projection_validate(&self) -> Result<(), StoreError> {
        self.projection_work(1)?;
        if self.poisoned {
            return Err(StoreError::RecoveryRequired);
        }
        {
            let state = self.projection.borrow();
            let budget = state.as_ref().ok_or(StoreError::RecoveryRequired)?;
            if budget.root != self.current.root
                || budget.generation != self.current.generation
                || budget.sequence != self.sequence
            {
                return Err(StoreError::StaleRevision);
            }
        }
        self.storage.validate()?;
        // Exactly the retained-pointer checks of Storage::validate_current,
        // with the same 4096-byte ceiling and actual metered read requests.
        let pointer = storage::read_file(&self.storage.root, "CURRENT")?;
        storage::validate_file(
            &self.storage.root,
            "CURRENT",
            &pointer,
            &self.current_identity,
        )?;
        let bytes =
            self.storage
                .read_bytes_metered(&self.storage.root, "CURRENT", 4096, &|requested| {
                    self.projection_read(requested)
                })?;
        if bytes != format::encode(&self.current)? {
            return Err(StoreError::IdentityChanged);
        }
        self.validate_log()
    }
    fn projection_page(
        &self,
        hash: [u8; 32],
        expected: Option<&format::Child>,
    ) -> Result<Arc<format::PageNode>, StoreError> {
        self.projection_work(1)?;
        let cached = self
            .projection
            .borrow()
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?
            .pages
            .get(&hash)
            .cloned();
        let page = if let Some(page) = cached {
            page
        } else {
            {
                let mut state = self.projection.borrow_mut();
                let budget = state.as_mut().ok_or(StoreError::RecoveryRequired)?;
                if budget.stats.pages >= PAGE_CAP {
                    return Err(StoreError::LimitExceeded);
                }
                budget.stats.pages += 1;
            }
            let page = Arc::new(
                self.storage
                    .page_metered(hash, &|requested| self.projection_read(requested))?,
            );
            // Account for validation of every row or child in a newly read page.
            self.projection_work(match &page.node {
                format::Node::Leaf(rows) => rows.len(),
                format::Node::Index(children) => children.len(),
            })?;
            self.projection
                .borrow_mut()
                .as_mut()
                .ok_or(StoreError::RecoveryRequired)?
                .pages
                .insert(hash, Arc::clone(&page));
            page
        };
        if let Some(expected) = expected {
            self.projection_work(match &page.node {
                format::Node::Leaf(rows) => rows.len(),
                format::Node::Index(children) => children.len(),
            })?;
            if page.reference(hash)? != *expected {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(page)
    }
    fn projection_row_copy(&self, row: &Row) -> Result<Row, StoreError> {
        // Bound copying/decoding of large overlay rows and repeated lookup joins,
        // as well as the number of keys. One unit covers at most 64 encoded bytes.
        self.projection_work(format::encode(row)?.len().div_ceil(64).saturating_add(1))?;
        Ok(row.clone())
    }
    pub(super) fn worker_projection_row(&self, key: &[u8]) -> Result<Option<Row>, StoreError> {
        self.projection_work(1)?;
        if let Some(row) = self.overlay.get(key) {
            return self.projection_row_copy(row).map(Some);
        }
        let Some(mut hash) = self.current.root else {
            return Ok(None);
        };
        let mut expected = None;
        for _ in 0..8 {
            let page = self.projection_page(hash, expected.as_ref())?;
            match &page.node {
                format::Node::Leaf(rows) => {
                    self.projection_work(rows.len())?;
                    return match rows.binary_search_by(|row| row.key().as_slice().cmp(key)) {
                        Ok(index) => self.projection_row_copy(&rows[index]).map(Some),
                        Err(_) => Ok(None),
                    };
                }
                format::Node::Index(children) => {
                    self.projection_work(children.len())?;
                    let Some(child) = children.iter().find(|child| {
                        child.first.as_slice() <= key && key <= child.last.as_slice()
                    }) else {
                        return Ok(None);
                    };
                    hash = child.hash;
                    expected = Some(child.clone());
                }
            }
        }
        Err(StoreError::LimitExceeded)
    }
    pub(super) fn worker_table_page(
        &self,
        id: ConnectionId,
        table: u8,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<Row>, StoreError> {
        use std::ops::Bound::{Excluded, Included};
        if !(1..=crate::connected::PAGE_LIMIT).contains(&limit) || !matches!(table, 2 | 3) {
            return Err(StoreError::LimitExceeded);
        }
        self.connection(id)?;
        let mut prefix = vec![table];
        prefix.extend(id.0);
        let mut upper = prefix.clone();
        // The table byte is 2 or 3, so a lexicographic successor always exists.
        for byte in upper.iter_mut().rev() {
            if *byte != u8::MAX {
                *byte += 1;
                break;
            }
            *byte = 0;
        }
        let (lower, exclusive) = match cursor {
            Some(cursor)
                if cursor.checkpoint == self.current.generation
                    && cursor.sequence == self.sequence
                    && cursor.last_key.starts_with(&prefix)
                    && cursor.last_key.len() == if table == 2 { 29 } else { 25 } =>
            {
                (cursor.last_key, true)
            }
            Some(_) => return Err(StoreError::StaleRevision),
            None => (prefix, false),
        };
        let range_lower = if exclusive {
            Excluded(lower.clone())
        } else {
            Included(lower.clone())
        };
        let mut overlay = self
            .overlay
            .range((range_lower, Excluded(upper.clone())))
            .peekable();
        let mut disk = RangeRows::new(self, lower, exclusive, upper)?.peekable();
        let mut entries = Vec::with_capacity(limit);
        let mut last = None;
        loop {
            self.projection_work(1)?;
            let row = match (disk.peek(), overlay.peek()) {
                (Some(Err(_)), _) => {
                    return Err(disk
                        .next()
                        .ok_or(StoreError::Corrupt)?
                        .err()
                        .ok_or(StoreError::Corrupt)?);
                }
                (Some(Ok(old)), Some((key, new))) => match old.key().cmp(key) {
                    std::cmp::Ordering::Less => disk.next().ok_or(StoreError::Corrupt)??,
                    std::cmp::Ordering::Equal => {
                        disk.next();
                        let row = self.projection_row_copy(new)?;
                        overlay.next();
                        row
                    }
                    std::cmp::Ordering::Greater => {
                        let row = self.projection_row_copy(new)?;
                        overlay.next();
                        row
                    }
                },
                (Some(_), None) => disk.next().ok_or(StoreError::Corrupt)??,
                (None, Some((_, row))) => {
                    let row = self.projection_row_copy(row)?;
                    overlay.next();
                    row
                }
                (None, None) => {
                    return Ok(Page {
                        entries,
                        next: None,
                    });
                }
            };
            if entries.len() == limit {
                return Ok(Page {
                    entries,
                    next: Some(PageCursor {
                        checkpoint: self.current.generation,
                        sequence: self.sequence,
                        last_key: last.ok_or(StoreError::Corrupt)?,
                    }),
                });
            }
            last = Some(row.key());
            entries.push(row);
        }
    }
    pub(in crate::connection) fn worker_recovery_page(
        &self,
        id: ConnectionId,
        cursor: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page<RecoveryEntry>, StoreError> {
        let rows = self.worker_table_page(id, 2, cursor, limit)?;
        Ok(Page {
            entries: rows
                .entries
                .into_iter()
                .map(|row| match row {
                    Row::Recovery(entry) => entry.decode(),
                    _ => Err(StoreError::Corrupt),
                })
                .collect::<Result<_, _>>()?,
            next: rows.next,
        })
    }
}

struct RangeRows<'a> {
    store: &'a ConnectionStore,
    lower: Vec<u8>,
    exclusive: bool,
    upper: Vec<u8>,
    stack: Vec<format::Child>,
    rows: std::vec::IntoIter<Row>,
    failed: bool,
}
impl<'a> RangeRows<'a> {
    fn new(
        store: &'a ConnectionStore,
        lower: Vec<u8>,
        exclusive: bool,
        upper: Vec<u8>,
    ) -> Result<Self, StoreError> {
        let mut result = Self {
            store,
            lower,
            exclusive,
            upper,
            stack: Vec::new(),
            rows: Vec::new().into_iter(),
            failed: false,
        };
        if let Some(hash) = store.current.root {
            let reference = store.projection_page(hash, None)?.reference(hash)?;
            if result.overlaps(&reference) {
                result.stack.push(reference);
            }
        }
        Ok(result)
    }
    fn overlaps(&self, child: &format::Child) -> bool {
        child.first < self.upper
            && if self.exclusive {
                child.last > self.lower
            } else {
                child.last >= self.lower
            }
    }
    fn advance(&mut self) -> Result<Option<Row>, StoreError> {
        loop {
            if let Some(row) = self.rows.next() {
                return self.store.projection_row_copy(&row).map(Some);
            }
            let Some(child) = self.stack.pop() else {
                return Ok(None);
            };
            let page = self.store.projection_page(child.hash, Some(&child))?;
            match &page.node {
                format::Node::Leaf(rows) => {
                    self.store.projection_work(rows.len())?;
                    self.rows = rows
                        .iter()
                        .filter(|row| {
                            let key = row.key();
                            key < self.upper
                                && if self.exclusive {
                                    key > self.lower
                                } else {
                                    key >= self.lower
                                }
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_iter();
                }
                format::Node::Index(children) => {
                    self.store.projection_work(children.len())?;
                    let matching: Vec<_> = children
                        .iter()
                        .rev()
                        .filter(|child| self.overlaps(child))
                        .cloned()
                        .collect();
                    self.stack.extend(matching);
                    if self.stack.len() > 8 * 32 {
                        return Err(StoreError::LimitExceeded);
                    }
                }
            }
        }
    }
}
impl Iterator for RangeRows<'_> {
    type Item = Result<Row, StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.advance() {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
pub(in crate::connection) mod test_support {
    use super::*;
    use std::sync::{Mutex, mpsc};
    static WATCH: Mutex<Option<(std::path::PathBuf, mpsc::SyncSender<ReadStats>)>> =
        Mutex::new(None);
    static FAILURE: Mutex<Option<(std::path::PathBuf, bool)>> = Mutex::new(None);
    pub(in crate::connection) fn watch(path: &std::path::Path) -> mpsc::Receiver<ReadStats> {
        let (sender, receiver) = mpsc::sync_channel(64);
        *WATCH.lock().unwrap() = Some((path.to_owned(), sender));
        receiver
    }
    pub(super) fn record(path: &std::path::Path, stats: ReadStats) {
        if let Some((wanted, sender)) = WATCH.lock().unwrap().as_ref() {
            if wanted == path {
                let _ = sender.try_send(stats);
            }
        }
    }
    pub(in crate::connection) fn fail_next_read(path: &std::path::Path, unwind: bool) {
        *FAILURE.lock().unwrap() = Some((path.to_owned(), unwind));
    }
    pub(super) fn read_cap(path: &std::path::Path) -> usize {
        let mut slot = FAILURE.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|(wanted, unwind)| wanted == path && !unwind)
        {
            slot.take();
            1
        } else {
            READ_CAP
        }
    }
    pub(super) fn before_read(path: &std::path::Path) -> Result<(), StoreError> {
        let failure = {
            let mut slot = FAILURE.lock().unwrap();
            if slot.as_ref().is_some_and(|(wanted, _)| wanted == path) {
                slot.take()
            } else {
                None
            }
        };
        match failure {
            Some((_, true)) => panic!("projection read unwind fixture"),
            Some((_, false)) => unreachable!("read-cap fixture consumed at scope entry"),
            None => Ok(()),
        }
    }

    /// Persist incomplete metadata only; no bytes, receipt or native capability.
    pub(in crate::connection) fn checkpoint_inventory(
        store: &mut ConnectionStore,
        id: ConnectionId,
    ) -> Vec<std::path::PathBuf> {
        for (connection, count) in [
            (ConnectionId([1; 16]), 2048_u64),
            (id, 130),
            (ConnectionId([250; 16]), 2048),
        ] {
            for serial in 1..=count {
                let row = format::Recovery {
                    captured: None,
                    connection: connection.0,
                    serial,
                    slot: 0,
                    side: 0,
                    path: RelativePath::unix(format!("metadata-{serial:05}").as_bytes())
                        .unwrap()
                        .into(),
                    reason: 0,
                    state: 0,
                    content: None,
                    object: None,
                    retained: 0,
                };
                store.overlay.insert(row.key(), Row::Recovery(row));
            }
        }
        store.checkpoint().unwrap();
        assert!(store.overlay.is_empty());
        let mut stack = vec![store.current.root.unwrap()];
        let mut unrelated = Vec::new();
        while let Some(hash) = stack.pop() {
            match store.storage.page(hash).unwrap().node {
                format::Node::Index(children) => stack.extend(children.into_iter().map(|child| child.hash)),
                format::Node::Leaf(rows) => {
                    if rows.iter().all(|row| matches!(row, Row::Recovery(entry) if entry.connection == [1;16] || entry.connection == [250;16])) {
                        unrelated.push(store.storage.path.join("pages").join(format::hex(&hash)));
                    }
                }
            }
        }
        assert!(unrelated.len() > 20);
        unrelated
    }

    pub(in crate::connection) fn selected_page(
        store: &ConnectionStore,
        id: ConnectionId,
    ) -> std::path::PathBuf {
        let key = recovery::recovery_key(RecoveryId {
            operation: OperationId {
                connection: id,
                serial: 1,
            },
            slot: 0,
        });
        let mut hash = store.current.root.unwrap();
        loop {
            match store.storage.page(hash).unwrap().node {
                format::Node::Leaf(rows) => {
                    assert!(rows.iter().any(|row| row.key() == key));
                    return store.storage.path.join("pages").join(format::hex(&hash));
                }
                format::Node::Index(children) => {
                    hash = children
                        .into_iter()
                        .find(|child| child.first <= key && key <= child.last)
                        .unwrap()
                        .hash;
                }
            }
        }
    }

    pub(in crate::connection) fn shadow_first(store: &mut ConnectionStore, id: ConnectionId) {
        let key = recovery::recovery_key(RecoveryId {
            operation: OperationId {
                connection: id,
                serial: 1,
            },
            slot: 0,
        });
        let Some(Row::Recovery(mut entry)) = store.row(&key).unwrap() else {
            panic!("checkpointed entry")
        };
        entry.path = RelativePath::unix(b"overlay-shadow").unwrap().into();
        store.overlay.insert(key, Row::Recovery(entry));
    }
}
