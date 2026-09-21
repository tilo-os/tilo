//! Complete initial consent and inert reviewed history. Decoding grants no authority.
#[cfg(test)]
mod tests;

use super::*;
use format::{
    DirectoryChainScheduleState as Schedule, InitialReviewControlTransitionV1 as Control,
};
use format::{InitialReviewCurrentV1 as Current, InitialReviewLinkKindV1 as LinkKind};
use sha2::{Digest, Sha256};

impl format::InitialChainReviewV1 {
    pub(super) fn identity(&self) -> Result<[u8; 32], StoreError> {
        Ok(format::digest(&format::encode(self)?))
    }
    pub(super) fn original_record(&self) -> Result<format::Connection, StoreError> {
        if self.original.len() > format::MAX_BYTES {
            return Err(StoreError::LimitExceeded);
        }
        let original: format::Connection =
            serde_json::from_slice(&self.original).map_err(|_| StoreError::Corrupt)?;
        original.decode()?;
        if format::encode(&original)? != self.original
            || original.id != self.connection
            || original.revision != self.original_revision
            || original.fence != self.fence
            || [
                original.source.identity.clone(),
                original.destination.identity.clone(),
            ] != self.endpoints
            || original.lifecycle != 0
            || original.baseline.is_some()
            || original.pending.is_some()
            || original.serial != 0
            || original.initial_empty.is_some()
            || original.directory_chain.is_some()
            || original.directory_chain_schedule.is_some()
        {
            return Err(StoreError::Corrupt);
        }
        Ok(original)
    }
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.original_revision == 0
            || self.count == 0
            || self.count > 4096
            || self.counts.iter().any(|n| *n > 4096)
            || self.counts.iter().all(|n| *n == 0)
            || self.count < self.counts[0].max(self.counts[1])
            || self.count
                > self.counts[0]
                    .checked_add(self.counts[1])
                    .ok_or(StoreError::Corrupt)?
            || self.encodings.iter().any(|n| !matches!(n, 1 | 2))
        {
            return Err(StoreError::Corrupt);
        }
        self.original_record()?;
        for side in 0..2 {
            self.endpoint(side)?;
        }
        format::encode(self)?;
        Ok(())
    }
    fn endpoint(&self, side: usize) -> Result<super::super::model::Endpoint, StoreError> {
        use super::super::model::{Endpoint, NamePolicy, Observation, ObservedTree};
        let encoding = match self.encodings[side] {
            1 => Encoding::Unix,
            2 => Encoding::Windows,
            _ => return Err(StoreError::Corrupt),
        };
        let names = match &self.policies[side] {
            format::ReviewNamePolicyV1::Exact {} => NamePolicy::Exact,
            format::ReviewNamePolicyV1::Canonical { keys } => {
                if keys.len() > super::super::model::MAX_PLAN_ENTRIES {
                    return Err(StoreError::LimitExceeded);
                }
                let mut result = BTreeMap::new();
                let mut previous = None;
                for key in keys {
                    let path = key.path.to_relative()?;
                    if previous.as_ref().is_some_and(|old| old >= &path)
                        || result
                            .insert(path.clone(), key.components.clone())
                            .is_some()
                    {
                        return Err(StoreError::Corrupt);
                    }
                    previous = Some(path);
                }
                NamePolicy::Canonical(result)
            }
        };
        let endpoint = Endpoint {
            identity: EndpointIdentity {
                volume: self.endpoints[side].volume,
                root: self.endpoints[side].root,
            },
            encoding,
            names,
        };
        ObservedTree::new(endpoint.clone(), [], Observation::Complete)
            .map_err(|_| StoreError::Corrupt)?;
        Ok(endpoint)
    }
}
impl format::InitialChainReviewEntryV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.origin == 0
            || self.ordinal >= 4096
            || self
                .sides
                .iter()
                .all(|side| matches!(side, format::InitialReviewEntrySideV1::Absent {}))
        {
            return Err(StoreError::Corrupt);
        }
        self.path.to_relative()?;
        for side in &self.sides {
            if let format::InitialReviewEntrySideV1::Present { entry } = side {
                if entry.ordinal >= 4096 {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(())
    }
}
impl Current {
    fn operation(&self) -> Option<format::ChainOperationIdV1> {
        match self {
            Self::None {} => None,
            Self::Chain { operation, .. } | Self::Equality { operation, .. } => Some(*operation),
        }
    }
    fn validate(&self, connection: [u8; 16]) -> Result<(), StoreError> {
        match self {
            Self::None {} => Ok(()),
            Self::Chain {
                step,
                entry,
                anchor,
                operation,
                ..
            } => {
                anchor.operation()?;
                operation.operation()?;
                if *step >= 4096
                    || *entry >= 4096
                    || anchor.connection != connection
                    || operation.connection != connection
                    || anchor.serial > operation.serial
                {
                    return Err(StoreError::Corrupt);
                }
                Ok(())
            }
            Self::Equality {
                step,
                entry,
                operation,
            } => {
                operation.operation()?;
                if *step >= 4096 || *entry >= 4096 || operation.connection != connection {
                    return Err(StoreError::Corrupt);
                }
                Ok(())
            }
        }
    }
}
impl format::InitialReviewCursorV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.origin == 0
            || self.progress_revision <= self.origin
            || self.next_step >= 4096
            || self.schedule_revision == 0
            || matches!(self.state, Schedule::Abandoning)
            || (!matches!(self.state, Schedule::Disconnected) && self.schedule_revision == u64::MAX)
            || self
                .control_sequence
                .checked_add(if self.state == Schedule::Disconnected {
                    2
                } else {
                    1
                })
                != Some(self.schedule_revision)
        {
            return Err(StoreError::Corrupt);
        }
        self.current.validate(self.connection)
    }
}
impl format::InitialReviewLinkV1 {
    pub(super) fn current(&self) -> Current {
        match &self.kind {
            LinkKind::Chain { anchor, digest, .. } => Current::Chain {
                step: self.step,
                entry: self.entry,
                anchor: *anchor,
                digest: *digest,
                operation: self.operation,
            },
            LinkKind::Equality {} => Current::Equality {
                step: self.step,
                entry: self.entry,
                operation: self.operation,
            },
        }
    }
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.origin == 0
            || self.operation.connection != self.connection
            || matches!(self.kind, LinkKind::Chain { review_revision, .. } if review_revision <= self.origin)
        {
            return Err(StoreError::Corrupt);
        }
        self.current().validate(self.connection)
    }
}
impl format::InitialReviewStopBoundaryV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        let f = self.fact();
        if f.version != 1
            || f.origin == 0
            || f.operation.connection != f.connection
            || f.step >= 4096
            || f.entry >= 4096
            || !matches!(f.stage, 0 | 3)
            || f.before_revision != f.before_progress
            || f.before_revision <= f.origin
            || f.before_revision.checked_add(1) != Some(f.resulting_revision)
            || f.resulting_revision != f.resulting_progress
            || f.control_sequence.checked_add(1) != Some(f.schedule_revision)
            || f.control_sequence == 0
            || f.schedule_revision == u64::MAX
        {
            return Err(StoreError::Corrupt);
        }
        f.operation.operation()?;
        Ok(())
    }
}
impl format::ReviewedInnerControlV1 {
    pub(super) fn canonical(&self) -> Change {
        Change::DirectoryChainControl {
            connection: self.connection,
            anchor: self.anchor,
            digest: self.digest,
            expected_progress: self.expected_progress,
            expected_schedule: self.expected_schedule,
            control_sequence: self.control_sequence,
            transition: self.transition,
        }
    }
}

impl ConnectionStore {
    pub(super) fn require_no_review_or_chain_history(
        &self,
        id: ConnectionId,
    ) -> Result<(), StoreError> {
        let connection = self.connection(id)?;
        if connection.directory_chain.is_some() || connection.directory_chain_schedule.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        for row in self.rows()? {
            let row = row?;
            if review_connection(&row).is_some_and(|owner| owner == id.0) {
                return Err(StoreError::RecoveryRequired);
            }
            if let Row::Operation(operation) = row {
                if operation.connection == id.0
                    && (operation.directory_chain_anchor.is_some()
                        || operation.directory_chain_ref.is_some()
                        || operation.directory_chain_retired.is_some()
                        || operation.directory_chain_stop_boundary.is_some())
                {
                    return Err(StoreError::RecoveryRequired);
                }
            }
        }
        Ok(())
    }
}
pub(super) fn review_connection(row: &Row) -> Option<[u8; 16]> {
    match row {
        Row::InitialChainReview(r) => Some(r.connection),
        Row::InitialChainReviewEntry(r) => Some(r.connection),
        Row::InitialReviewCursor(r) => Some(r.connection),
        Row::InitialReviewLink(r) => Some(r.connection),
        Row::InitialReviewStop(r) => Some(r.fact().connection),
        _ => None,
    }
}

/// Validated, read-only consent. It deliberately has no session/proof constructor.
pub(super) struct ReviewView {
    pub fact: format::InitialChainReviewV1,
    pub entries: Vec<format::InitialChainReviewEntryV1>,
}
impl ConnectionStore {
    pub(super) fn read_review(
        &self,
        fact: &format::InitialChainReviewV1,
    ) -> Result<ReviewView, StoreError> {
        fact.validate()?;
        let root = self.storage.page(fact.root)?.reference(fact.root)?;
        if root.count != u64::from(fact.count) {
            return Err(StoreError::Corrupt);
        }
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut previous = None;
        let mut trees: [Vec<(RelativePath, EntryState)>; 2] = [Vec::new(), Vec::new()];
        let mut hash = Sha256::new();
        hash.update(b"Connected explicit initial preview v1");
        hash.update(&fact.original);
        for row in storage::TreeIter::new_review(&self.storage, fact.root, fact.count)? {
            let Row::InitialChainReviewEntry(entry) = row? else {
                return Err(StoreError::Corrupt);
            };
            entry.validate()?;
            if entries.len() >= fact.count as usize
                || entry.ordinal as usize != entries.len()
                || entry.connection != fact.connection
                || entry.origin != fact.original_revision
            {
                return Err(StoreError::Corrupt);
            }
            bytes = bytes
                .checked_add(format::encode(&Row::InitialChainReviewEntry(entry.clone()))?.len())
                .ok_or(StoreError::LimitExceeded)?;
            if bytes > 32 * 1024 * 1024 {
                return Err(StoreError::LimitExceeded);
            }
            let path = entry.path.to_relative()?;
            if previous.as_ref().is_some_and(|old| old >= &path) {
                return Err(StoreError::Corrupt);
            }
            previous = Some(path.clone());
            for side in 0..2 {
                if let format::InitialReviewEntrySideV1::Present { entry: value } =
                    &entry.sides[side]
                {
                    if value.ordinal as usize != trees[side].len() {
                        return Err(StoreError::Corrupt);
                    }
                    trees[side].push((path.clone(), value.state.clone().into()));
                }
            }
            entries.push(entry);
        }
        if entries.len() != fact.count as usize {
            return Err(StoreError::Corrupt);
        }
        for (side, tree) in trees.into_iter().enumerate() {
            if tree.len() != fact.counts[side] as usize {
                return Err(StoreError::Corrupt);
            }
            hash.update([side as u8]);
            hash.update(u64::from(fact.counts[side]).to_le_bytes());
            // Validate exact original per-side order/encoding/ancestors and policy.
            let observed = super::super::model::ObservedTree::new(
                fact.endpoint(side)?,
                tree,
                super::super::model::Observation::Complete,
            )
            .map_err(|_| StoreError::Corrupt)?;
            for (ordinal, (path, state)) in observed.entries().iter().enumerate() {
                let bytes = format::encode(&format::BaselineEntry {
                    connection: fact.connection,
                    revision: fact.original_revision,
                    ordinal: u32::try_from(ordinal).map_err(|_| StoreError::Corrupt)?,
                    path: path.clone().into(),
                    state: state.clone().into(),
                })?;
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        if <[u8; 32]>::from(hash.finalize()) != fact.preview {
            return Err(StoreError::Corrupt);
        }
        Ok(ReviewView {
            fact: fact.clone(),
            entries,
        })
    }
    fn review_cursor(
        &self,
        connection: [u8; 16],
        origin: u64,
    ) -> Result<format::InitialReviewCursorV1, StoreError> {
        match self.row(&format::key(9, connection, origin))? {
            Some(Row::InitialReviewCursor(cursor)) => Ok(cursor),
            _ => Err(StoreError::Corrupt),
        }
    }
    fn review_fact(
        &self,
        connection: [u8; 16],
        origin: u64,
    ) -> Result<format::InitialChainReviewV1, StoreError> {
        match self.row(&format::key(7, connection, origin))? {
            Some(Row::InitialChainReview(review)) => Ok(review),
            _ => Err(StoreError::Corrupt),
        }
    }
    fn reduce_review_acceptance(
        &self,
        review: &format::InitialChainReviewV1,
        cursor: &format::InitialReviewCursorV1,
    ) -> Result<Vec<Row>, StoreError> {
        self.require_no_review_or_chain_history(ConnectionId(review.connection))?;
        self.read_review(review)?;
        let mut connection = self.connection(ConnectionId(review.connection))?;
        if connection != review.original_record()? {
            return Err(StoreError::StaleRevision);
        }
        connection.revision = format::next(connection.revision)?;
        let expected = format::InitialReviewCursorV1 {
            version: 1,
            connection: review.connection,
            review: review.identity()?,
            origin: review.original_revision,
            progress_revision: connection.revision,
            schedule_revision: format::next(0)?,
            control_sequence: 0,
            state: Schedule::Running,
            next_step: 0,
            current: Current::None {},
        };
        if *cursor != expected {
            return Err(StoreError::Corrupt);
        }
        Ok(vec![
            Row::Connection(connection),
            Row::InitialChainReview(review.clone()),
            Row::InitialReviewCursor(cursor.clone()),
        ])
    }
    pub(super) fn validate_review_history(&self, replacements: &[Row]) -> Result<(), StoreError> {
        let mut rows = BTreeMap::new();
        for row in self.rows()? {
            let row = row?;
            rows.insert(row.key(), row);
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in replacements {
            if !keys.insert(row.key()) {
                return Err(StoreError::Corrupt);
            }
            row.validate()?;
            rows.insert(row.key(), row.clone());
        }
        let mut owners = std::collections::BTreeSet::new();
        for row in rows.values() {
            self.storage.review_charge(0, 0, 1, 1)?;
            let Some(id) = review_connection(row) else {
                continue;
            };
            row.validate()?;
            if let Row::InitialChainReview(review) = row {
                if !owners.insert(id) {
                    return Err(StoreError::Corrupt);
                }
                let view = self.read_review(review)?;
                self.storage.review_charge(
                    0,
                    0,
                    0,
                    u64::from(review.count)
                        .checked_mul(64)
                        .ok_or(StoreError::LimitExceeded)?,
                )?;
                let Some(Row::Connection(c)) = rows.get(&format::key(0, id, 0)) else {
                    return Err(StoreError::Corrupt);
                };
                let Some(Row::InitialReviewCursor(cursor)) =
                    rows.get(&format::key(9, id, review.original_revision))
                else {
                    return Err(StoreError::Corrupt);
                };
                let original = review.original_record()?;
                if c.source != original.source
                    || c.destination != original.destination
                    || cursor.review != review.identity()?
                    || cursor.origin != review.original_revision
                    || c.source.identity != review.endpoints[0]
                    || c.destination.identity != review.endpoints[1]
                    || c.baseline.is_some()
                    || c.initial_empty.is_some()
                    || cursor.next_step != 0
                    || cursor.progress_revision > c.revision
                {
                    return Err(StoreError::Corrupt);
                }
                if cursor.state == Schedule::Disconnected {
                    if c.lifecycle == 0 || c.fence != format::next(review.fence)? {
                        return Err(StoreError::Corrupt);
                    }
                } else if c.lifecycle != 0
                    || c.fence != review.fence
                    || c.revision != cursor.progress_revision
                {
                    return Err(StoreError::Corrupt);
                }
                match &cursor.current {
                    Current::None {} => {
                        if c.pending.is_some()
                            || c.serial != 0
                            || cursor.progress_revision != format::next(review.original_revision)?
                            || c.directory_chain.is_some()
                            || c.directory_chain_schedule.is_some()
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    current => {
                        let id = current.operation().ok_or(StoreError::Corrupt)?;
                        let Some(Row::Operation(op)) =
                            rows.get(&format::key(1, id.connection, id.serial))
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        let Some(Row::InitialReviewLink(link)) =
                            rows.get(&format::key(10, id.connection, id.serial))
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        if link.current() != *current
                            || c.serial != id.serial
                            || link.step != cursor.next_step
                        {
                            return Err(StoreError::Corrupt);
                        }
                        let anchor = match &link.kind {
                            LinkKind::Chain { anchor, .. } => {
                                match rows.get(&format::key(1, anchor.connection, anchor.serial)) {
                                    Some(Row::Operation(op)) => Some(op),
                                    _ => return Err(StoreError::Corrupt),
                                }
                            }
                            _ => None,
                        };
                        view.validate_link(link, op, anchor)?;
                        if cursor.state != Schedule::Disconnected {
                            if c.pending != Some(id.serial)
                                || op.revision != cursor.progress_revision
                            {
                                return Err(StoreError::Corrupt);
                            }
                        } else if op.revision < cursor.progress_revision {
                            return Err(StoreError::Corrupt);
                        }
                        match current {
                            Current::Chain { anchor, digest, .. } => {
                                let inner =
                                    c.directory_chain.as_ref().ok_or(StoreError::Corrupt)?;
                                let schedule = c
                                    .directory_chain_schedule
                                    .as_ref()
                                    .ok_or(StoreError::Corrupt)?;
                                if inner.anchor != *anchor
                                    || inner.digest != *digest
                                    || inner.current != Some(id)
                                    || inner.progress_revision != cursor.progress_revision
                                    || schedule.state != cursor.state
                                {
                                    return Err(StoreError::Corrupt);
                                }
                            }
                            Current::Equality { .. } => {
                                if c.directory_chain.is_some() || c.directory_chain_schedule.is_some()
                                    || !matches!(op.stage,0|3|7) || (op.stage==7 && (cursor.state!=Schedule::Disconnected || c.pending.is_some() || op.native_boundary!=Some(0)))
                                    || (op.stage!=7 && c.pending!=Some(id.serial))
                                    || rows.values().any(|r|matches!(r,Row::NativeRegistration(reg) if reg.connection==id.connection && reg.serial==id.serial))
                                { return Err(StoreError::Corrupt); }
                                if let Some(Row::InitialReviewStop(boundary)) =
                                    rows.get(&format::key(11, id.connection, id.serial))
                                {
                                    self.validate_saved_equality_stop(
                                        &view, link, cursor, op, boundary,
                                    )?;
                                } else if matches!(cursor.state, Schedule::Stopped { .. }) {
                                    return Err(StoreError::Corrupt);
                                }
                            }
                            _ => return Err(StoreError::Corrupt),
                        }
                    }
                }
            }
        }
        for row in rows.values() {
            self.storage.review_charge(0, 0, 1, 1)?;
            let Some(id) = review_connection(row) else {
                continue;
            };
            match row {
                Row::InitialChainReview(_) => (),
                Row::InitialReviewCursor(cursor) if owners.contains(&id) => {
                    if !matches!(
                        rows.get(&format::key(7, id, cursor.origin)),
                        Some(Row::InitialChainReview(_))
                    ) {
                        return Err(StoreError::Corrupt);
                    }
                }
                Row::InitialReviewLink(link) if owners.contains(&id) => {
                    let Some(Row::InitialReviewCursor(cursor)) =
                        rows.get(&format::key(9, id, link.origin))
                    else {
                        return Err(StoreError::Corrupt);
                    };
                    if cursor.current != link.current() {
                        return Err(StoreError::Corrupt);
                    }
                }
                Row::InitialReviewStop(boundary) if owners.contains(&id) => {
                    let f = boundary.fact();
                    let Some(Row::InitialReviewCursor(cursor)) =
                        rows.get(&format::key(9, id, f.origin))
                    else {
                        return Err(StoreError::Corrupt);
                    };
                    if cursor.current
                        != (Current::Equality {
                            step: f.step,
                            entry: f.entry,
                            operation: f.operation,
                        })
                    {
                        return Err(StoreError::Corrupt);
                    }
                }
                // Entry rows are valid only in the immutable referenced page tree.
                _ => return Err(StoreError::Corrupt),
            }
        }
        for row in rows.values() {
            self.storage.review_charge(0, 0, 1, 1)?;
            if let Row::Operation(op) = row {
                if owners.contains(&op.connection)
                    && !matches!(
                        rows.get(&format::key(10, op.connection, op.serial)),
                        Some(Row::InitialReviewLink(_))
                    )
                {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(())
    }
    pub(super) fn reduce_review_change(&self, change: &Change) -> Result<Vec<Row>, StoreError> {
        match change {
            Change::InitialReviewAccepted { review, cursor } => {
                self.reduce_review_acceptance(review, cursor)
            }
            Change::InitialReviewControl { control } => Ok(vec![Row::InitialReviewCursor(
                self.reduce_outer_control(control, None)?,
            )]),
            Change::ReviewedOrdinaryStopBoundary { boundary } => {
                self.reduce_equality_stop(boundary)
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
}

impl ConnectionStore {
    #[cfg(test)]
    pub(in crate::connection) fn review_read_lease(&self) -> Result<ReviewReadLease, StoreError> {
        self.storage.read_lease(true)
    }
    pub(in crate::connection) fn initial_review_preimage(
        &self,
        id: ConnectionId,
    ) -> Result<Vec<u8>, StoreError> {
        format::encode(&self.connection(id)?)
    }
    #[cfg(test)]
    pub(in crate::connection) fn validate_review_owner(
        &self,
        record: &ConnectionRecord,
        lifetime: &std::sync::Weak<()>,
        gate: &std::sync::Weak<super::super::transaction::AdmissionGate>,
        original: &[u8],
    ) -> Result<(), StoreError> {
        self.require_no_review_or_chain_history(record.id)?;
        if lifetime.strong_count() == 0
            || !self.same_native_lifetime(lifetime)
            || self.load_connection(record.id)? != *record
            || record.lifecycle != Lifecycle::Active
            || record.baseline_revision.is_some()
            || self.executor_position(record.id)? != (None, None)
            || self.initial_review_preimage(record.id)? != original
        {
            return Err(StoreError::StaleRevision);
        }
        let active = self
            .native_gates
            .get(&record.id.0)
            .ok_or(StoreError::StaleRevision)?;
        if !gate.ptr_eq(&Arc::downgrade(active))
            || active.stopped().load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(StoreError::Cancelled);
        }
        active.require_quiescent()?;
        Ok(())
    }
    /// Consumes explicit fresh consent and returns only a durable metadata acknowledgement.
    #[cfg(test)]
    pub(in crate::connection) fn accept_initial_set(
        &mut self,
        owner: super::super::transaction::ReviewedInitialSet,
    ) -> Result<DurableAck<Revision>, StoreError> {
        owner.validate(self)?;
        let trees = owner.trees();
        let mut union: BTreeMap<RelativePath, [format::InitialReviewEntrySideV1; 2]> =
            BTreeMap::new();
        for (side, tree) in trees.iter().enumerate() {
            for (ordinal, (path, state)) in tree.entries().iter().enumerate() {
                let value = union.entry(path.clone()).or_insert_with(|| {
                    [
                        format::InitialReviewEntrySideV1::Absent {},
                        format::InitialReviewEntrySideV1::Absent {},
                    ]
                });
                value[side] = format::InitialReviewEntrySideV1::Present {
                    entry: format::InitialReviewSideV1 {
                        ordinal: u32::try_from(ordinal).map_err(|_| StoreError::LimitExceeded)?,
                        state: state.clone().into(),
                    },
                };
                if union.len() > 4096 {
                    return Err(StoreError::LimitExceeded);
                }
            }
        }
        if union.is_empty() {
            return Err(StoreError::LimitExceeded);
        }
        let count = u32::try_from(union.len()).map_err(|_| StoreError::LimitExceeded)?;
        let entries: Vec<Row> = union
            .into_iter()
            .enumerate()
            .map(|(ordinal, (path, sides))| {
                Row::InitialChainReviewEntry(format::InitialChainReviewEntryV1 {
                    version: 1,
                    connection: owner.record().id.0,
                    origin: owner.record().revision,
                    ordinal: ordinal as u32,
                    path: path.into(),
                    sides,
                })
            })
            .collect();
        let mut size = 0usize;
        for entry in &entries {
            size = size
                .checked_add(format::encode(entry)?.len())
                .ok_or(StoreError::LimitExceeded)?;
            if size > 32 * 1024 * 1024 {
                return Err(StoreError::LimitExceeded);
            }
            format::encode(&format::PageNode {
                schema: 1,
                node: format::Node::Leaf(vec![entry.clone()]),
            })?;
        }
        let policy = |tree: &super::super::model::ObservedTree| match &tree.endpoint().names {
            super::super::model::NamePolicy::Exact => format::ReviewNamePolicyV1::Exact {},
            super::super::model::NamePolicy::Canonical(keys) => {
                format::ReviewNamePolicyV1::Canonical {
                    keys: keys
                        .iter()
                        .map(|(path, components)| format::ReviewNameKeyV1 {
                            path: path.clone().into(),
                            components: components.clone(),
                        })
                        .collect(),
                }
            }
        };
        let mut review = format::InitialChainReviewV1 {
            version: 1,
            connection: owner.record().id.0,
            original_revision: owner.record().revision,
            fence: owner.record().fence_generation,
            preview: owner.preview(),
            endpoints: [
                owner.record().source.identity.into(),
                owner.record().destination.identity.into(),
            ],
            encodings: trees.map(|tree| {
                if tree.endpoint().encoding == Encoding::Unix {
                    1
                } else {
                    2
                }
            }),
            policies: [policy(trees[0]), policy(trees[1])],
            counts: trees.map(|tree| tree.entries().len() as u32),
            count,
            root: [255; 32],
            original: owner.original().to_vec(),
        };
        // JSON octets have variable decimal width; use all-255 root/digest bytes for capacity.
        let make_cursor = |review: &format::InitialChainReviewV1| -> Result<_, StoreError> {
            Ok(format::InitialReviewCursorV1 {
                version: 1,
                connection: review.connection,
                review: review.identity()?,
                origin: review.original_revision,
                progress_revision: format::next(review.original_revision)?,
                schedule_revision: format::next(0)?,
                control_sequence: 0,
                state: Schedule::Running,
                next_step: 0,
                current: Current::None {},
            })
        };
        review.validate()?;
        let mut cursor = make_cursor(&review)?;
        cursor.review = [255; 32];
        let preview = Event {
            schema: 1,
            predecessor: self.digest,
            change: Change::InitialReviewAccepted {
                review: review.clone(),
                cursor,
            },
        };
        format::encode(&preview)?;
        format::encode(&format::PageNode {
            schema: 1,
            node: format::Node::Leaf(vec![Row::InitialChainReview(review.clone())]),
        })?;
        owner.validate(self)?;
        review.root = storage::build_pages(&self.storage, entries.into_iter().map(Ok))?
            .ok_or(StoreError::Corrupt)?;
        let cursor = make_cursor(&review)?;
        let revision = cursor.progress_revision;
        owner.validate(self)?;
        self.append_checked_with_baseline(
            Change::InitialReviewAccepted { review, cursor },
            RecoveryChecks::default(),
            BaselineAppendCheck::InitialReview(&owner),
        )?;
        Ok(self.ack(revision))
    }
}

pub(super) fn canonical_component(change: &Change) -> Option<Change> {
    match change {
        Change::ReviewedIntent { operation, .. } => Some(Change::Intent {
            operation: operation.clone(),
        }),
        Change::ReviewedChainControl { inner, .. } => Some(inner.canonical()),
        _ => None,
    }
}
impl ConnectionStore {
    fn owned_review(
        &self,
        id: [u8; 16],
    ) -> Result<Option<format::InitialChainReviewV1>, StoreError> {
        let mut found = None;
        for row in self.rows()? {
            if let Row::InitialChainReview(review) = row? {
                if review.connection == id {
                    if found.replace(review).is_some() {
                        return Err(StoreError::Corrupt);
                    }
                }
            }
        }
        Ok(found)
    }
    pub(super) fn review_event_guard(&self, change: &Change) -> Result<(), StoreError> {
        if matches!(
            change,
            Change::Create { .. } | Change::InitialReviewAccepted { .. }
        ) {
            return Ok(());
        }
        let id = super::chain::change_connection(change)?;
        let Some(review) = self.owned_review(id)? else {
            return if matches!(
                change,
                Change::ReviewedIntent { .. }
                    | Change::ReviewedChainControl { .. }
                    | Change::InitialReviewControl { .. }
                    | Change::ReviewedOrdinaryStopBoundary { .. }
            ) {
                Err(StoreError::RecoveryRequired)
            } else {
                Ok(())
            };
        };
        let cursor = self.review_cursor(id, review.original_revision)?;
        let running = cursor.state == Schedule::Running;
        let current = cursor.current.operation();
        let serial = match change {
            Change::Fence { .. }
            | Change::Disconnect { .. }
            | Change::InitialReviewControl { .. }
            | Change::ReviewedChainControl { .. } => return Ok(()),
            Change::ReviewedIntent { .. }
                if running && matches!(cursor.current, Current::None {}) =>
            {
                return Ok(());
            }
            Change::ReviewedOrdinaryStopBoundary { .. }
                if matches!(cursor.state, Schedule::Stopping { .. }) =>
            {
                return Ok(());
            }
            Change::ReadReserve {
                serial,
                recovery_only,
                ..
            } if running || *recovery_only => *serial,
            Change::NativeReserve { registration, .. }
            | Change::NativePrepared { registration, .. }
            | Change::NativeOutcome { registration, .. }
                if running =>
            {
                registration.serial
            }
            Change::Verified { serial, .. } | Change::Published { serial, .. } if running => {
                *serial
            }
            Change::DirectoryChainStopBoundary { serial, .. }
                if matches!(cursor.state, Schedule::Stopping { .. }) =>
            {
                *serial
            }
            Change::NativeBoundary { serial, .. } | Change::DirectoryBoundary { serial, .. }
                if cursor.state == Schedule::Disconnected =>
            {
                *serial
            }
            // Completion/archive evidence is deliberately not implemented in slice 1.
            _ => return Err(StoreError::RecoveryRequired),
        };
        if current
            != Some(format::ChainOperationIdV1 {
                connection: id,
                serial,
            })
        {
            return Err(StoreError::StaleRevision);
        }
        Ok(())
    }
    fn reduce_outer_control(
        &self,
        control: &format::InitialReviewControlV1,
        inner: Option<&format::ReviewedInnerControlV1>,
    ) -> Result<format::InitialReviewCursorV1, StoreError> {
        control.validate()?;
        let mut cursor = self.review_cursor(control.connection, control.origin)?;
        let c = self.connection(ConnectionId(control.connection))?;
        if control.version != 1
            || control.review != cursor.review
            || control.expected_progress != cursor.progress_revision
            || control.expected_schedule != cursor.schedule_revision
            || control.control_sequence != format::next(cursor.control_sequence)?
            || control.current != cursor.current
            || c.lifecycle != 0
            || cursor.progress_revision != c.revision
            || format::next(cursor.schedule_revision)? == u64::MAX
        {
            return Err(StoreError::StaleRevision);
        }
        match (&cursor.current, inner) {
            (Current::Chain { anchor, digest, .. }, Some(inner)) => {
                let expected = match control.transition {
                    Control::StopRequested { reason } => {
                        format::DirectoryChainControlTransitionV1::StopRequested { reason }
                    }
                    Control::StopAcknowledged {} => {
                        format::DirectoryChainControlTransitionV1::StopAcknowledged
                    }
                    Control::Resume {} => format::DirectoryChainControlTransitionV1::Resume,
                };
                if inner.connection != control.connection
                    || inner.anchor != *anchor
                    || inner.digest != *digest
                    || inner.expected_progress != cursor.progress_revision
                    || inner.transition != expected
                {
                    return Err(StoreError::StaleRevision);
                }
                // Canonical reducer checks the distinct inner schedule/control tuple.
                self.reduce_chain_control(&inner.canonical())?;
            }
            (Current::Chain { .. }, None) | (_, Some(_)) => return Err(StoreError::Corrupt),
            (_, None) if c.directory_chain.is_some() || c.directory_chain_schedule.is_some() => {
                return Err(StoreError::Corrupt);
            }
            _ => (),
        }
        cursor.state = match (&cursor.state, &control.transition) {
            (Schedule::Running, Control::StopRequested { reason }) => {
                Schedule::Stopping { reason: *reason }
            }
            (Schedule::Stopping { reason }, Control::StopAcknowledged {}) => {
                match &cursor.current {
                    Current::None {} if c.pending.is_none() => (),
                    Current::Equality { operation, .. } => {
                        let Some(Row::InitialReviewStop(boundary)) =
                            self.row(&format::key(11, control.connection, operation.serial))?
                        else {
                            return Err(StoreError::RecoveryRequired);
                        };
                        let f = boundary.fact();
                        let op = self.operation(operation.operation()?)?;
                        if c.pending != Some(operation.serial)
                            || f.review != cursor.review
                            || f.operation != *operation
                            || f.resulting_revision != op.revision
                            || f.resulting_progress != cursor.progress_revision
                            || f.schedule_revision != cursor.schedule_revision
                            || f.control_sequence != cursor.control_sequence
                        {
                            return Err(StoreError::StaleRevision);
                        }
                    }
                    Current::Chain { .. } if inner.is_some() => (),
                    _ => return Err(StoreError::RecoveryRequired),
                }
                Schedule::Stopped { reason: *reason }
            }
            (Schedule::Stopped { .. }, Control::Resume {}) => Schedule::Running,
            _ => return Err(StoreError::RecoveryRequired),
        };
        cursor.schedule_revision = format::next(cursor.schedule_revision)?;
        cursor.control_sequence = control.control_sequence;
        cursor.validate()?;
        Ok(cursor)
    }
    pub(super) fn complete_review_rows(
        &self,
        change: &Change,
        rows: &mut Vec<Row>,
    ) -> Result<(), StoreError> {
        if matches!(
            change,
            Change::Create { .. }
                | Change::InitialReviewAccepted { .. }
                | Change::InitialReviewControl { .. }
                | Change::ReviewedOrdinaryStopBoundary { .. }
        ) {
            return Ok(());
        }
        if let Change::ReviewedChainControl { control, inner } = change {
            rows.push(Row::InitialReviewCursor(
                self.reduce_outer_control(control, Some(inner))?,
            ));
            return Ok(());
        }
        let id = super::chain::change_connection(change)?;
        let Some(review) = self.owned_review(id)? else {
            return Ok(());
        };
        let mut cursor = self.review_cursor(id, review.original_revision)?;
        let resulting = rows
            .iter()
            .find_map(|row| match row {
                Row::Connection(c) if c.id == id => Some(c),
                _ => None,
            })
            .ok_or(StoreError::Corrupt)?;
        if matches!(change, Change::Fence { .. }) {
            cursor.schedule_revision = format::next(cursor.schedule_revision)?;
            cursor.state = Schedule::Disconnected;
        } else if cursor.state != Schedule::Disconnected {
            cursor.progress_revision = resulting.revision;
        }
        if let Change::ReviewedIntent {
            operation,
            link,
            expected_progress,
            expected_schedule,
            control_sequence,
        } = change
        {
            let old = self.review_cursor(id, review.original_revision)?;
            if old.current != (Current::None {})
                || old.state != Schedule::Running
                || old.progress_revision != *expected_progress
                || old.schedule_revision != *expected_schedule
                || old.control_sequence != *control_sequence
                || old.review != link.review
                || link.step != old.next_step
            {
                return Err(StoreError::StaleRevision);
            }
            let view = self.read_review(&review)?;
            view.validate_link(
                link,
                operation,
                match &link.kind {
                    LinkKind::Chain { anchor, .. } if anchor.serial == operation.serial => {
                        Some(operation)
                    }
                    _ => None,
                },
            )?;
            cursor.current = link.current();
            rows.push(Row::InitialReviewLink(link.clone()));
        }
        rows.push(Row::InitialReviewCursor(cursor));
        Ok(())
    }
}

impl ReviewView {
    pub(super) fn selection(
        &self,
        ordinal: u32,
        revision: u64,
    ) -> Result<super::super::transaction::chain::DirectoryChainSelectionV1, StoreError> {
        use super::super::transaction::chain::{
            DirectoryChainSelectionV1, DirectoryChainTerminalV1, RegularContentV1,
        };
        use format::InitialReviewEntrySideV1 as S;
        if revision <= self.fact.original_revision {
            return Err(StoreError::StaleRevision);
        }
        let entry = self
            .entries
            .get(ordinal as usize)
            .ok_or(StoreError::Corrupt)?;
        let (from, side, state) = match &entry.sides {
            [S::Present { entry }, S::Absent {}] => (Side::Source, 0, &entry.state),
            [S::Absent {}, S::Present { entry }] => (Side::Destination, 1, &entry.state),
            _ => return Err(StoreError::RecoveryRequired),
        };
        let terminal = match state {
            format::State::Directory => DirectoryChainTerminalV1::DirectoryAdd,
            format::State::File {
                sha256,
                bytes,
                executable,
            } => DirectoryChainTerminalV1::RegularAdd {
                content: RegularContentV1 {
                    sha256: *sha256,
                    bytes: *bytes,
                    executable: *executable,
                },
            },
        };
        let path = entry.path.to_relative()?;
        let mut existing = 0u64;
        let mut missing = 0u64;
        let mut parent = path.parent();
        while let Some(path) = parent {
            let original = self
                .entries
                .iter()
                .find(|entry| {
                    entry
                        .path
                        .to_relative()
                        .is_ok_and(|candidate| candidate == path)
                })
                .ok_or(StoreError::Corrupt)?;
            if !matches!(&original.sides[side],S::Present {entry} if entry.state==format::State::Directory)
            {
                return Err(StoreError::RecoveryRequired);
            }
            let bit = 1u64
                .checked_shl(path.components().len() as u32)
                .ok_or(StoreError::LimitExceeded)?;
            match &original.sides[1 - side] {
                S::Present { entry } if entry.state == format::State::Directory => existing |= bit,
                S::Absent {} => missing |= bit,
                _ => return Err(StoreError::RecoveryRequired),
            }
            parent = path.parent();
        }
        DirectoryChainSelectionV1 {
            version: 1,
            connection: ConnectionId(self.fact.connection),
            endpoints: self.fact.endpoints.clone().map(|id| EndpointIdentity {
                volume: id.volume,
                root: id.root,
            }),
            review_revision: revision,
            original_fence: self.fact.fence,
            baseline: None,
            preview_digest: self.fact.preview,
            selection_digest: [0; 32],
            path,
            from,
            terminal,
            baselined_prefixes: 0,
            existing_prefixes: existing,
            missing_prefixes: missing,
        }
        .checked()
    }
    fn validate_link(
        &self,
        link: &format::InitialReviewLinkV1,
        op: &format::Operation,
        anchor: Option<&format::Operation>,
    ) -> Result<(), StoreError> {
        link.validate()?;
        let entry = self
            .entries
            .get(link.entry as usize)
            .ok_or(StoreError::Corrupt)?;
        if link.review != self.fact.identity()?
            || link.connection != self.fact.connection
            || link.origin != self.fact.original_revision
            || link.step != 0
            || link.operation.connection != op.connection
            || link.operation.serial != op.serial
            || link.entry_digest != format::digest(&format::encode(entry)?)
            || op.source_binding != self.fact.endpoints[0]
            || op.destination_binding != self.fact.endpoints[1]
            || op.action.is_some()
            || op.initial_review.is_some()
            || op.native_budget.is_none()
            || op.native_identity.is_none()
            || op.native_capture.is_some()
            || !op.native_capture_displaced.is_empty()
            || !op.recovery.is_empty()
        {
            return Err(StoreError::Corrupt);
        }
        match &link.kind {
            LinkKind::Chain {
                anchor: id,
                digest,
                review_revision,
            } => {
                let anchor = anchor.ok_or(StoreError::Corrupt)?;
                let selected = self.selection(link.entry, *review_revision)?;
                if anchor.connection != id.connection
                    || anchor.serial != id.serial
                    || anchor.expected != *review_revision
                    || super::chain::first_selection(anchor)? != selected
                    || selected.selection_digest != *digest
                    || op
                        .directory_chain_ref
                        .as_ref()
                        .is_none_or(|r| r.anchor != *id || r.digest != *digest)
                {
                    return Err(StoreError::Corrupt);
                }
            }
            LinkKind::Equality {} => {
                let admission = super::super::transaction::native::descriptor::Admission::decode(
                    op.native_identity.as_deref().ok_or(StoreError::Corrupt)?,
                )
                .map_err(|_| StoreError::Corrupt)?;
                if admission.fence != self.fact.fence {
                    return Err(StoreError::Corrupt);
                }
                let sides = entry.sides.clone().map(|side| match side {
                    format::InitialReviewEntrySideV1::Present { entry } => Some(entry.state),
                    _ => None,
                });
                if !matches!(&sides[0], Some(format::State::File { .. }))
                    || sides[0] != sides[1]
                    || op.path != entry.path
                    || op.source != sides[0]
                    || op.destination != sides[1]
                    || op.decision != 0
                    || op.native_kind.is_some()
                    || op.directory_chain_ref.is_some()
                    || op.directory_chain_anchor.is_some()
                    || op.directory_chain_retired.is_some()
                    || op.directory_chain_stop_boundary.is_some()
                    || op.expected != format::next(self.fact.original_revision)?
                    || op.baseline.is_some()
                {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(())
    }
}

fn original_equality_intent_digest(operation: &format::Operation) -> Result<[u8; 32], StoreError> {
    let mut original = operation.clone();
    original.revision = format::next(original.expected)?;
    original.stage = 0;
    original.native_budget = Some(format::NativeBudget::initial());
    original.native_boundary = None;
    original.native_boundary_evidence = None;
    original.native_directory_boundary = None;
    Ok(format::digest(&format::encode(&original)?))
}
impl ConnectionStore {
    fn validate_saved_equality_stop(
        &self,
        view: &ReviewView,
        link: &format::InitialReviewLinkV1,
        cursor: &format::InitialReviewCursorV1,
        operation: &format::Operation,
        boundary: &format::InitialReviewStopBoundaryV1,
    ) -> Result<(), StoreError> {
        boundary.validate()?;
        let f = boundary.fact();
        if !matches!(link.kind, LinkKind::Equality {})
            || f.review != cursor.review
            || f.origin != cursor.origin
            || f.step != link.step
            || f.entry != link.entry
            || f.operation != link.operation
            || f.fence != view.fact.fence
            || f.admission_digest
                != format::digest(
                    operation
                        .native_identity
                        .as_deref()
                        .ok_or(StoreError::Corrupt)?,
                )
            || f.intent_digest != original_equality_intent_digest(operation)?
            // Saved stage must precede the real current equality stage. Projecting
            // a clone cannot invent Verified before the first Verified transition.
            || !matches!((f.stage, operation.stage), (0, 0 | 3 | 7) | (3, 3 | 7))
            || (f.stage == 3 && f.before_revision < format::next(format::next(operation.expected)?)?)
            || (f.stage == 0 && operation.stage == 3 && operation.revision <= f.resulting_revision)
            || f.before_revision < format::next(operation.expected)?
            || f.resulting_revision > operation.revision
            || f.schedule_revision > cursor.schedule_revision
            || (matches!(cursor.state, Schedule::Stopped { .. })
                && (f.schedule_revision.checked_add(1) != Some(cursor.schedule_revision)
                    || f.control_sequence.checked_add(1) != Some(cursor.control_sequence)))
        {
            return Err(StoreError::Corrupt);
        }
        let mut historical = operation.clone();
        historical.revision = f.before_revision;
        historical.stage = f.stage;
        historical.native_boundary = None;
        historical.native_boundary_evidence = None;
        historical.native_directory_boundary = None;
        let registrations = self.operation_registrations(operation)?;
        if !registrations.is_empty() {
            return Err(StoreError::Corrupt);
        }
        self.validate_regular_boundary(&historical, f.before_progress, f.fence, 0, None, false, &[])
    }
    fn reduce_equality_stop(
        &self,
        boundary: &format::InitialReviewStopBoundaryV1,
    ) -> Result<Vec<Row>, StoreError> {
        boundary.validate()?;
        let f = boundary.fact();
        let old = self.connection(ConnectionId(f.connection))?;
        let operation = self.operation(f.operation.operation()?)?;
        let mut cursor = self.review_cursor(f.connection, f.origin)?;
        let Some(Row::InitialReviewLink(link)) =
            self.row(&format::key(10, f.connection, f.operation.serial))?
        else {
            return Err(StoreError::Corrupt);
        };
        if cursor.current
            != (Current::Equality {
                step: f.step,
                entry: f.entry,
                operation: f.operation,
            })
            || !matches!(cursor.state, Schedule::Stopping { .. })
            || old.pending != Some(f.operation.serial)
            || operation.revision != f.before_revision
            || operation.stage != f.stage
            || cursor.progress_revision != f.before_progress
            || old.revision != f.before_progress
            || cursor.schedule_revision != f.schedule_revision
            || cursor.control_sequence != f.control_sequence
            || operation.action.is_some()
            || operation.decision != 0
            || operation.native_boundary.is_some()
        {
            return Err(StoreError::StaleRevision);
        }
        let view = self.read_review(&self.review_fact(f.connection, f.origin)?)?;
        // Validate the same saved cut against the prospective revision, before deriving it.
        let mut prospective = operation.clone();
        prospective.revision = f.resulting_revision;
        self.validate_saved_equality_stop(&view, &link, &cursor, &prospective, boundary)?;
        let mut rows = self.reduce_boundary_rows(
            f.operation.operation()?,
            f.before_progress,
            f.before_revision,
            f.fence,
            0,
            None,
            false,
        )?;
        for row in &mut rows {
            match row {
                Row::Connection(c) => {
                    if c.revision != f.resulting_revision {
                        return Err(StoreError::Corrupt);
                    }
                    c.pending = old.pending;
                }
                Row::Operation(op) => {
                    op.stage = operation.stage;
                    op.native_boundary = operation.native_boundary;
                    op.native_boundary_evidence = operation.native_boundary_evidence.clone();
                    op.native_directory_boundary = operation.native_directory_boundary.clone();
                }
                _ => return Err(StoreError::Corrupt),
            }
        }
        cursor.progress_revision = f.resulting_progress;
        rows.push(Row::InitialReviewCursor(cursor));
        rows.push(Row::InitialReviewStop(boundary.clone()));
        Ok(rows)
    }
}

impl format::InitialReviewControlV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1
            || self.origin == 0
            || self.expected_progress <= self.origin
            || self.expected_schedule == 0
            || self.control_sequence == 0
            || self
                .expected_schedule
                .checked_add(1)
                .is_none_or(|n| n == u64::MAX)
        {
            return Err(StoreError::Corrupt);
        }
        self.current.validate(self.connection)
    }
}
