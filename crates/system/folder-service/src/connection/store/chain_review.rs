//! Complete initial consent and inert reviewed history. Decoding grants no authority.
#[cfg(test)]
mod tests;

use super::*;
use format::{
    DirectoryChainScheduleState as Schedule, InitialReviewControlTransitionV1 as Control,
};
use format::{InitialReviewCurrentV1 as Current, InitialReviewLinkKindV1 as LinkKind};
use sha2::{Digest, Sha256};

// An inert replay lease never supplies native credit. Paid readers retain both
// the same store lease and the exact explicitly borrowed operation context.
pub(super) enum HistoryReadContext<'a> {
    Inert {
        lease: super::storage::ReadLease,
    },
    Paid {
        lease: super::storage::ReadLease,
        native: &'a NativeReadContext<'a>,
    },
}
impl HistoryReadContext<'_> {
    fn check(&self) -> Result<(), StoreError> {
        match self {
            Self::Inert { lease } => lease.check(),
            Self::Paid { lease, native } => {
                lease.check()?;
                native.check()
            }
        }
    }
}
impl ConnectionStore {
    pub(super) fn history_read_context<'a>(
        &self,
        native: Option<&'a NativeReadContext<'a>>,
    ) -> Result<HistoryReadContext<'a>, StoreError> {
        self.check_history_context(native)?;
        let lease = self.storage.read_lease(false)?;
        if let Some(native) = native {
            let operation = self.operation_with_context(native.id, Some(native))?;
            // Paid history inherits the existing durable operation payer. The
            // later endpoint identity field is not a prerequisite of that API.
            require(operation.native_budget.is_some())?;
        }
        let context = match native {
            Some(native) => HistoryReadContext::Paid { lease, native },
            None => HistoryReadContext::Inert { lease },
        };
        context.check()?;
        Ok(context)
    }
}

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
            Self::None {}
            | Self::BetweenMembers { .. }
            | Self::BetweenSteps { .. }
            | Self::ReviewComplete { .. }
            | Self::StepLimit { .. } => None,
            Self::Continuation { operation, .. }
            | Self::Chain { operation, .. }
            | Self::Equality { operation, .. } => Some(*operation),
            Self::AbandoningChain { cut, .. } => match cut {
                format::ReviewAbandonCut::Member { operation } => Some(*operation),
                format::ReviewAbandonCut::BetweenMembers { .. } => None,
            },
        }
    }
    fn validate(&self, connection: [u8; 16]) -> Result<(), StoreError> {
        match self {
            Self::None {} | Self::ReviewComplete { .. } | Self::StepLimit { .. } => Ok(()),
            Self::BetweenSteps { last_step, .. } => require(*last_step < 4096),
            Self::Continuation { operation, .. } => {
                operation.operation()?;
                require(operation.connection == connection)
            }
            Self::BetweenMembers {
                step,
                entry,
                anchor,
                next_index,
                predecessor,
                ..
            } => {
                anchor.operation()?;
                predecessor.operation()?;
                require(
                    *step < 4096
                        && *entry < 4096
                        && anchor.connection == connection
                        && predecessor.connection == connection
                        && anchor.serial <= predecessor.serial
                        && (1..64).contains(next_index),
                )
            }
            Self::AbandoningChain {
                step,
                entry,
                anchor,
                cut,
                ..
            } => {
                anchor.operation()?;
                require(*step < 4096 && *entry < 4096 && anchor.connection == connection)?;
                match cut {
                    format::ReviewAbandonCut::Member { operation } => {
                        operation.operation()?;
                        require(
                            operation.connection == connection && operation.serial >= anchor.serial,
                        )
                    }
                    format::ReviewAbandonCut::BetweenMembers {
                        next_index,
                        predecessor,
                    } => {
                        predecessor.operation()?;
                        require(
                            predecessor.connection == connection
                                && predecessor.serial >= anchor.serial
                                && (1..64).contains(next_index),
                        )
                    }
                }
            }
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
            || self.next_step > 4096
            || (self.next_step == 4096
                && !matches!(
                    self.current,
                    Current::ReviewComplete { .. }
                        | Current::Continuation { .. }
                        | Current::StepLimit { .. }
                ))
            || self.schedule_revision == 0
            || (matches!(self.state, Schedule::Abandoning)
                && !matches!(self.current, Current::AbandoningChain { .. }))
            || (matches!(self.current, Current::AbandoningChain { .. })
                && !matches!(self.state, Schedule::Abandoning | Schedule::Disconnected))
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
        self.require_no_review_or_chain_history_with_context(id, None)
    }
    pub(super) fn require_no_review_or_chain_history_with_context(
        &self,
        id: ConnectionId,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let connection = self.connection_with_context(id, context)?;
        if connection.directory_chain.is_some() || connection.directory_chain_schedule.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        for row in self.rows_with_context(context)? {
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
        Row::ReviewedBaselineResultV1(r) => Some(r.key.connection),
        Row::InitialReviewStepCompletedV1(r) => Some(r.key.connection),
        Row::InitialReviewProgressV1(r) => Some(r.key.connection),
        Row::InitialReviewCompletedV1(r) => Some(r.key.connection),
        Row::PostReviewOperationV1(r) => Some(r.key.connection),
        Row::ChainCompletionBaselineV1(r) => Some(r.core.key.connection),
        Row::ChainBaselineLineageV1(r) => Some(r.key.connection),
        Row::ChainBaselineSupersessionV1(r) => Some(r.key.connection),
        Row::ReviewedCandidateRootsV1(r) => Some(r.connection),
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
    pub(super) fn read_review_with_context(
        &self,
        fact: &format::InitialChainReviewV1,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<ReviewView, StoreError> {
        let _read_lease = self.history_read_context(context)?;
        if let Some(context) = context {
            context.check()?;
        }
        fact.validate()?;
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut previous = None;
        let mut trees: [Vec<(RelativePath, EntryState)>; 2] = [Vec::new(), Vec::new()];
        let mut hash = Sha256::new();
        hash.update(b"Connected explicit initial preview v1");
        hash.update(&fact.original);
        for row in storage::TreeIter::new_context(
            &self.storage,
            Some(fact.root),
            Some(fact.count),
            context,
        )? {
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
    fn review_cursor_with_context(
        &self,
        connection: [u8; 16],
        origin: u64,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<format::InitialReviewCursorV1, StoreError> {
        match self.row_with_context(&format::key(9, connection, origin), context)? {
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
    pub(super) fn validate_review_history_with_context(
        &self,
        replacements: &[Row],
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let _read_lease = self.history_read_context(context)?;
        if let Some(context) = context {
            context.check()?;
        }
        let mut rows = BTreeMap::new();
        for row in self.rows_with_context(context)? {
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
        let mut durable_owners = std::collections::BTreeSet::new();
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
                if matches!(
                    rows.get(&format::key(14, id, review.original_revision)),
                    Some(Row::InitialReviewProgressV1(_))
                ) {
                    durable_owners.insert(id);
                    self.validate_durable_review(review, &rows, context)?;
                    continue;
                }
                let view = self.read_review_with_context(review, context)?;
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
                        if !current_matches_link(current, link)
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
                            if (c.pending != Some(id.serial)
                                && !(cursor.state == Schedule::Abandoning
                                    && op.stage == 7
                                    && c.pending.is_none()))
                                || op.revision != cursor.progress_revision
                            {
                                return Err(StoreError::Corrupt);
                            }
                        } else if op.revision < cursor.progress_revision {
                            return Err(StoreError::Corrupt);
                        }
                        match current {
                            Current::Chain { anchor, digest, .. }
                            | Current::AbandoningChain { anchor, digest, .. } => {
                                validate_current_chain_control(
                                    c,
                                    cursor,
                                    *anchor,
                                    *digest,
                                    Some(id),
                                )?;
                            }
                            Current::Equality { .. } => self.validate_current_equality(
                                &view, link, cursor, op, c, &rows, context,
                            )?,
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
            if durable_owners.contains(&id) {
                if matches!(
                    row,
                    Row::InitialChainReviewEntry(_) | Row::ReviewedCandidateRootsV1(_)
                ) {
                    return Err(StoreError::Corrupt);
                }
                continue;
            }
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
                    if !current_matches_link(&cursor.current, link) {
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
                    && !matches!(
                        rows.get(&format::key(16, op.connection, op.serial)),
                        Some(Row::PostReviewOperationV1(_))
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
    pub(in crate::connection) fn review_read_lease(&self) -> Result<ReviewReadLease, StoreError> {
        self.storage.read_lease(true)
    }
    pub(in crate::connection) fn initial_review_preimage(
        &self,
        id: ConnectionId,
    ) -> Result<Vec<u8>, StoreError> {
        format::encode(&self.connection(id)?)
    }
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
        Change::ReviewedBaselineCommit { body, .. } => Some(body.canonical()),
        Change::ReviewedChainRetirement { inner, .. } => Some(inner.canonical()),
        Change::ReviewedContinuationIntent { operation, .. }
        | Change::ReviewedIntent { operation, .. } => Some(Change::Intent {
            operation: operation.clone(),
        }),
        Change::ReviewedChainControl { inner, .. } => Some(inner.canonical()),
        _ => None,
    }
}
impl ConnectionStore {
    fn owned_review_with_context(
        &self,
        id: [u8; 16],
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<Option<format::InitialChainReviewV1>, StoreError> {
        let mut found = None;
        for row in self.rows_with_context(context)? {
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
    pub(super) fn review_event_guard(
        &self,
        change: &Change,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        if matches!(
            change,
            Change::Create { .. } | Change::InitialReviewAccepted { .. }
        ) {
            return Ok(());
        }
        let id = super::chain::change_connection(change)?;
        let Some(review) = self.owned_review_with_context(id, context)? else {
            return if matches!(
                change,
                Change::ReviewedIntent { .. }
                    | Change::ReviewedChainControl { .. }
                    | Change::InitialReviewControl { .. }
                    | Change::ReviewedOrdinaryStopBoundary { .. }
                    | Change::ReviewedBaselineCommit { .. }
                    | Change::ReviewedContinuationIntent { .. }
                    | Change::ReviewedChainRetirement { .. }
            ) {
                Err(StoreError::RecoveryRequired)
            } else {
                Ok(())
            };
        };
        let cursor = self.review_cursor_with_context(id, review.original_revision, context)?;
        let running = cursor.state == Schedule::Running;
        let current = cursor.current.operation();
        let serial = match change {
            Change::Fence { .. }
            | Change::Disconnect { .. }
            | Change::InitialReviewControl { .. }
            | Change::ReviewedChainControl { .. } => return Ok(()),
            Change::ReviewedIntent { .. }
                if running
                    && matches!(
                        cursor.current,
                        Current::None {}
                            | Current::BetweenSteps { .. }
                            | Current::BetweenMembers { .. }
                    ) =>
            {
                return Ok(());
            }
            Change::ReviewedChainRetirement { inner, .. }
                if matches!(
                    inner.transition,
                    format::DirectoryChainControlTransitionV1::AbandonRequested
                        | format::DirectoryChainControlTransitionV1::Retire
                ) =>
            {
                return Ok(());
            }
            Change::ReviewedContinuationIntent { .. }
                if running && matches!(cursor.current, Current::ReviewComplete { .. }) =>
            {
                return Ok(());
            }
            Change::ReviewedOrdinaryStopBoundary { .. }
                if matches!(cursor.state, Schedule::Stopping { .. }) =>
            {
                return Ok(());
            }
            Change::ReviewedBaselineCommit { provenance, .. } if running => match provenance {
                format::ReviewedBaselineProvenanceV1::ReviewedStep { operation, .. }
                | format::ReviewedBaselineProvenanceV1::Continuation { operation, .. } => {
                    operation.serial
                }
            },
            Change::Reserve { entry, .. } | Change::Bytes { entry, .. }
                if running && matches!(cursor.current, Current::Continuation { .. }) =>
            {
                entry.serial
            }
            Change::NativeBound { fact, .. }
                if running && matches!(cursor.current, Current::Continuation { .. }) =>
            {
                fact.serial
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
                if matches!(cursor.state, Schedule::Disconnected | Schedule::Abandoning) =>
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
            (
                Current::Chain { anchor, digest, .. }
                | Current::BetweenMembers { anchor, digest, .. },
                Some(inner),
            ) => {
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
                // The canonical component already checked the distinct inner tuple.
            }
            (Current::Chain { .. } | Current::BetweenMembers { .. }, None) | (_, Some(_)) => {
                return Err(StoreError::Corrupt);
            }
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
                    Current::None {}
                    | Current::BetweenSteps { .. }
                    | Current::ReviewComplete { .. }
                    | Current::StepLimit { .. }
                        if c.pending.is_none() =>
                    {
                        ()
                    }
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
                    Current::Chain { .. } | Current::BetweenMembers { .. } if inner.is_some() => (),
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
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        if let Change::ReviewedChainRetirement {
            key,
            before,
            inner,
            baseline,
            lineage,
        } = change
        {
            return self
                .complete_review_retirement(key, before, inner, baseline, lineage, rows, context);
        }
        if let Change::ReviewedBaselineCommit { body, provenance } = change {
            return self.complete_review_baseline(body, provenance, rows, context);
        }
        if let Change::ReviewedContinuationIntent {
            operation,
            provenance,
            expected_progress,
            expected_schedule,
            control_sequence,
        } = change
        {
            return self.complete_continuation_intent(
                operation,
                provenance,
                *expected_progress,
                *expected_schedule,
                *control_sequence,
                rows,
                context,
            );
        }
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
        let Some(review) = self.owned_review_with_context(id, context)? else {
            return Ok(());
        };
        let mut cursor = self.review_cursor_with_context(id, review.original_revision, context)?;
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
            let old = self.review_cursor_with_context(id, review.original_revision, context)?;
            if !matches!(
                old.current,
                Current::None {} | Current::BetweenSteps { .. } | Current::BetweenMembers { .. }
            ) || old.state != Schedule::Running
                || old.progress_revision != *expected_progress
                || old.schedule_revision != *expected_schedule
                || old.control_sequence != *control_sequence
                || old.review != link.review
                || link.step != old.next_step
            {
                return Err(StoreError::StaleRevision);
            }
            let view = self.read_review_with_context(&review, context)?;
            let all = self.prospective_review_rows(rows, context)?;
            let key = review_key(&review)?;
            let results = self.review_result_index(&key, &all, context)?;
            let cut_index = self.review_cut_index(&key, &all, &results, context)?;
            self.validate_link_cut(&view, link, operation, &all, &cut_index, context)?;
            require(format::encode(&original_intent(operation)?)? == format::encode(operation)?)?;
            if let Current::BetweenMembers {
                step,
                entry,
                anchor,
                digest,
                next_index,
                predecessor,
            } = old.current
            {
                let member = operation
                    .directory_chain_ref
                    .as_ref()
                    .ok_or(StoreError::Corrupt)?;
                require(
                    link.step == step
                        && link.entry == entry
                        && member.anchor == anchor
                        && member.digest == digest
                        && member.index == next_index
                        && member.predecessor == Some(predecessor),
                )?;
            }
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
                let mut selected = self.selection(link.entry, *review_revision)?;
                let retained = super::chain::first_selection(anchor)?;
                if retained.baseline.is_some() {
                    selected.baseline = retained.baseline;
                    selected.baselined_prefixes = retained.baselined_prefixes;
                    selected.existing_prefixes &= !retained.baselined_prefixes;
                    selected.missing_prefixes &= !retained.baselined_prefixes;
                    selected = selected.checked()?;
                }
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
                    || op.expected < format::next(self.fact.original_revision)?
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
    fn validate_current_equality(
        &self,
        view: &ReviewView,
        link: &format::InitialReviewLinkV1,
        cursor: &format::InitialReviewCursorV1,
        op: &format::Operation,
        connection: &format::Connection,
        rows: &BTreeMap<Vec<u8>, Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let id = link.operation;
        self.storage.review_charge(0, 0, 0, 1)?;
        let prefix = format::key(5, id.connection, id.serial);
        let registered = rows
            .range(prefix.clone()..)
            .next()
            .is_some_and(|(key, _)| key.starts_with(&prefix));
        if connection.directory_chain.is_some()
            || connection.directory_chain_schedule.is_some()
            || !matches!(op.stage, 0 | 3 | 7)
            || (op.stage == 7
                && (cursor.state != Schedule::Disconnected
                    || connection.pending.is_some()
                    || op.native_boundary != Some(0)))
            || (op.stage != 7 && connection.pending != Some(id.serial))
            || registered
        {
            return Err(StoreError::Corrupt);
        }
        if let Some(Row::InitialReviewStop(boundary)) =
            rows.get(&format::key(11, id.connection, id.serial))
        {
            self.validate_saved_equality_stop(view, link, cursor, op, boundary, context)?;
        } else if matches!(cursor.state, Schedule::Stopped { .. }) {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
    fn validate_saved_equality_stop(
        &self,
        view: &ReviewView,
        link: &format::InitialReviewLinkV1,
        cursor: &format::InitialReviewCursorV1,
        operation: &format::Operation,
        boundary: &format::InitialReviewStopBoundaryV1,
        context: Option<&NativeReadContext<'_>>,
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
        let registrations = self.operation_registrations_with_context(operation, context)?;
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
        self.validate_saved_equality_stop(&view, &link, &cursor, &prospective, boundary, None)?;
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

// The new facts are inert evidence. Local shape validation is separate from the
// prospective owner, receipt, baseline and lineage closure below.
use format::{BaselineVersion, LineageRef, PageRef, ReviewKey};
fn require(value: bool) -> Result<(), StoreError> {
    if value {
        Ok(())
    } else {
        Err(StoreError::Corrupt)
    }
}
fn fact_shape<T: serde::Serialize>(
    value: &T,
    version: u8,
    key: &ReviewKey,
) -> Result<(), StoreError> {
    require(version == 1 && key.origin > 0)?;
    format::encode(value)?;
    Ok(())
}
fn reference_operation(id: &format::ChainOperationIdV1, key: &ReviewKey) -> Result<(), StoreError> {
    id.operation()?;
    require(id.connection == key.connection)
}
impl PageRef {
    pub(super) fn root(&self) -> Option<[u8; 32]> {
        match self {
            Self::Empty {} => None,
            Self::Page { hash } => Some(*hash),
        }
    }
    pub(super) fn from_root(root: Option<[u8; 32]>) -> Self {
        root.map_or(Self::Empty {}, |hash| Self::Page { hash })
    }
}
impl BaselineVersion {
    pub(super) fn from_connection(c: &format::Connection) -> Result<Self, StoreError> {
        let version = match c.baseline {
            None => {
                require(c.baseline_root.is_none() && c.baseline_count == 0)?;
                Self::Absent {}
            }
            Some(revision) => Self::Present {
                revision,
                root: PageRef::from_root(c.baseline_root),
                count: c.baseline_count,
            },
        };
        version.validate()?;
        Ok(version)
    }
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        match self {
            Self::Absent {} => Ok(()),
            Self::Present {
                revision,
                root,
                count,
            } => require(*revision > 0 && *count <= 4096 && root.root().is_none() == (*count == 0)),
        }
    }
    pub(super) fn revision(&self) -> Option<u64> {
        match self {
            Self::Absent {} => None,
            Self::Present { revision, .. } => Some(*revision),
        }
    }
    pub(super) fn root(&self) -> Option<[u8; 32]> {
        match self {
            Self::Absent {} => None,
            Self::Present { root, .. } => root.root(),
        }
    }
    pub(super) fn count(&self) -> u32 {
        match self {
            Self::Absent {} => 0,
            Self::Present { count, .. } => *count,
        }
    }
}
impl LineageRef {
    fn validate(&self) -> Result<(), StoreError> {
        if let Self::Present { index, version, .. } = self {
            require(*index <= 65_536)?;
            version.validate()?;
        }
        Ok(())
    }
}
impl format::ReviewCut {
    fn validate(&self) -> Result<(), StoreError> {
        require(
            self.progress_revision > 0
                && self.schedule_revision > 0
                && self.schedule_revision < u64::MAX
                && self.control_sequence.checked_add(1) == Some(self.schedule_revision),
        )
    }
    fn from_cursor(c: &format::InitialReviewCursorV1) -> Result<Self, StoreError> {
        Ok(Self {
            progress_revision: c.progress_revision,
            schedule_revision: c.schedule_revision,
            control_sequence: c.control_sequence,
            current_digest: format::digest(&format::encode(&c.current)?),
        })
    }
}
impl format::CommitWitness {
    fn validate(&self, key: &ReviewKey) -> Result<(), StoreError> {
        reference_operation(&self.operation, key)?;
        self.before.validate()?;
        self.after.validate()?;
        require(
            self.precommit_revision > key.origin
                && self.precommit_revision.checked_add(1) == Some(self.result_revision)
                && self.after.revision() == Some(self.result_revision)
                && self
                    .before
                    .revision()
                    .is_none_or(|r| r < self.precommit_revision),
        )?;
        self.budget.validate()?;
        match self.receipt {
            format::CommitReceiptV1::EqualityVerified {} => (),
            format::CommitReceiptV1::Regular { attempt, .. }
            | format::CommitReceiptV1::Directory { attempt, .. } => {
                require((1..=8).contains(&attempt))?
            }
        }
        match &self.existing_ancestors {
            format::ReviewPatch::Absent {} => (),
            format::ReviewPatch::Present { value } => require(
                value.version == 1
                    && value.admission == self.admission_digest
                    && value.prefixes != 0
                    && value.prefixes & 1 == 0,
            )?,
        }
        match &self.directory_ancestors {
            format::ReviewPatch::Absent {} => (),
            format::ReviewPatch::Present { value } => require(
                value.version == 1
                    && value.admission == self.admission_digest
                    && value.prefixes != 0
                    && value.prefixes & 1 == 0,
            )?,
        }
        require(
            matches!(self.existing_ancestors, format::ReviewPatch::Absent {})
                || matches!(self.directory_ancestors, format::ReviewPatch::Absent {}),
        )
    }
}
impl format::ReviewedBaselineResultV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        self.witness.validate(&self.key)?;
        self.cut.validate()?;
        self.lineage.validate()?;
        require(self.cut.progress_revision == self.witness.precommit_revision)?;
        if let format::ReviewedResultProvenanceV1::ReviewStep {
            step,
            entry,
            selected,
            ..
        } = &self.provenance
        {
            require(*step < 4096 && *entry < 4096)?;
            if let format::ReviewedSelectionV1::Chain { anchor, index, .. } = selected {
                reference_operation(anchor, &self.key)?;
                require(*index < 64 && anchor.serial <= self.witness.operation.serial)?;
            }
        }
        Ok(())
    }
}
impl format::InitialReviewStepCompletedV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        reference_operation(&self.first_operation, &self.key)?;
        require(
            self.step < 4096
                && self.entry < 4096
                && self.resulting_progress > self.key.origin
                && self.control_sequence.checked_add(1) == Some(self.schedule_revision),
        )?;
        match &self.terminal {
            format::ReviewTerminalV1::Completed {
                operation, archive, ..
            } => {
                reference_operation(operation, &self.key)?;
                if let format::ReviewArchiveRefV1::Present { anchor, .. } = archive {
                    reference_operation(anchor, &self.key)?;
                }
            }
            format::ReviewTerminalV1::Abandoned { anchor, .. } => {
                reference_operation(anchor, &self.key)?
            }
        }
        Ok(())
    }
}
impl format::ReviewRemainingV1 {
    fn total(&self) -> Result<u32, StoreError> {
        [
            self.unselected,
            self.structural,
            self.conflict,
            self.abandoned,
        ]
        .into_iter()
        .try_fold(0u32, |n, v| {
            require(v <= 4096)?;
            n.checked_add(v).ok_or(StoreError::Corrupt)
        })
    }
}
impl format::InitialReviewProgressV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        require(
            self.completed_steps <= 4096
                && self.covered <= 4096
                && self
                    .covered
                    .checked_add(self.remaining.total()?)
                    .is_some_and(|n| n <= 4096)
                && self.progress_revision > self.key.origin,
        )?;
        require(match self.last_step {
            format::ReviewLastStepV1::None {} => self.completed_steps == 0,
            format::ReviewLastStepV1::Step { step, .. } => {
                step.checked_add(1) == Some(self.completed_steps)
            }
        })
    }
}
impl format::InitialReviewCompletedV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        self.baseline.validate()?;
        self.lineage.validate()?;
        require(
            (1..=4096).contains(&self.completed_steps)
                && (1..=4096).contains(&self.entries)
                && self
                    .baseline
                    .revision()
                    .is_some_and(|r| r <= self.progress_revision)
                && self.control_sequence.checked_add(1) == Some(self.schedule_revision),
        )
    }
}
impl format::PostReviewOperationV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        reference_operation(&self.operation, &self.key)?;
        self.baseline.validate()?;
        self.lineage.validate()?;
        require(
            self.expected > self.key.origin
                && self.baseline.revision().is_some_and(|r| r <= self.expected),
        )
    }
}
impl format::ArchiveCore {
    fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        reference_operation(&self.anchor, &self.key)?;
        self.baseline.validate()?;
        match &self.created {
            format::ArchiveCreatedV1::Baseline { operation, .. } => {
                reference_operation(operation, &self.key)?
            }
            format::ArchiveCreatedV1::Retirement {
                before,
                inner,
                result,
            } => {
                before.validate()?;
                reference_operation(&inner.anchor, &self.key)?;
                require(
                    inner.connection == self.key.connection
                        && inner.anchor == self.anchor
                        && inner.digest == self.digest
                        && inner.transition == format::DirectoryChainControlTransitionV1::Retire
                        && inner.expected_progress == before.progress_revision
                        && inner.expected_schedule > 0
                        && inner.control_sequence == inner.expected_schedule
                        && result.progress_revision == before.progress_revision
                        && before.schedule_revision.checked_add(1)
                            == Some(result.schedule_revision)
                        && before.control_sequence.checked_add(1) == Some(result.control_sequence)
                        && self.retirement.reason == format::ChainRetirementReason::Abandoned
                        && self.retirement.cursor.progress_revision == before.progress_revision,
                )?;
            }
        }
        Ok(())
    }
}
impl format::ChainCompletionBaselineV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        self.core.validate()?;
        self.lineage.validate()?;
        require(
            (1..=4096).contains(&self.ordinal) && !matches!(self.lineage, LineageRef::Absent {}),
        )?;
        format::encode(self)?;
        Ok(())
    }
}
impl format::ChainBaselineLineageV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        self.origin_version.validate()?;
        self.current_version.validate()?;
        reference_operation(&self.origin_archive.anchor, &self.key)?;
        require(self.head_index <= 65_536 && (1..=4096).contains(&self.archives))
    }
    fn reference(&self) -> LineageRef {
        LineageRef::Present {
            origin: self.origin,
            index: self.head_index,
            head: self.head_digest,
            version: self.current_version.clone(),
        }
    }
}
impl format::ChainBaselineSupersessionV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        fact_shape(self, self.version, &self.key)?;
        reference_operation(&self.operation, &self.key)?;
        self.previous.validate()?;
        self.result.validate()?;
        require(
            (1..=65_536).contains(&self.index)
                && (1..=4096).contains(&self.archives_before)
                && self.archives_after >= self.archives_before
                && self.archives_after <= 4096,
        )?;
        let LineageRef::Present { index, version, .. } = &self.previous else {
            return Err(StoreError::Corrupt);
        };
        require(
            index.checked_add(1) == Some(self.index)
                && self
                    .result
                    .revision()
                    .is_some_and(|r| version.revision().is_none_or(|old| old < r)),
        )
    }
}
impl format::ReviewedCandidateRootsV1 {
    pub(super) fn validate(&self) -> Result<(), StoreError> {
        require(self.version == 1 && self.expected > 0 && self.review.root().is_some())?;
        self.before.validate()?;
        self.after.validate()?;
        require(self.after.revision() == self.expected.checked_add(1))?;
        format::encode(self)?;
        Ok(())
    }
}
impl format::ReviewedBaselineBodyV1 {
    pub(super) fn canonical(&self) -> Change {
        Change::BaselineCommit {
            directory_chain: self.directory_chain.clone(),
            directory_ancestors: self.directory_ancestors.clone(),
            existing_ancestors: self.existing_ancestors.clone(),
            connection: self.connection,
            expected: self.expected,
            root: self.root,
            count: self.count,
            source: self.source.clone(),
            destination: self.destination.clone(),
            initial_review: self.initial_review,
        }
    }
}

pub(super) struct FirstReviewedEquality {
    operation: format::ChainOperationIdV1,
    revision: u64,
}
impl FirstReviewedEquality {
    pub(super) fn permits(
        &self,
        connection: &format::Connection,
        operation: &format::Operation,
    ) -> bool {
        connection.id == self.operation.connection
            && connection.pending == Some(self.operation.serial)
            && connection.revision == self.revision
            && connection.baseline.is_none()
            && operation.connection == self.operation.connection
            && operation.serial == self.operation.serial
            && operation.revision == self.revision
            && operation.stage == 3
    }
}
fn tagged_digest<T: serde::Serialize>(domain: &[u8], value: &T) -> Result<[u8; 32], StoreError> {
    let mut h = Sha256::new();
    h.update(domain);
    h.update(format::encode(value)?);
    Ok(h.finalize().into())
}
fn result_digest(r: &format::ReviewedBaselineResultV1) -> Result<[u8; 32], StoreError> {
    tagged_digest(
        b"Connected reviewed baseline result v1",
        &Row::ReviewedBaselineResultV1(r.clone()),
    )
}
fn terminal_digest(r: &format::InitialReviewStepCompletedV1) -> Result<[u8; 32], StoreError> {
    tagged_digest(
        b"Connected review step v1",
        &Row::InitialReviewStepCompletedV1(r.clone()),
    )
}
fn completion_digest(r: &format::InitialReviewCompletedV1) -> Result<[u8; 32], StoreError> {
    tagged_digest(
        b"Connected review completion v1",
        &Row::InitialReviewCompletedV1(r.clone()),
    )
}
fn continuation_digest(r: &format::PostReviewOperationV1) -> Result<[u8; 32], StoreError> {
    tagged_digest(
        b"Connected post-review operation v1",
        &Row::PostReviewOperationV1(r.clone()),
    )
}
fn original_intent(op: &format::Operation) -> Result<format::Operation, StoreError> {
    let mut value = op.clone();
    value.revision = format::next(value.expected)?;
    value.stage = 0;
    value.native_budget = Some(format::NativeBudget::initial());
    value.recovery.clear();
    value.native_capture_displaced.clear();
    value.directory_chain_retired = None;
    value.directory_chain_stop_boundary = None;
    value.native_directory_boundary = None;
    value.selected_applied = None;
    value.native_capture = None;
    value.native_boundary_evidence = None;
    value.native_boundary = None;
    value.initial_review = None;
    Ok(value)
}
fn original_intent_digest(op: &format::Operation) -> Result<[u8; 32], StoreError> {
    if op.decision == 0 {
        original_equality_intent_digest(op)
    } else {
        Ok(format::digest(&format::encode(&original_intent(op)?)?))
    }
}
fn review_key(review: &format::InitialChainReviewV1) -> Result<ReviewKey, StoreError> {
    Ok(ReviewKey {
        connection: review.connection,
        review: review.identity()?,
        origin: review.original_revision,
    })
}
fn row_operation(
    rows: &BTreeMap<Vec<u8>, Row>,
    id: format::ChainOperationIdV1,
) -> Result<&format::Operation, StoreError> {
    match rows.get(&format::key(1, id.connection, id.serial)) {
        Some(Row::Operation(op)) => Ok(op),
        _ => Err(StoreError::Corrupt),
    }
}
fn row_link(
    rows: &BTreeMap<Vec<u8>, Row>,
    id: format::ChainOperationIdV1,
) -> Result<&format::InitialReviewLinkV1, StoreError> {
    match rows.get(&format::key(10, id.connection, id.serial)) {
        Some(Row::InitialReviewLink(link)) => Ok(link),
        _ => Err(StoreError::Corrupt),
    }
}
impl ConnectionStore {
    fn prospective_review_rows(
        &self,
        replacements: &[Row],
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<BTreeMap<Vec<u8>, Row>, StoreError> {
        let mut rows = BTreeMap::new();
        for row in self.rows_with_context(context)? {
            let row = row?;
            require(rows.insert(row.key(), row).is_none())?;
        }
        let mut keys = std::collections::BTreeSet::new();
        for row in replacements {
            require(keys.insert(row.key()))?;
            row.validate()?;
            rows.insert(row.key(), row.clone());
        }
        Ok(rows)
    }
    pub(super) fn reviewed_baseline_input(
        &self,
        change: &Change,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<Option<FirstReviewedEquality>, StoreError> {
        let Change::ReviewedBaselineCommit { body, provenance } = change else {
            return Ok(None);
        };
        let review = self
            .owned_review_with_context(body.connection, context)?
            .ok_or(StoreError::RecoveryRequired)?;
        let cursor =
            self.review_cursor_with_context(body.connection, review.original_revision, context)?;
        let connection = self.connection_with_context(ConnectionId(body.connection), context)?;
        let (id, progress, schedule, sequence, lineage) = match provenance {
            format::ReviewedBaselineProvenanceV1::ReviewedStep {
                operation,
                expected_progress,
                expected_schedule,
                control_sequence,
                lineage,
                ..
            }
            | format::ReviewedBaselineProvenanceV1::Continuation {
                operation,
                expected_progress,
                expected_schedule,
                control_sequence,
                lineage,
                ..
            } => (
                *operation,
                *expected_progress,
                *expected_schedule,
                *control_sequence,
                lineage,
            ),
        };
        require(
            cursor.state == Schedule::Running
                && cursor.current.operation() == Some(id)
                && connection.pending == Some(id.serial)
                && id.connection == body.connection
                && connection.revision == body.expected
                && body.expected == progress
                && cursor.progress_revision == progress
                && cursor.schedule_revision == schedule
                && cursor.control_sequence == sequence
                && !body.initial_review
                && connection.fence == review.fence
                && connection.lifecycle == 0,
        )?;
        let rows = self.prospective_review_rows(&[], context)?;
        let actual_lineage =
            match rows.get(&format::key(18, body.connection, review.original_revision)) {
                Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
                None => LineageRef::Absent {},
                _ => return Err(StoreError::Corrupt),
            };
        require(*lineage == actual_lineage)?;
        let op = row_operation(&rows, id)?;
        require(op.stage == 3 && op.revision == progress)?;
        match provenance {
            format::ReviewedBaselineProvenanceV1::ReviewedStep {
                link_digest,
                step,
                entry,
                ..
            } => {
                let link = row_link(&rows, id)?;
                require(
                    link.current() == cursor.current
                        && link.step == *step
                        && link.entry == *entry
                        && *step == cursor.next_step
                        && *link_digest == format::digest(&format::encode(link)?),
                )?;
                let view = self.read_review_with_context(&review, context)?;
                let anchor = match link.kind {
                    LinkKind::Chain { anchor, .. } => Some(row_operation(&rows, anchor)?),
                    _ => None,
                };
                view.validate_link(link, op, anchor)?;
                if matches!(link.kind, LinkKind::Equality {}) && connection.baseline.is_none() {
                    require(body.directory_chain.is_none() && body.directory_ancestors.is_none())?;
                    return Ok(Some(FirstReviewedEquality {
                        operation: id,
                        revision: progress,
                    }));
                }
            }
            format::ReviewedBaselineProvenanceV1::Continuation {
                provenance_digest, ..
            } => {
                let Some(Row::PostReviewOperationV1(link)) =
                    rows.get(&format::key(16, id.connection, id.serial))
                else {
                    return Err(StoreError::Corrupt);
                };
                require(
                    *provenance_digest == continuation_digest(link)?
                        && cursor.current
                            == (Current::Continuation {
                                operation: id,
                                provenance_digest: *provenance_digest,
                            }),
                )?;
            }
        }
        Ok(None)
    }
    fn commit_receipt(
        &self,
        op: &format::Operation,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<format::CommitReceiptV1, StoreError> {
        if op.decision == 0 {
            return Ok(format::CommitReceiptV1::EqualityVerified {});
        }
        let mut latest = None;
        for row in self.rows_with_context(context)? {
            if let Row::NativeRegistration(reg) = row? {
                if reg.connection == op.connection
                    && reg.serial == op.serial
                    && latest
                        .as_ref()
                        .is_none_or(|old: &format::NativeRegistration| old.attempt < reg.attempt)
                {
                    latest = Some(reg);
                }
            }
        }
        self.native_baseline_value_with_context(op, context)?;
        let registration = latest.ok_or(StoreError::Corrupt)?;
        if op.native_kind.is_none() {
            self.reviewed_regular_receipt_join(op, &registration, context)?;
        }
        let outcome = format::digest(registration.outcome.as_deref().ok_or(StoreError::Corrupt)?);
        Ok(
            if op.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
                format::CommitReceiptV1::Directory {
                    attempt: registration.attempt.into(),
                    outcome,
                }
            } else {
                format::CommitReceiptV1::Regular {
                    attempt: registration.attempt.into(),
                    outcome,
                }
            },
        )
    }
    fn reviewed_regular_receipt_join(
        &self,
        op: &format::Operation,
        registration: &format::NativeRegistration,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        use super::super::transaction::native::descriptor as d;
        registration.validate()?;
        let admission =
            d::Admission::decode(op.native_identity.as_deref().ok_or(StoreError::Corrupt)?)
                .map_err(|_| StoreError::Corrupt)?;
        let intent = self.regular_registration_join(op, registration, context)?;
        let d::Body::Intent(before) = &intent.body else {
            return Err(StoreError::Corrupt);
        };
        require(
            registration.connection == op.connection
                && registration.serial == op.serial
                && registration.admission == format::next(op.expected)?
                && registration.baseline == op.baseline
                && registration.fence == admission.fence
                && before.endpoints == admission.endpoints
                && before.direction == admission.direction
                && before.decision == admission.decision
                && before.path == admission.path
                && before.chains == admission.chains
                && before.expected == admission.states
                && before.selection == d::Selection::None,
        )?;
        let outcome = d::decode(registration.outcome.as_deref().ok_or(StoreError::Corrupt)?)
            .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Outcome(body) = &outcome.body else {
            return Err(StoreError::Corrupt);
        };
        require(
            body.sequence == 1
                && body.kind <= 2
                && outcome.header.progress >= registration.admission
                && outcome.header.progress < op.revision
                && (body.kind == 2
                    || body.states.get(usize::from(before.direction))
                        == before.expected.get(usize::from(before.direction))),
        )?;
        let mut accounted = registration.outcome_displaced.is_none();
        let mut seen = std::collections::BTreeSet::new();
        for reference in &body.recovery {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            require(seen.insert(reference.slot) && op.recovery.contains(&reference.slot))?;
            let mut key = format::key(6, op.connection, op.serial);
            key.extend(reference.slot.to_be_bytes());
            let Some(Row::NativeRecoveryFact(bound)) = self.row_with_context(&key, context)? else {
                return Err(StoreError::Corrupt);
            };
            let id = super::RecoveryId {
                operation: super::OperationId {
                    connection: ConnectionId(op.connection),
                    serial: op.serial,
                },
                slot: reference.slot,
            };
            let Some(Row::Recovery(entry)) =
                self.row_with_context(&super::recovery::recovery_key(id), context)?
            else {
                return Err(StoreError::Corrupt);
            };
            let (_, displaced, fact) =
                self.native_recovery_binding(&entry, registration, &bound, op.stage == 4)?;
            require(
                bound.attempt == registration.attempt
                    && fact.intent == intent.digest
                    && fact.side == reference.side
                    && fact.role == reference.role
                    && fact.binding_digest().map_err(|_| StoreError::Corrupt)? == reference.binding
                    && entry.path == op.path
                    && entry
                        .object
                        .as_ref()
                        .is_some_and(|o| o.root == self.storage.identity),
            )?;
            if fact.role == 0
                && fact.object == displaced.object
                && fact.side == displaced.side
                && registration.outcome_displaced.as_ref() == Some(&bound.displaced)
            {
                accounted = true;
            }
        }
        require(accounted)
    }
    fn complete_review_baseline(
        &self,
        body: &format::ReviewedBaselineBodyV1,
        provenance: &format::ReviewedBaselineProvenanceV1,
        rows: &mut Vec<Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let review = self
            .owned_review_with_context(body.connection, context)?
            .ok_or(StoreError::Corrupt)?;
        let key = review_key(&review)?;
        let mut cursor = self.review_cursor_with_context(body.connection, key.origin, context)?;
        let before = self.connection_with_context(ConnectionId(body.connection), context)?;
        let id = cursor.current.operation().ok_or(StoreError::Corrupt)?;
        let op = self.operation_with_context(id.operation()?, context)?;
        let cut = format::ReviewCut::from_cursor(&cursor)?;
        let connection = rows
            .iter()
            .find_map(|row| match row {
                Row::Connection(c) if c.id == body.connection => Some(c.clone()),
                _ => None,
            })
            .ok_or(StoreError::Corrupt)?;
        let witness = format::CommitWitness {
            operation: id,
            precommit_revision: op.revision,
            result_revision: connection.revision,
            intent_digest: original_intent_digest(&op)?,
            admission_digest: format::digest(
                op.native_identity.as_deref().ok_or(StoreError::Corrupt)?,
            ),
            budget: op.native_budget.clone().ok_or(StoreError::Corrupt)?,
            receipt: self.commit_receipt(&op, context)?,
            before: BaselineVersion::from_connection(&before)?,
            after: BaselineVersion::from_connection(&connection)?,
            existing_ancestors: body
                .existing_ancestors
                .clone()
                .map_or(format::ReviewPatch::Absent {}, |value| {
                    format::ReviewPatch::Present { value }
                }),
            directory_ancestors: body
                .directory_ancestors
                .clone()
                .map_or(format::ReviewPatch::Absent {}, |value| {
                    format::ReviewPatch::Present { value }
                }),
        };
        let prior = self.prospective_review_rows(&[], context)?;
        let (result_provenance, step_link, final_member) = match provenance {
            format::ReviewedBaselineProvenanceV1::ReviewedStep { .. } => {
                let link = row_link(&prior, id)?.clone();
                let (selected, final_member) = match &link.kind {
                    LinkKind::Equality {} => (format::ReviewedSelectionV1::Equality {}, true),
                    LinkKind::Chain { anchor, digest, .. } => {
                        let member = op.directory_chain_ref.as_ref().ok_or(StoreError::Corrupt)?;
                        let final_member = connection.directory_chain.is_none();
                        (
                            format::ReviewedSelectionV1::Chain {
                                anchor: *anchor,
                                digest: *digest,
                                index: member.index,
                                final_member,
                            },
                            final_member,
                        )
                    }
                };
                (
                    format::ReviewedResultProvenanceV1::ReviewStep {
                        step: link.step,
                        entry: link.entry,
                        entry_digest: link.entry_digest,
                        selected,
                    },
                    Some(link),
                    final_member,
                )
            }
            format::ReviewedBaselineProvenanceV1::Continuation {
                provenance_digest, ..
            } => (
                format::ReviewedResultProvenanceV1::Continuation {
                    link_digest: *provenance_digest,
                },
                None,
                false,
            ),
        };
        let (lineage, archive) =
            self.advance_review_lineage(&review, &witness, step_link.as_ref(), rows, context)?;
        let result = format::ReviewedBaselineResultV1 {
            version: 1,
            key: key.clone(),
            provenance: result_provenance,
            cut,
            witness,
            lineage: lineage.clone(),
        };
        let commit_digest = result_digest(&result)?;
        rows.push(Row::ReviewedBaselineResultV1(result));
        cursor.progress_revision = connection.revision;
        if let Some(link) = step_link {
            if final_member {
                let first_operation = match link.kind {
                    LinkKind::Chain { anchor, .. } => anchor,
                    LinkKind::Equality {} => id,
                };
                let terminal = format::InitialReviewStepCompletedV1 {
                    version: 1,
                    key: key.clone(),
                    step: link.step,
                    entry: link.entry,
                    entry_digest: link.entry_digest,
                    first_operation,
                    terminal: format::ReviewTerminalV1::Completed {
                        operation: id,
                        commit_digest,
                        archive,
                    },
                    resulting_progress: connection.revision,
                    schedule_revision: cursor.schedule_revision,
                    control_sequence: cursor.control_sequence,
                };
                let digest = terminal_digest(&terminal)?;
                cursor.next_step = link.step.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                cursor.current = Current::BetweenSteps {
                    last_step: link.step,
                    completion_digest: digest,
                };
                rows.push(Row::InitialReviewStepCompletedV1(terminal));
            } else {
                let inner = connection
                    .directory_chain
                    .as_ref()
                    .ok_or(StoreError::Corrupt)?;
                cursor.current = Current::BetweenMembers {
                    step: link.step,
                    entry: link.entry,
                    anchor: inner.anchor,
                    digest: inner.digest,
                    next_index: inner.next_index,
                    predecessor: id,
                };
            }
            let all = self.prospective_review_rows(rows, context)?;
            let view = self.read_review_with_context(&review, context)?;
            let progress = self.review_coverage(&view, &all, &cursor, context)?;
            if final_member && progress.remaining.total()? == 0 {
                let format::ReviewLastStepV1::Step { digest, .. } = progress.last_step else {
                    return Err(StoreError::Corrupt);
                };
                let complete = format::InitialReviewCompletedV1 {
                    version: 1,
                    key,
                    completed_steps: cursor.next_step,
                    entries: review.count,
                    coverage_digest: progress.coverage_digest,
                    last_step_digest: digest,
                    baseline: BaselineVersion::from_connection(&connection)?,
                    lineage,
                    progress_revision: connection.revision,
                    schedule_revision: cursor.schedule_revision,
                    control_sequence: cursor.control_sequence,
                };
                cursor.current = Current::ReviewComplete {
                    completion_digest: completion_digest(&complete)?,
                };
                rows.push(Row::InitialReviewCompletedV1(complete));
            } else if cursor.next_step == 4096 {
                cursor.current = Current::StepLimit {
                    progress_digest: tagged_digest(
                        b"Connected review progress v1",
                        &Row::InitialReviewProgressV1(progress.clone()),
                    )?,
                };
            }
            rows.push(Row::InitialReviewProgressV1(progress));
        } else {
            let Some(Row::InitialReviewCompletedV1(complete)) =
                prior.get(&format::key(15, key.connection, key.origin))
            else {
                return Err(StoreError::Corrupt);
            };
            cursor.current = Current::ReviewComplete {
                completion_digest: completion_digest(complete)?,
            };
        }
        rows.push(Row::InitialReviewCursor(cursor));
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ArchiveSetSeedV1<'a> {
    key: &'a ReviewKey,
    endpoints: &'a [storage::Identity; 2],
    fence: u64,
}
#[derive(serde::Serialize)]
#[serde(tag = "kind")]
enum AncestorReceiptPreimageV1<'a> {
    Existing {
        value: &'a format::ExistingAncestorPatchV1,
    },
    Directory {
        value: &'a format::DirectoryAncestorPatchV1,
    },
}
#[derive(serde::Serialize)]
struct SupersessionPreimageV1<'a> {
    version: u8,
    key: &'a ReviewKey,
    endpoints: &'a [storage::Identity; 2],
    fence: u64,
    index: u32,
    previous: &'a LineageRef,
    operation: format::ChainOperationIdV1,
    witness_digest: [u8; 32],
    admission_digest: [u8; 32],
    receipt_digest: [u8; 32],
    result: &'a BaselineVersion,
    archives_before: u32,
    set_before: [u8; 32],
    archives_after: u32,
    set_after: [u8; 32],
}
fn edge_digest(e: &format::ChainBaselineSupersessionV1) -> Result<[u8; 32], StoreError> {
    tagged_digest(
        b"Connected baseline supersession v1",
        &SupersessionPreimageV1 {
            version: e.version,
            key: &e.key,
            endpoints: &e.endpoints,
            fence: e.fence,
            index: e.index,
            previous: &e.previous,
            operation: e.operation,
            witness_digest: e.witness_digest,
            admission_digest: e.admission_digest,
            receipt_digest: e.receipt_digest,
            result: &e.result,
            archives_before: e.archives_before,
            set_before: e.set_before,
            archives_after: e.archives_after,
            set_after: e.set_after,
        },
    )
}
fn archive_set(previous: [u8; 32], ordinal: u32, core: [u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"Connected archive append v1");
    h.update(previous);
    h.update(ordinal.to_be_bytes());
    h.update(core);
    h.finalize().into()
}
fn lineage_origin(core: [u8; 32], baseline: &BaselineVersion) -> Result<[u8; 32], StoreError> {
    let mut h = Sha256::new();
    h.update(b"Connected lineage origin v1");
    h.update(core);
    h.update(format::encode(baseline)?);
    Ok(h.finalize().into())
}
impl ConnectionStore {
    fn advance_review_lineage(
        &self,
        review: &format::InitialChainReviewV1,
        witness: &format::CommitWitness,
        link: Option<&format::InitialReviewLinkV1>,
        rows: &mut Vec<Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(LineageRef, format::ReviewArchiveRefV1), StoreError> {
        let key = review_key(review)?;
        let all = self.prospective_review_rows(rows, context)?;
        let core = if let Some(link) = link {
            if let LinkKind::Chain { anchor, digest, .. } = link.kind {
                let anchor_op = row_operation(&all, anchor)?;
                if let Some(retirement) = &anchor_op.directory_chain_retired {
                    require(
                        retirement.reason == format::ChainRetirementReason::Completed
                            && retirement.cursor.predecessor == Some(witness.operation),
                    )?;
                    let first_link = row_link(&all, anchor)?;
                    Some(format::ArchiveCore {
                        version: 1,
                        key: key.clone(),
                        endpoints: review.endpoints.clone(),
                        fence: review.fence,
                        anchor,
                        digest,
                        link_digest: format::digest(&format::encode(first_link)?),
                        retirement: retirement.clone(),
                        baseline: witness.after.clone(),
                        created: format::ArchiveCreatedV1::Baseline {
                            operation: witness.operation,
                            witness_digest: tagged_digest(
                                b"Connected baseline witness v1",
                                witness,
                            )?,
                        },
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        self.install_review_archive(review, Some(witness), core, rows, context)
    }
    fn install_review_archive(
        &self,
        review: &format::InitialChainReviewV1,
        witness: Option<&format::CommitWitness>,
        core: Option<format::ArchiveCore>,
        rows: &mut Vec<Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(LineageRef, format::ReviewArchiveRefV1), StoreError> {
        let key = review_key(review)?;
        let old =
            match self.row_with_context(&format::key(18, key.connection, key.origin), context)? {
                Some(Row::ChainBaselineLineageV1(head)) => Some(head),
                None => None,
                _ => return Err(StoreError::Corrupt),
            };
        if old.is_none() && core.is_none() {
            return Ok((LineageRef::Absent {}, format::ReviewArchiveRefV1::Absent {}));
        }
        let previous_set = match &old {
            Some(head) => head.set,
            None => tagged_digest(
                b"Connected archive set v1",
                &ArchiveSetSeedV1 {
                    key: &key,
                    endpoints: &review.endpoints,
                    fence: review.fence,
                },
            )?,
        };
        let old_count = old.as_ref().map_or(0, |head| head.archives);
        let new_count = old_count
            .checked_add(u32::from(core.is_some()))
            .ok_or(StoreError::LimitExceeded)?;
        require(new_count <= 4096)?;
        let core_digest = core
            .as_ref()
            .map(|core| tagged_digest(b"Connected archive core v1", core))
            .transpose()?;
        let set = core_digest.map_or(previous_set, |digest| {
            archive_set(previous_set, new_count, digest)
        });
        let head = if let Some(mut head) = old {
            if let Some(witness) = witness {
                require(head.current_version == witness.before)?;
                let mut edge = format::ChainBaselineSupersessionV1 {
                    version: 1,
                    key: key.clone(),
                    endpoints: review.endpoints.clone(),
                    fence: review.fence,
                    index: head
                        .head_index
                        .checked_add(1)
                        .ok_or(StoreError::LimitExceeded)?,
                    previous: head.reference(),
                    operation: witness.operation,
                    witness_digest: tagged_digest(b"Connected baseline witness v1", witness)?,
                    admission_digest: witness.admission_digest,
                    receipt_digest: tagged_digest(
                        b"Connected baseline receipt v1",
                        &witness.receipt,
                    )?,
                    result: witness.after.clone(),
                    archives_before: old_count,
                    set_before: previous_set,
                    archives_after: new_count,
                    set_after: set,
                    head: [0; 32],
                };
                edge.validate()?;
                edge.head = edge_digest(&edge)?;
                head.head_index = edge.index;
                head.head_digest = edge.head;
                head.current_version = witness.after.clone();
                head.archives = new_count;
                head.set = set;
                rows.push(Row::ChainBaselineSupersessionV1(edge));
            } else {
                require(core.as_ref().is_some_and(|core| {
                    core.baseline == head.current_version
                        && matches!(core.created, format::ArchiveCreatedV1::Retirement { .. })
                }))?;
                head.archives = new_count;
                head.set = set;
            }
            head
        } else {
            let core = core.as_ref().ok_or(StoreError::Corrupt)?;
            let core_digest = core_digest.ok_or(StoreError::Corrupt)?;
            let origin = lineage_origin(core_digest, &core.baseline)?;
            format::ChainBaselineLineageV1 {
                version: 1,
                key: key.clone(),
                endpoints: review.endpoints.clone(),
                fence: review.fence,
                origin,
                origin_version: core.baseline.clone(),
                origin_archive: format::OriginArchiveV1 {
                    anchor: core.anchor,
                    core_digest,
                },
                head_index: 0,
                head_digest: origin,
                current_version: core.baseline.clone(),
                archives: 1,
                set,
            }
        };
        let lineage = head.reference();
        let reference = if let Some(core) = core {
            let reference = format::ReviewArchiveRefV1::Present {
                anchor: core.anchor,
                core_digest: core_digest.ok_or(StoreError::Corrupt)?,
            };
            rows.push(Row::ChainCompletionBaselineV1(
                format::ChainCompletionBaselineV1 {
                    core,
                    ordinal: new_count,
                    previous_set,
                    set,
                    lineage: lineage.clone(),
                },
            ));
            reference
        } else {
            format::ReviewArchiveRefV1::Absent {}
        };
        rows.push(Row::ChainBaselineLineageV1(head));
        Ok((lineage, reference))
    }
    fn review_coverage(
        &self,
        view: &ReviewView,
        rows: &BTreeMap<Vec<u8>, Row>,
        cursor: &format::InitialReviewCursorV1,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<format::InitialReviewProgressV1, StoreError> {
        if let Some(context) = context {
            context.check()?;
        }
        use format::{
            CoverageDecisionV1 as D, CoverageEffectV1 as E, InitialReviewEntrySideV1 as S,
        };
        let key = review_key(&view.fact)?;
        let mut effects: BTreeMap<RelativePath, ((u64, u64), D)> = BTreeMap::new();
        let mut abandoned = BTreeMap::new();
        let mut terminals = BTreeMap::new();
        for row in rows.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            match row {
                Row::ReviewedBaselineResultV1(result) if result.key == key => {
                    let format::ReviewedResultProvenanceV1::ReviewStep { selected, .. } =
                        &result.provenance
                    else {
                        continue;
                    };
                    let op = row_operation(rows, result.witness.operation)?;
                    let order = (result.witness.result_revision, op.serial);
                    let commit_digest = result_digest(result)?;
                    let effect = match selected {
                        format::ReviewedSelectionV1::Chain { index, .. } => {
                            E::Member { index: *index }
                        }
                        format::ReviewedSelectionV1::Equality {} => E::Equality {},
                    };
                    let mut insert = |path: RelativePath, effect| {
                        if effects.get(&path).is_none_or(|(old, _)| order < *old) {
                            effects.insert(
                                path,
                                (
                                    order,
                                    D::Committed {
                                        operation: result.witness.operation,
                                        commit_digest,
                                        effect,
                                    },
                                ),
                            );
                        }
                    };
                    insert(op.path.to_relative()?, effect);
                    let receipt = match (
                        &result.witness.existing_ancestors,
                        &result.witness.directory_ancestors,
                    ) {
                        (format::ReviewPatch::Present { value }, _) => Some((
                            value.prefixes,
                            tagged_digest(
                                b"Connected ancestor receipt v1",
                                &AncestorReceiptPreimageV1::Existing { value },
                            )?,
                        )),
                        (_, format::ReviewPatch::Present { value }) => Some((
                            value.prefixes,
                            tagged_digest(
                                b"Connected ancestor receipt v1",
                                &AncestorReceiptPreimageV1::Directory { value },
                            )?,
                        )),
                        _ => None,
                    };
                    if let Some((mask, receipt_digest)) = receipt {
                        let mut parent = op.path.to_relative()?.parent();
                        while let Some(path) = parent {
                            if let Some(context) = context {
                                context.check()?;
                            }
                            self.storage.review_charge(0, 0, 0, 1)?;
                            let prefix = u8::try_from(path.components().len())
                                .map_err(|_| StoreError::LimitExceeded)?;
                            if mask
                                & 1u64
                                    .checked_shl(prefix.into())
                                    .ok_or(StoreError::LimitExceeded)?
                                != 0
                            {
                                insert(
                                    path.clone(),
                                    E::ExistingAncestor {
                                        prefix,
                                        receipt_digest,
                                    },
                                );
                            }
                            parent = path.parent();
                        }
                    }
                }
                Row::InitialReviewStepCompletedV1(step) if step.key == key => {
                    require(terminals.insert(step.step, step).is_none())?;
                    if let format::ReviewTerminalV1::Abandoned { core_digest, .. } = step.terminal {
                        abandoned
                            .entry(step.entry)
                            .or_insert((step.step, core_digest));
                    }
                }
                _ => (),
            }
        }
        require(terminals.len() == cursor.next_step as usize)?;
        let mut last_step = format::ReviewLastStepV1::None {};
        for (expected, (step, fact)) in terminals.iter().enumerate() {
            require(*step as usize == expected)?;
            last_step = format::ReviewLastStepV1::Step {
                step: *step,
                digest: terminal_digest(fact)?,
            };
        }
        let mut h = Sha256::new();
        h.update(b"Connected review coverage v1");
        let bytes = format::encode(&key)?;
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
        let mut covered = 0u32;
        let mut remaining = format::ReviewRemainingV1 {
            unselected: 0,
            structural: 0,
            conflict: 0,
            abandoned: 0,
        };
        for (ordinal, entry) in view.entries.iter().enumerate() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            require(entry.ordinal as usize == ordinal)?;
            let path = entry.path.to_relative()?;
            let decision = if let Some((_, effect)) = effects.get(&path) {
                covered += 1;
                effect.clone()
            } else if let Some((step, core_digest)) = abandoned.get(&entry.ordinal) {
                remaining.abandoned += 1;
                D::Abandoned {
                    step: *step,
                    core_digest: *core_digest,
                }
            } else {
                match &entry.sides {
                    [S::Present { entry: a }, S::Present { entry: b }] if a.state != b.state => {
                        remaining.conflict += 1;
                        D::Conflict {}
                    }
                    [S::Present { entry: a }, S::Present { .. }]
                        if a.state == format::State::Directory =>
                    {
                        remaining.structural += 1;
                        D::Structural {}
                    }
                    _ => {
                        remaining.unselected += 1;
                        D::Unselected {}
                    }
                }
            };
            let entry = format::CoverageEntryV1 {
                ordinal: entry.ordinal,
                entry_digest: format::digest(&format::encode(entry)?),
                decision,
            };
            let bytes = format::encode(&entry)?;
            h.update([1]);
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
        require(view.entries.len() == view.fact.count as usize)?;
        h.update([0]);
        h.update((view.entries.len() as u64).to_le_bytes());
        Ok(format::InitialReviewProgressV1 {
            version: 1,
            key,
            completed_steps: cursor.next_step,
            covered,
            remaining,
            coverage_digest: h.finalize().into(),
            last_step,
            progress_revision: cursor.progress_revision,
        })
    }
}

impl ConnectionStore {
    fn baseline_version_entries(
        &self,
        connection: &format::Connection,
        version: &BaselineVersion,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<BTreeMap<RelativePath, EntryState>, StoreError> {
        self.baseline_version_entries_with(connection, version, &|root, count, revision| {
            self.read_historical_baseline_with_context(connection, root, count, revision, context)
        })
    }
    fn baseline_version_entries_with(
        &self,
        connection: &format::Connection,
        version: &BaselineVersion,
        read: &dyn Fn(Option<[u8; 32]>, u32, u64) -> Result<VerifiedBaseline, StoreError>,
    ) -> Result<BTreeMap<RelativePath, EntryState>, StoreError> {
        version.validate()?;
        match version {
            BaselineVersion::Absent {} => Ok(BTreeMap::new()),
            BaselineVersion::Present {
                revision,
                root,
                count,
            } => {
                require(*revision <= connection.revision)?;
                Ok(read(root.root(), *count, *revision)?
                    .entries()
                    .cloned()
                    .unwrap_or_default())
            }
        }
    }
    fn validate_durable_review(
        &self,
        review: &format::InitialChainReviewV1,
        rows: &BTreeMap<Vec<u8>, Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let key = review_key(review)?;
        let view = self.read_review_with_context(review, context)?;
        let Some(Row::Connection(c)) = rows.get(&format::key(0, key.connection, 0)) else {
            return Err(StoreError::Corrupt);
        };
        let Some(Row::InitialReviewCursor(cursor)) =
            rows.get(&format::key(9, key.connection, key.origin))
        else {
            return Err(StoreError::Corrupt);
        };
        let original = review.original_record()?;
        require(
            c.source == original.source
                && c.destination == original.destination
                && c.initial_empty.is_none()
                && cursor.review == key.review
                && cursor.origin == key.origin
                && cursor.progress_revision <= c.revision,
        )?;
        require(if cursor.state == Schedule::Disconnected {
            c.lifecycle != 0 && c.fence == format::next(review.fence)?
        } else {
            c.lifecycle == 0 && c.fence == review.fence && cursor.progress_revision == c.revision
        })?;
        for row in rows.values() {
            if review_connection(row) != Some(key.connection) {
                continue;
            }
            row.validate()?;
            match row {
                Row::InitialChainReview(r) => require(r == review)?,
                Row::InitialReviewCursor(r) => {
                    require(r.origin == key.origin && r.review == key.review)?
                }
                Row::InitialReviewLink(r) => require(
                    r.origin == key.origin
                        && r.review == key.review
                        && r.operation.connection == key.connection,
                )?,
                Row::InitialReviewStop(r) => {
                    let f = r.fact();
                    require(
                        f.origin == key.origin
                            && f.review == key.review
                            && rows.contains_key(&format::key(
                                10,
                                key.connection,
                                f.operation.serial,
                            )),
                    )?;
                }
                Row::ReviewedBaselineResultV1(r) => require(r.key == key)?,
                Row::InitialReviewStepCompletedV1(r) => require(r.key == key)?,
                Row::InitialReviewProgressV1(r) => require(r.key == key)?,
                Row::InitialReviewCompletedV1(r) => require(r.key == key)?,
                Row::PostReviewOperationV1(r) => require(r.key == key)?,
                Row::ChainCompletionBaselineV1(r) => require(r.core.key == key)?,
                Row::ChainBaselineLineageV1(r) => require(r.key == key)?,
                Row::ChainBaselineSupersessionV1(r) => require(r.key == key)?,
                _ => return Err(StoreError::Corrupt),
            }
        }
        let results = self.review_result_index(&key, rows, context)?;
        let cut_index = self.review_cut_index(&key, rows, &results, context)?;
        self.with_review_baseline_owner(review, rows, context, |read| {
            let mut before = BaselineVersion::Absent {};
            for result in results.values() {
                let w = &result.witness;
                let op = row_operation(rows, w.operation)?;
                require(
                    op.stage == 4
                        && op.revision == w.result_revision
                        && op.initial_review.is_none()
                        && op.native_budget.as_ref() == Some(&w.budget)
                        && original_intent_digest(op)? == w.intent_digest
                        && format::digest(
                            op.native_identity.as_deref().ok_or(StoreError::Corrupt)?,
                        ) == w.admission_digest
                        && w.before == before
                        && op.expected < w.precommit_revision
                        && op.baseline == w.before.revision()
                        && op.source_binding == review.endpoints[0]
                        && op.destination_binding == review.endpoints[1],
                )?;
                let current = match &result.provenance {
                    format::ReviewedResultProvenanceV1::ReviewStep {
                        step,
                        entry,
                        entry_digest,
                        selected,
                    } => {
                        let link = row_link(rows, w.operation)?;
                        require(
                            link.step == *step
                                && link.entry == *entry
                                && link.entry_digest == *entry_digest,
                        )?;
                        let anchor = match link.kind {
                            LinkKind::Chain { anchor, .. } => Some(row_operation(rows, anchor)?),
                            _ => None,
                        };
                        view.validate_link(link, op, anchor)?;
                        self.validate_link_cut(&view, link, op, rows, &cut_index, context)?;
                        match (selected, &link.kind) {
                            (format::ReviewedSelectionV1::Equality {}, LinkKind::Equality {}) => (),
                            (
                                format::ReviewedSelectionV1::Chain {
                                    anchor,
                                    digest,
                                    index,
                                    final_member,
                                },
                                LinkKind::Chain {
                                    anchor: a,
                                    digest: d,
                                    ..
                                },
                            ) => {
                                let member =
                                    op.directory_chain_ref.as_ref().ok_or(StoreError::Corrupt)?;
                                let selection =
                                    super::chain::first_selection(anchor_op(rows, *anchor)?)?;
                                require(
                                    anchor == a
                                        && digest == d
                                        && member.index == *index
                                        && *final_member
                                            == (index.checked_add(1)
                                                == Some(selection.member_count()?)),
                                )?;
                            }
                            _ => return Err(StoreError::Corrupt),
                        }
                        link.current()
                    }
                    format::ReviewedResultProvenanceV1::Continuation { link_digest } => {
                        let Some(Row::PostReviewOperationV1(link)) =
                            rows.get(&format::key(16, key.connection, op.serial))
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        require(*link_digest == continuation_digest(link)?)?;
                        Current::Continuation {
                            operation: w.operation,
                            provenance_digest: *link_digest,
                        }
                    }
                };
                require(
                    result.cut.current_digest == format::digest(&format::encode(&current)?)
                        && self.commit_receipt(op, context)? == w.receipt,
                )?;
                let old = self.baseline_version_entries_with(c, &w.before, read)?;
                let existing = match &w.existing_ancestors {
                    format::ReviewPatch::Absent {} => None,
                    format::ReviewPatch::Present { value } => Some(value),
                };
                let directory = match &w.directory_ancestors {
                    format::ReviewPatch::Absent {} => None,
                    format::ReviewPatch::Present { value } => Some(value),
                };
                let chain_first = op
                    .directory_chain_ref
                    .as_ref()
                    .is_some_and(|r| r.index == 0)
                    && op.baseline.is_none();
                let patch =
                    self.native_baseline_patch(op, old, chain_first, existing, directory, context)?;
                require(patch == self.baseline_version_entries_with(c, &w.after, read)?)?;
                if let Some(Row::InitialReviewStop(boundary)) =
                    rows.get(&format::key(11, key.connection, op.serial))
                {
                    self.validate_completed_equality_stop(
                        &view,
                        row_link(rows, w.operation)?,
                        boundary,
                        CompletedEqualityView {
                            operation: op,
                            result,
                        },
                        context,
                    )?;
                }

                before = w.after.clone();
            }
            require(before == BaselineVersion::from_connection(c)?)?;
            Ok(())
        })?;
        self.validate_review_lineage(review, rows, c, &results, context)?;
        let mut steps = BTreeMap::new();
        for row in rows.values() {
            if let Row::InitialReviewStepCompletedV1(step) = row {
                if step.key.connection != key.connection {
                    continue;
                }
                step.validate()?;
                require(step.key == key && steps.insert(step.step, step).is_none())?;
                let entry = view
                    .entries
                    .get(step.entry as usize)
                    .ok_or(StoreError::Corrupt)?;
                require(
                    step.entry_digest == format::digest(&format::encode(entry)?)
                        && row_link(rows, step.first_operation)?.step == step.step,
                )?;
                match &step.terminal {
                    format::ReviewTerminalV1::Completed {
                        operation,
                        commit_digest,
                        archive,
                    } => {
                        let Some(Row::ReviewedBaselineResultV1(result)) =
                            rows.get(&format::key(12, key.connection, operation.serial))
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        require(
                            result_digest(result)? == *commit_digest
                                && result.witness.result_revision == step.resulting_progress
                                && result.cut.schedule_revision == step.schedule_revision
                                && result.cut.control_sequence == step.control_sequence,
                        )?;
                        match (&result.provenance, archive) {
                            (
                                format::ReviewedResultProvenanceV1::ReviewStep {
                                    step: n,
                                    entry: e,
                                    selected: format::ReviewedSelectionV1::Equality {},
                                    ..
                                },
                                format::ReviewArchiveRefV1::Absent {},
                            ) => require(
                                *n == step.step
                                    && *e == step.entry
                                    && step.first_operation == *operation,
                            )?,
                            (
                                format::ReviewedResultProvenanceV1::ReviewStep {
                                    step: n,
                                    entry: e,
                                    selected:
                                        format::ReviewedSelectionV1::Chain {
                                            anchor,
                                            final_member: true,
                                            ..
                                        },
                                    ..
                                },
                                format::ReviewArchiveRefV1::Present {
                                    anchor: a,
                                    core_digest,
                                },
                            ) => {
                                let Some(Row::ChainCompletionBaselineV1(archive)) =
                                    rows.get(&format::key(17, key.connection, anchor.serial))
                                else {
                                    return Err(StoreError::Corrupt);
                                };
                                require(
                                    *n == step.step
                                        && *e == step.entry
                                        && anchor == a
                                        && *anchor == step.first_operation
                                        && tagged_digest(
                                            b"Connected archive core v1",
                                            &archive.core,
                                        )? == *core_digest,
                                )?;
                            }
                            _ => return Err(StoreError::Corrupt),
                        }
                    }
                    format::ReviewTerminalV1::Abandoned {
                        anchor,
                        core_digest,
                    } => {
                        let Some(Row::ChainCompletionBaselineV1(archive)) =
                            rows.get(&format::key(17, key.connection, anchor.serial))
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        require(
                            archive.core.retirement.reason
                                == format::ChainRetirementReason::Abandoned
                                && tagged_digest(b"Connected archive core v1", &archive.core)?
                                    == *core_digest
                                && step.first_operation == *anchor,
                        )?;
                        let format::ArchiveCreatedV1::Retirement { result, .. } =
                            &archive.core.created
                        else {
                            return Err(StoreError::Corrupt);
                        };
                        require(
                            step.resulting_progress == result.progress_revision
                                && step.schedule_revision == result.schedule_revision
                                && step.control_sequence == result.control_sequence,
                        )?;
                    }
                }
            }
        }
        require(steps.len() == cursor.next_step as usize)?;
        for (index, step) in steps.keys().enumerate() {
            require(index == *step as usize)?;
        }
        let Some(Row::InitialReviewProgressV1(progress)) =
            rows.get(&format::key(14, key.connection, key.origin))
        else {
            return Err(StoreError::Corrupt);
        };
        let mut progress_cursor = cursor.clone();
        progress_cursor.progress_revision = progress.progress_revision;
        require(
            *progress == self.review_coverage(&view, rows, &progress_cursor, context)?
                && progress.progress_revision <= cursor.progress_revision,
        )?;
        let complete = match rows.get(&format::key(15, key.connection, key.origin)) {
            Some(Row::InitialReviewCompletedV1(complete)) => {
                require(
                    complete.key == key
                        && complete.completed_steps == progress.completed_steps
                        && complete.entries == review.count
                        && complete.coverage_digest == progress.coverage_digest
                        && progress.remaining.total()? == 0
                        && complete.progress_revision == progress.progress_revision,
                )?;
                let format::ReviewLastStepV1::Step { digest, .. } = progress.last_step else {
                    return Err(StoreError::Corrupt);
                };
                require(
                    steps
                        .get(&(complete.completed_steps - 1))
                        .is_some_and(|step| {
                            step.schedule_revision == complete.schedule_revision
                                && step.control_sequence == complete.control_sequence
                        })
                        && complete.last_step_digest == digest
                        && results.get(&complete.progress_revision).is_some_and(|r| {
                            r.witness.after == complete.baseline && r.lineage == complete.lineage
                        }),
                )?;
                Some(complete)
            }
            None => None,
            _ => return Err(StoreError::Corrupt),
        };
        for row in rows.values() {
            if let Row::PostReviewOperationV1(link) = row {
                if link.key.connection != key.connection {
                    continue;
                }
                link.validate()?;
                let complete = complete.ok_or(StoreError::Corrupt)?;
                let op = row_operation(rows, link.operation)?;
                let admitted = results
                    .range(..=op.expected)
                    .next_back()
                    .map(|(_, result)| *result)
                    .ok_or(StoreError::Corrupt)?;
                require(
                    link.key == key
                        && link.completion_digest == completion_digest(complete)?
                        && complete.progress_revision <= op.expected
                        && link.expected == op.expected
                        && link.baseline == admitted.witness.after
                        && link.lineage == admitted.lineage
                        && link.intent_digest == original_intent_digest(op)?
                        && link.admission_digest
                            == format::digest(
                                op.native_identity.as_deref().ok_or(StoreError::Corrupt)?,
                            )
                        && op.directory_chain_ref.is_none()
                        && op.directory_chain_anchor.is_none()
                        && op.initial_review.is_none()
                        && (op.stage == 4
                            && rows.contains_key(&format::key(12, key.connection, op.serial))
                            || cursor.current.operation() == Some(link.operation)),
                )?;
                validate_continuation_kind(op, &link.kind)?;
            }
        }
        match &cursor.current {
            Current::BetweenSteps {
                last_step,
                completion_digest,
            } => {
                let step = steps.get(last_step).ok_or(StoreError::Corrupt)?;
                require(
                    cursor.next_step == last_step + 1
                        && cursor.next_step < 4096
                        && *completion_digest == terminal_digest(step)?
                        && c.pending.is_none()
                        && c.directory_chain.is_none()
                        && c.directory_chain_schedule.is_none()
                        && complete.is_none(),
                )?;
            }
            Current::ReviewComplete {
                completion_digest: digest,
            } => require(
                complete.is_some_and(|f| completion_digest(f).is_ok_and(|d| d == *digest))
                    && c.pending.is_none()
                    && c.directory_chain.is_none(),
            )?,
            Current::StepLimit { progress_digest } => require(
                cursor.next_step == 4096
                    && progress.remaining.total()? > 0
                    && *progress_digest
                        == tagged_digest(
                            b"Connected review progress v1",
                            &Row::InitialReviewProgressV1(progress.clone()),
                        )?
                    && c.pending.is_none(),
            )?,
            Current::BetweenMembers {
                step,
                entry,
                anchor,
                digest,
                next_index,
                predecessor,
            } => {
                let inner = c.directory_chain.as_ref().ok_or(StoreError::Corrupt)?;
                let predecessor_op = row_operation(rows, *predecessor)?;
                let link = row_link(rows, *predecessor)?;
                validate_current_chain_control(c, cursor, *anchor, *digest, None)?;
                require(
                    *step == cursor.next_step
                        && link.step == *step
                        && link.entry == *entry
                        && inner.anchor == *anchor
                        && inner.digest == *digest
                        && inner.next_index == *next_index
                        && inner.predecessor == Some(*predecessor)
                        && inner.current.is_none()
                        && c.pending.is_none()
                        && predecessor_op.stage == 4,
                )?;
            }
            Current::Chain { .. } | Current::Equality { .. } => {
                let id = cursor.current.operation().ok_or(StoreError::Corrupt)?;
                let op = row_operation(rows, id)?;
                let link = row_link(rows, id)?;
                require(
                    link.current() == cursor.current
                        && link.step == cursor.next_step
                        && c.serial == id.serial,
                )?;
                require(if cursor.state == Schedule::Disconnected {
                    op.revision >= cursor.progress_revision
                } else {
                    op.revision == cursor.progress_revision && c.pending == Some(id.serial)
                })?;
                let anchor = match link.kind {
                    LinkKind::Chain { anchor, .. } => Some(row_operation(rows, anchor)?),
                    _ => None,
                };
                view.validate_link(link, op, anchor)?;
                match cursor.current {
                    Current::Chain { anchor, digest, .. } => {
                        validate_current_chain_control(c, cursor, anchor, digest, Some(id))?
                    }
                    Current::Equality { .. } => {
                        self.validate_current_equality(&view, link, cursor, op, c, rows, context)?
                    }
                    _ => return Err(StoreError::Corrupt),
                }
            }
            Current::AbandoningChain {
                step,
                entry,
                anchor,
                digest,
                cut,
            } => {
                let inner = c.directory_chain.as_ref().ok_or(StoreError::Corrupt)?;
                let schedule = c
                    .directory_chain_schedule
                    .as_ref()
                    .ok_or(StoreError::Corrupt)?;
                require(
                    *step == cursor.next_step
                        && inner.anchor == *anchor
                        && inner.digest == *digest
                        && inner.progress_revision == cursor.progress_revision
                        && schedule.state == cursor.state
                        && matches!(cursor.state, Schedule::Abandoning | Schedule::Disconnected),
                )?;
                match cut {
                    format::ReviewAbandonCut::Member { operation } => {
                        let op = row_operation(rows, *operation)?;
                        let link = row_link(rows, *operation)?;
                        require(
                            link.step == *step
                                && link.entry == *entry
                                && current_matches_link(&cursor.current, link)
                                && inner.current == Some(*operation)
                                && op.revision <= c.revision
                                && (op.stage == 7 && c.pending.is_none()
                                    || op.stage != 7 && c.pending == Some(operation.serial)),
                        )?;
                    }
                    format::ReviewAbandonCut::BetweenMembers {
                        next_index,
                        predecessor,
                    } => {
                        let op = row_operation(rows, *predecessor)?;
                        let link = row_link(rows, *predecessor)?;
                        require(
                            link.step == *step
                                && link.entry == *entry
                                && inner.current.is_none()
                                && inner.next_index == *next_index
                                && inner.predecessor == Some(*predecessor)
                                && c.pending.is_none()
                                && op.stage == 4,
                        )?;
                    }
                }
            }
            Current::Continuation {
                operation,
                provenance_digest,
            } => {
                let Some(Row::PostReviewOperationV1(link)) =
                    rows.get(&format::key(16, key.connection, operation.serial))
                else {
                    return Err(StoreError::Corrupt);
                };
                require(
                    complete.is_some()
                        && continuation_digest(link)? == *provenance_digest
                        && c.pending == Some(operation.serial)
                        && row_operation(rows, *operation)?.revision == cursor.progress_revision,
                )?;
            }
            _ => return Err(StoreError::Corrupt),
        }
        for row in rows.values() {
            if let Row::InitialReviewLink(link) = row {
                if link.connection != key.connection {
                    continue;
                }
                require(link.step <= cursor.next_step)?;
                if link.step < cursor.next_step {
                    let abandoned = steps.get(&link.step).is_some_and(|step| matches!(step.terminal, format::ReviewTerminalV1::Abandoned { anchor, .. } if rows.get(&format::key(17, key.connection, anchor.serial)).is_some_and(|row| matches!(row, Row::ChainCompletionBaselineV1(a) if a.core.retirement.cursor.current == Some(link.operation) && row_operation(rows, link.operation).is_ok_and(|op| op.stage == 7)))));
                    require(
                        rows.contains_key(&format::key(12, key.connection, link.operation.serial))
                            || abandoned,
                    )?;
                } else {
                    let completed_member = rows.get(&format::key(12, key.connection, link.operation.serial)).is_some_and(|row| matches!(row, Row::ReviewedBaselineResultV1(r) if matches!((&r.provenance, &link.kind), (format::ReviewedResultProvenanceV1::ReviewStep { step, entry, selected: format::ReviewedSelectionV1::Chain { anchor, digest, final_member: false, .. }, .. }, LinkKind::Chain { anchor: a, digest: d, .. }) if *step == link.step && *entry == link.entry && anchor == a && digest == d)));
                    require(current_matches_link(&cursor.current, link) || completed_member)?;
                }
            }
        }
        Ok(())
    }
}
fn anchor_op(
    rows: &BTreeMap<Vec<u8>, Row>,
    id: format::ChainOperationIdV1,
) -> Result<&format::Operation, StoreError> {
    row_operation(rows, id)
}

impl ConnectionStore {
    fn validate_review_lineage(
        &self,
        review: &format::InitialChainReviewV1,
        rows: &BTreeMap<Vec<u8>, Row>,
        connection: &format::Connection,
        results: &BTreeMap<u64, &format::ReviewedBaselineResultV1>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        if let Some(context) = context {
            context.check()?;
        }
        let key = review_key(review)?;
        let mut archives = BTreeMap::new();
        let mut edges = BTreeMap::new();
        for row in rows.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            match row {
                Row::ChainCompletionBaselineV1(a) if a.core.key.connection == key.connection => {
                    a.validate()?;
                    require(a.core.key == key && archives.insert(a.ordinal, a).is_none())?;
                }
                Row::ChainBaselineSupersessionV1(e) if e.key.connection == key.connection => {
                    e.validate()?;
                    require(e.key == key && edges.insert(e.index, e).is_none())?;
                }
                _ => (),
            }
        }
        let head = match rows.get(&format::key(18, key.connection, key.origin)) {
            Some(Row::ChainBaselineLineageV1(head)) => head,
            None => {
                require(
                    archives.is_empty()
                        && edges.is_empty()
                        && results
                            .values()
                            .all(|r| matches!(r.lineage, LineageRef::Absent {})),
                )?;
                return Ok(());
            }
            _ => return Err(StoreError::Corrupt),
        };
        head.validate()?;
        require(
            head.key == key
                && head.endpoints == review.endpoints
                && head.fence == review.fence
                && head.archives as usize == archives.len()
                && head.head_index as usize == edges.len()
                && head.current_version == BaselineVersion::from_connection(connection)?,
        )?;
        let first = archives.get(&1).ok_or(StoreError::Corrupt)?;
        let first_digest = tagged_digest(b"Connected archive core v1", &first.core)?;
        let origin = lineage_origin(first_digest, &first.core.baseline)?;
        require(
            head.origin == origin
                && head.origin_version == first.core.baseline
                && head.origin_archive.anchor == first.core.anchor
                && head.origin_archive.core_digest == first_digest,
        )?;
        let mut sets = BTreeMap::new();
        let mut set = tagged_digest(
            b"Connected archive set v1",
            &ArchiveSetSeedV1 {
                key: &key,
                endpoints: &review.endpoints,
                fence: review.fence,
            },
        )?;
        sets.insert(0u32, set);
        let mut last_checkpoint = 0u32;
        for (expected, (ordinal, a)) in archives.iter().enumerate() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            let core = &a.core;
            let op = row_operation(rows, core.anchor)?;
            let selection = super::chain::first_selection(op)?;
            require(
                *ordinal as usize == expected + 1
                    && core.endpoints == review.endpoints
                    && core.fence == review.fence
                    && core.digest == selection.selection_digest
                    && op.directory_chain_retired.as_ref() == Some(&core.retirement)
                    && core.link_digest
                        == format::digest(&format::encode(row_link(rows, core.anchor)?)?),
            )?;
            let digest = tagged_digest(b"Connected archive core v1", core)?;
            require(a.previous_set == set && a.set == archive_set(set, *ordinal, digest))?;
            set = a.set;
            sets.insert(*ordinal, set);
            let LineageRef::Present {
                origin: o,
                index,
                version,
                ..
            } = &a.lineage
            else {
                return Err(StoreError::Corrupt);
            };
            require(
                *o == origin
                    && *index >= last_checkpoint
                    && *index <= head.head_index
                    && *version == core.baseline,
            )?;
            last_checkpoint = *index;
            match &core.created {
                format::ArchiveCreatedV1::Baseline {
                    operation,
                    witness_digest,
                } => {
                    let result = results
                        .get(&core.baseline.revision().ok_or(StoreError::Corrupt)?)
                        .ok_or(StoreError::Corrupt)?;
                    require(
                        result.witness.operation == *operation
                            && result.witness.after == core.baseline
                            && tagged_digest(b"Connected baseline witness v1", &result.witness)?
                                == *witness_digest
                            && result.lineage == a.lineage
                            && core.retirement.reason == format::ChainRetirementReason::Completed
                            && core.retirement.cursor.predecessor == Some(*operation),
                    )?;
                }
                format::ArchiveCreatedV1::Retirement {
                    before,
                    inner,
                    result,
                } => {
                    let link = row_link(rows, core.anchor)?;
                    let cut = if let Some(operation) = core.retirement.cursor.current {
                        format::ReviewAbandonCut::Member { operation }
                    } else {
                        format::ReviewAbandonCut::BetweenMembers {
                            next_index: core.retirement.cursor.next_index,
                            predecessor: core
                                .retirement
                                .cursor
                                .predecessor
                                .ok_or(StoreError::Corrupt)?,
                        }
                    };
                    let current = Current::AbandoningChain {
                        step: link.step,
                        entry: link.entry,
                        anchor: core.anchor,
                        digest: core.digest,
                        cut,
                    };
                    let actual = results
                        .range(..=before.progress_revision)
                        .next_back()
                        .map(|(_, r)| r.witness.after.clone())
                        .unwrap_or(BaselineVersion::Absent {});
                    require(
                        core.retirement.reason == format::ChainRetirementReason::Abandoned
                            && inner.transition
                                == format::DirectoryChainControlTransitionV1::Retire
                            && inner.anchor == core.anchor
                            && inner.digest == core.digest
                            && inner.connection == key.connection
                            && inner.expected_progress == before.progress_revision
                            && before.current_digest == format::digest(&format::encode(&current)?)
                            && before.progress_revision == result.progress_revision
                            && result.progress_revision == core.retirement.cursor.progress_revision
                            && before.schedule_revision.checked_add(1)
                                == Some(result.schedule_revision)
                            && before.control_sequence.checked_add(1)
                                == Some(result.control_sequence)
                            && core.baseline == actual,
                    )?;
                }
            }
        }
        require(set == head.set)?;
        let mut checkpoints = BTreeMap::new();
        let mut reference = LineageRef::Present {
            origin,
            index: 0,
            head: origin,
            version: head.origin_version.clone(),
        };
        checkpoints.insert(0u32, reference.clone());
        let mut result_versions = BTreeMap::new();
        if matches!(
            first.core.created,
            format::ArchiveCreatedV1::Baseline { .. }
        ) {
            if let Some(revision) = head.origin_version.revision() {
                result_versions.insert(revision, reference.clone());
            }
        }
        let mut archive_iter = archives.values().peekable();
        let mut count = 0u32;
        while archive_iter
            .peek()
            .is_some_and(|a| matches!(a.lineage, LineageRef::Present { index: 0, .. }))
        {
            archive_iter.next();
            count += 1;
        }
        require(count >= 1)?;
        for (expected, (index, edge)) in edges.iter().enumerate() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            require(
                *index as usize == expected + 1
                    && edge.previous == reference
                    && edge.endpoints == review.endpoints
                    && edge.fence == review.fence
                    && edge.head == edge_digest(edge)?
                    && edge.archives_before == count
                    && sets.get(&count) == Some(&edge.set_before),
            )?;
            let result = results
                .get(&edge.result.revision().ok_or(StoreError::Corrupt)?)
                .ok_or(StoreError::Corrupt)?;
            require(
                result.witness.operation == edge.operation
                    && result.witness.after == edge.result
                    && result.witness.before
                        == match &reference {
                            LineageRef::Present { version, .. } => version.clone(),
                            _ => return Err(StoreError::Corrupt),
                        }
                    && tagged_digest(b"Connected baseline witness v1", &result.witness)?
                        == edge.witness_digest
                    && tagged_digest(b"Connected baseline receipt v1", &result.witness.receipt)?
                        == edge.receipt_digest
                    && result.witness.admission_digest == edge.admission_digest,
            )?;
            reference = LineageRef::Present {
                origin,
                index: *index,
                head: edge.head,
                version: edge.result.clone(),
            };
            require(result.lineage == reference)?;
            checkpoints.insert(*index, reference.clone());
            result_versions.insert(result.witness.result_revision, reference.clone());
            if archive_iter.peek().is_some_and(|a| {
                matches!(a.lineage, LineageRef::Present { index: i, .. } if i == *index)
                    && matches!(a.core.created, format::ArchiveCreatedV1::Baseline { .. })
            }) {
                archive_iter.next();
                count += 1;
            }
            require(edge.archives_after == count && sets.get(&count) == Some(&edge.set_after))?;
            while archive_iter.peek().is_some_and(
                |a| matches!(a.lineage, LineageRef::Present { index: i, .. } if i == *index),
            ) {
                archive_iter.next();
                count += 1;
            }
        }
        require(
            archive_iter.next().is_none()
                && count == head.archives
                && reference == head.reference(),
        )?;
        for a in archives.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            let LineageRef::Present { index, .. } = a.lineage else {
                return Err(StoreError::Corrupt);
            };
            require(checkpoints.get(&index) == Some(&a.lineage))?;
        }
        for result in results.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            if let Some(expected) = result_versions.get(&result.witness.result_revision) {
                require(*expected == result.lineage)?;
            } else {
                require(
                    matches!(result.lineage, LineageRef::Absent {})
                        && head.origin_version.revision().is_some_and(|r| {
                            result.witness.result_revision < r
                                || result.witness.result_revision == r
                                    && matches!(
                                        first.core.created,
                                        format::ArchiveCreatedV1::Retirement { .. }
                                    )
                        }),
                )?;
            }
        }
        Ok(())
    }
}

impl ConnectionStore {
    // The baseline and masks come only from canonical effects before admission.
    // Callers cannot supply a revision/root pair or mask as proof.
    fn checked_review_selection(
        &self,
        view: &ReviewView,
        ordinal: u32,
        revision: u64,
        rows: &BTreeMap<Vec<u8>, Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<super::super::transaction::chain::DirectoryChainSelectionV1, StoreError> {
        let mut selected = view.selection(ordinal, revision)?;
        let mut version = BaselineVersion::Absent {};
        let mut latest = 0;
        let mut materialized = std::collections::BTreeSet::new();
        for row in rows.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            if let Row::ReviewedBaselineResultV1(result) = row {
                if result.key.connection != view.fact.connection
                    || result.witness.result_revision > revision
                {
                    continue;
                }
                if result.witness.result_revision > latest {
                    latest = result.witness.result_revision;
                    version = result.witness.after.clone();
                }
                let op = row_operation(rows, result.witness.operation)?;
                if op.native_kind == Some(format::NativeOperationKind::DirectoryV1) {
                    materialized.insert(op.path.to_relative()?);
                }
                let mask = match (
                    &result.witness.existing_ancestors,
                    &result.witness.directory_ancestors,
                ) {
                    (format::ReviewPatch::Present { value }, _) => value.prefixes,
                    (_, format::ReviewPatch::Present { value }) => value.prefixes,
                    _ => 0,
                };
                let mut parent = op.path.to_relative()?.parent();
                while let Some(path) = parent {
                    if let Some(context) = context {
                        context.check()?;
                    }
                    self.storage.review_charge(0, 0, 0, 1)?;
                    if mask
                        & 1u64
                            .checked_shl(path.components().len() as u32)
                            .ok_or(StoreError::LimitExceeded)?
                        != 0
                    {
                        materialized.insert(path.clone());
                    }
                    parent = path.parent();
                }
            }
        }
        let Some(Row::Connection(connection)) = rows.get(&format::key(0, view.fact.connection, 0))
        else {
            return Err(StoreError::Corrupt);
        };
        let baseline = self.baseline_version_entries(connection, &version, context)?;
        selected.baseline = version.revision();
        let mut parent = selected.path.parent();
        while let Some(path) = parent {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            if baseline.get(&path) == Some(&EntryState::Directory) && materialized.contains(&path) {
                let bit = 1u64
                    .checked_shl(path.components().len() as u32)
                    .ok_or(StoreError::LimitExceeded)?;
                selected.baselined_prefixes |= bit;
                selected.existing_prefixes &= !bit;
                selected.missing_prefixes &= !bit;
            }
            parent = path.parent();
        }
        selected.checked()
    }
    fn validate_link_cut(
        &self,
        view: &ReviewView,
        link: &format::InitialReviewLinkV1,
        op: &format::Operation,
        rows: &BTreeMap<Vec<u8>, Row>,
        cut_index: &ReviewCutIndex<'_>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let anchor = match link.kind {
            LinkKind::Chain { anchor, .. } => Some(row_operation(rows, anchor)?),
            _ => None,
        };
        view.validate_link(link, op, anchor)?;
        if let Some(anchor) = anchor {
            require(
                super::chain::first_selection(anchor)?
                    == self.checked_review_selection(
                        view,
                        link.entry,
                        anchor.expected,
                        rows,
                        context,
                    )?,
            )?;
        } else {
            let version = cut_index
                .results
                .range(..=op.expected)
                .next_back()
                .map(|(_, result)| &result.witness.after);
            require(op.baseline == version.and_then(|v| v.revision()))?;
        }
        require(
            cut_index
                .prefix_progress
                .get(link.step as usize)
                .is_some_and(|progress| *progress <= op.expected),
        )
    }
}

impl ConnectionStore {
    pub(super) fn owned_review_for_candidate(
        &self,
        id: [u8; 16],
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<[u8; 32], StoreError> {
        Ok(self
            .owned_review_with_context(id, context)?
            .ok_or(StoreError::RecoveryRequired)?
            .root)
    }
}

/// A baseline tuple admitted only after complete prospective receipt/lineage
/// closure. Both retired-chain read sites consume this same validated version.
pub(super) struct RetiredBaselineView {
    version: BaselineVersion,
}
impl RetiredBaselineView {
    pub(super) fn revision(&self) -> Option<u64> {
        self.version.revision()
    }
    pub(super) fn root(&self) -> Option<[u8; 32]> {
        self.version.root()
    }
    pub(super) fn count(&self) -> u32 {
        self.version.count()
    }
}
impl ConnectionStore {
    pub(super) fn retired_baseline_views(
        &self,
        replacements: &[Row],
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<BTreeMap<([u8; 16], u64), RetiredBaselineView>, StoreError> {
        let _read_lease = self.history_read_context(context)?;
        if let Some(context) = context {
            context.check()?;
        }
        self.validate_review_history_with_context(replacements, context)?;
        let rows = self.prospective_review_rows(replacements, context)?;
        let mut result = BTreeMap::new();
        for row in rows.values() {
            if let Row::ChainCompletionBaselineV1(archive) = row {
                if let Some(context) = context {
                    context.check()?;
                }
                self.storage.review_charge(0, 0, 0, 1)?;
                let key = &archive.core.key;
                require(
                    result
                        .insert(
                            (key.connection, archive.core.anchor.serial),
                            RetiredBaselineView {
                                version: archive.core.baseline.clone(),
                            },
                        )
                        .is_none(),
                )?;
            }
        }
        Ok(result)
    }
}

impl ConnectionStore {
    fn complete_continuation_intent(
        &self,
        operation: &format::Operation,
        provenance: &format::PostReviewOperationV1,
        expected_progress: u64,
        expected_schedule: u64,
        control_sequence: u64,
        rows: &mut Vec<Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let review = self
            .owned_review_with_context(operation.connection, context)?
            .ok_or(StoreError::RecoveryRequired)?;
        let key = review_key(&review)?;
        let mut cursor = self.review_cursor_with_context(key.connection, key.origin, context)?;
        let c = self.connection_with_context(ConnectionId(key.connection), context)?;
        let Some(Row::InitialReviewCompletedV1(complete)) =
            self.row_with_context(&format::key(15, key.connection, key.origin), context)?
        else {
            return Err(StoreError::Corrupt);
        };
        let digest = completion_digest(&complete)?;
        let lineage =
            match self.row_with_context(&format::key(18, key.connection, key.origin), context)? {
                Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
                None => LineageRef::Absent {},
                _ => return Err(StoreError::Corrupt),
            };
        require(
            cursor.state == Schedule::Running
                && cursor.current
                    == (Current::ReviewComplete {
                        completion_digest: digest,
                    })
                && cursor.progress_revision == expected_progress
                && cursor.schedule_revision == expected_schedule
                && cursor.control_sequence == control_sequence
                && provenance.version == 1
                && provenance.key == key
                && provenance.completion_digest == digest
                && provenance.operation.connection == operation.connection
                && provenance.operation.serial == operation.serial
                && operation.expected == c.revision
                && operation.expected == provenance.expected
                && c.pending.is_none()
                && operation.serial == format::next(c.serial)?
                && provenance.baseline == BaselineVersion::from_connection(&c)?
                && provenance.lineage == lineage
                && operation.directory_chain_ref.is_none()
                && operation.directory_chain_anchor.is_none()
                && operation.initial_review.is_none()
                && provenance.intent_digest == original_intent_digest(operation)?
                && format::encode(&original_intent(operation)?)? == format::encode(operation)?
                && provenance.admission_digest
                    == format::digest(
                        operation
                            .native_identity
                            .as_deref()
                            .ok_or(StoreError::Corrupt)?,
                    ),
        )?;
        validate_continuation_kind(operation, &provenance.kind)?;
        provenance.validate()?;
        let result = rows
            .iter()
            .find_map(|row| match row {
                Row::Connection(c) if c.id == key.connection => Some(c),
                _ => None,
            })
            .ok_or(StoreError::Corrupt)?;
        cursor.progress_revision = result.revision;
        cursor.current = Current::Continuation {
            operation: provenance.operation,
            provenance_digest: continuation_digest(provenance)?,
        };
        rows.push(Row::PostReviewOperationV1(provenance.clone()));
        rows.push(Row::InitialReviewCursor(cursor));
        Ok(())
    }
}
fn validate_continuation_kind(
    operation: &format::Operation,
    kind: &format::PostReviewKindV1,
) -> Result<(), StoreError> {
    match (&operation.action, kind) {
        (None, format::PostReviewKindV1::Ordinary {}) => Ok(()),
        (
            Some(action @ format::SelectedAction::Restore { .. }),
            format::PostReviewKindV1::SelectedRestore { action_digest },
        ) => require(*action_digest == format::digest(&format::encode(action)?)),
        _ => Err(StoreError::Corrupt),
    }
}

impl ConnectionStore {
    pub(super) fn read_review(
        &self,
        fact: &format::InitialChainReviewV1,
    ) -> Result<ReviewView, StoreError> {
        self.read_review_with_context(fact, None)
    }
    pub(super) fn validate_review_history(&self, replacements: &[Row]) -> Result<(), StoreError> {
        self.validate_review_history_with_context(replacements, None)
    }
    fn owned_review(
        &self,
        id: [u8; 16],
    ) -> Result<Option<format::InitialChainReviewV1>, StoreError> {
        self.owned_review_with_context(id, None)
    }
    fn review_cursor(
        &self,
        id: [u8; 16],
        origin: u64,
    ) -> Result<format::InitialReviewCursorV1, StoreError> {
        self.review_cursor_with_context(id, origin, None)
    }
}

struct CompletedEqualityView<'a> {
    operation: &'a format::Operation,
    result: &'a format::ReviewedBaselineResultV1,
}
impl ConnectionStore {
    fn validate_completed_equality_stop(
        &self,
        view: &ReviewView,
        link: &format::InitialReviewLinkV1,
        boundary: &format::InitialReviewStopBoundaryV1,
        completed: CompletedEqualityView<'_>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        let result = completed.result;
        let op = completed.operation;
        let w = &result.witness;
        require(
            op.stage == 4
                && op.revision == w.result_revision
                && w.operation == link.operation
                && w.precommit_revision.checked_add(1) == Some(op.revision)
                && matches!(w.receipt, format::CommitReceiptV1::EqualityVerified {})
                && matches!(
                    result.provenance,
                    format::ReviewedResultProvenanceV1::ReviewStep {
                        selected: format::ReviewedSelectionV1::Equality {},
                        ..
                    }
                )
                && boundary.fact().resulting_revision <= w.precommit_revision
                && (boundary.fact().stage != 0
                    || boundary.fact().resulting_revision < w.precommit_revision),
        )?;
        let mut historical = op.clone();
        historical.stage = 3;
        historical.revision = w.precommit_revision;
        let cursor = format::InitialReviewCursorV1 {
            version: 1,
            connection: result.key.connection,
            review: result.key.review,
            origin: result.key.origin,
            progress_revision: w.precommit_revision,
            schedule_revision: result.cut.schedule_revision,
            control_sequence: result.cut.control_sequence,
            state: Schedule::Running,
            next_step: link.step,
            current: link.current(),
        };
        self.validate_saved_equality_stop(view, link, &cursor, &historical, boundary, context)
    }
}

impl ConnectionStore {
    fn complete_review_retirement(
        &self,
        key: &ReviewKey,
        before: &format::ReviewCut,
        inner: &format::ReviewedInnerControlV1,
        baseline: &BaselineVersion,
        lineage: &LineageRef,
        rows: &mut Vec<Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<(), StoreError> {
        use format::DirectoryChainControlTransitionV1 as T;
        let review = self
            .owned_review_with_context(key.connection, context)?
            .ok_or(StoreError::Corrupt)?;
        require(*key == review_key(&review)?)?;
        let mut cursor = self.review_cursor_with_context(key.connection, key.origin, context)?;
        let c = self.connection_with_context(ConnectionId(key.connection), context)?;
        let old_head =
            match self.row_with_context(&format::key(18, key.connection, key.origin), context)? {
                Some(Row::ChainBaselineLineageV1(head)) => head.reference(),
                None => LineageRef::Absent {},
                _ => return Err(StoreError::Corrupt),
            };
        require(
            *before == format::ReviewCut::from_cursor(&cursor)?
                && *baseline == BaselineVersion::from_connection(&c)?
                && *lineage == old_head
                && inner.connection == key.connection
                && c.lifecycle == 0
                && c.fence == review.fence
                && before.progress_revision == c.revision,
        )?;
        let (step, entry, anchor, digest, cut) = match &cursor.current {
            Current::Chain {
                step,
                entry,
                anchor,
                digest,
                operation,
            } => (
                *step,
                *entry,
                *anchor,
                *digest,
                format::ReviewAbandonCut::Member {
                    operation: *operation,
                },
            ),
            Current::BetweenMembers {
                step,
                entry,
                anchor,
                digest,
                next_index,
                predecessor,
            } => (
                *step,
                *entry,
                *anchor,
                *digest,
                format::ReviewAbandonCut::BetweenMembers {
                    next_index: *next_index,
                    predecessor: *predecessor,
                },
            ),
            Current::AbandoningChain {
                step,
                entry,
                anchor,
                digest,
                cut,
            } => (*step, *entry, *anchor, *digest, cut.clone()),
            _ => return Err(StoreError::RecoveryRequired),
        };
        require(
            inner.anchor == anchor
                && inner.digest == digest
                && inner.expected_progress == cursor.progress_revision
                && format::next(cursor.schedule_revision)? < u64::MAX,
        )?;
        cursor.schedule_revision = format::next(cursor.schedule_revision)?;
        cursor.control_sequence = format::next(cursor.control_sequence)?;
        match inner.transition {
            T::AbandonRequested => {
                require(matches!(
                    cursor.state,
                    Schedule::Running | Schedule::Stopping { .. } | Schedule::Stopped { .. }
                ))?;
                cursor.state = Schedule::Abandoning;
                cursor.current = Current::AbandoningChain {
                    step,
                    entry,
                    anchor,
                    digest,
                    cut,
                };
                rows.push(Row::InitialReviewCursor(cursor));
            }
            T::Retire => {
                require(cursor.state == Schedule::Abandoning)?;
                let all = self.prospective_review_rows(rows, context)?;
                let anchor_op = row_operation(&all, anchor)?;
                let retirement = anchor_op
                    .directory_chain_retired
                    .clone()
                    .ok_or(StoreError::Corrupt)?;
                require(retirement.reason == format::ChainRetirementReason::Abandoned)?;
                let link = row_link(&all, anchor)?;
                let core = format::ArchiveCore {
                    version: 1,
                    key: key.clone(),
                    endpoints: review.endpoints.clone(),
                    fence: review.fence,
                    anchor,
                    digest,
                    link_digest: format::digest(&format::encode(link)?),
                    retirement,
                    baseline: baseline.clone(),
                    created: format::ArchiveCreatedV1::Retirement {
                        before: before.clone(),
                        inner: inner.clone(),
                        result: format::ReviewResultCutV1 {
                            progress_revision: cursor.progress_revision,
                            schedule_revision: cursor.schedule_revision,
                            control_sequence: cursor.control_sequence,
                        },
                    },
                };
                let core_digest = tagged_digest(b"Connected archive core v1", &core)?;
                self.install_review_archive(&review, None, Some(core), rows, context)?;
                let terminal = format::InitialReviewStepCompletedV1 {
                    version: 1,
                    key: key.clone(),
                    step,
                    entry,
                    entry_digest: link.entry_digest,
                    first_operation: anchor,
                    terminal: format::ReviewTerminalV1::Abandoned {
                        anchor,
                        core_digest,
                    },
                    resulting_progress: cursor.progress_revision,
                    schedule_revision: cursor.schedule_revision,
                    control_sequence: cursor.control_sequence,
                };
                cursor.next_step = step.checked_add(1).ok_or(StoreError::LimitExceeded)?;
                cursor.state = Schedule::Running;
                cursor.current = Current::BetweenSteps {
                    last_step: step,
                    completion_digest: terminal_digest(&terminal)?,
                };
                rows.push(Row::InitialReviewStepCompletedV1(terminal));
                let all = self.prospective_review_rows(rows, context)?;
                let view = self.read_review_with_context(&review, context)?;
                let progress = self.review_coverage(&view, &all, &cursor, context)?;
                if cursor.next_step == 4096 {
                    cursor.current = Current::StepLimit {
                        progress_digest: tagged_digest(
                            b"Connected review progress v1",
                            &Row::InitialReviewProgressV1(progress.clone()),
                        )?,
                    };
                }
                rows.push(Row::InitialReviewProgressV1(progress));
                rows.push(Row::InitialReviewCursor(cursor));
            }
            _ => return Err(StoreError::RecoveryRequired),
        }
        Ok(())
    }
}
fn current_matches_link(current: &Current, link: &format::InitialReviewLinkV1) -> bool {
    if *current == link.current() {
        return true;
    }
    match (current, &link.kind) {
        (
            Current::AbandoningChain {
                step,
                entry,
                anchor,
                digest,
                cut: format::ReviewAbandonCut::Member { operation },
            },
            LinkKind::Chain {
                anchor: a,
                digest: d,
                ..
            },
        ) => {
            *step == link.step
                && *entry == link.entry
                && anchor == a
                && digest == d
                && *operation == link.operation
        }
        _ => false,
    }
}

// The durable and legacy owners share this exact control-state join. Operation
// revision/pending handling stays at the caller, including disconnected cuts.
fn validate_current_chain_control(
    connection: &format::Connection,
    cursor: &format::InitialReviewCursorV1,
    anchor: format::ChainOperationIdV1,
    digest: [u8; 32],
    current: Option<format::ChainOperationIdV1>,
) -> Result<(), StoreError> {
    let inner = connection
        .directory_chain
        .as_ref()
        .ok_or(StoreError::Corrupt)?;
    let schedule = connection
        .directory_chain_schedule
        .as_ref()
        .ok_or(StoreError::Corrupt)?;
    require(
        inner.anchor == anchor
            && inner.digest == digest
            && inner.current == current
            && inner.progress_revision == cursor.progress_revision
            && schedule.state == cursor.state,
    )
}

// A checked local snapshot of the same prospective rows. Contiguous terminal
// keys make prefix_max <= expected exactly the old count == link.step predicate.
struct ReviewCutIndex<'a> {
    results: &'a BTreeMap<u64, &'a format::ReviewedBaselineResultV1>,
    prefix_progress: Vec<u64>,
}
impl ConnectionStore {
    fn review_result_index<'a>(
        &self,
        key: &ReviewKey,
        rows: &'a BTreeMap<Vec<u8>, Row>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<BTreeMap<u64, &'a format::ReviewedBaselineResultV1>, StoreError> {
        let mut results = BTreeMap::new();
        for row in rows.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            if let Row::ReviewedBaselineResultV1(result) = row {
                if result.key.connection != key.connection {
                    continue;
                }
                result.validate()?;
                require(result.key == *key)?;
                require(
                    results
                        .insert(result.witness.result_revision, result)
                        .is_none(),
                )?;
            }
        }
        Ok(results)
    }
    fn review_cut_index<'a>(
        &self,
        key: &ReviewKey,
        rows: &'a BTreeMap<Vec<u8>, Row>,
        results: &'a BTreeMap<u64, &'a format::ReviewedBaselineResultV1>,
        context: Option<&NativeReadContext<'_>>,
    ) -> Result<ReviewCutIndex<'a>, StoreError> {
        let mut steps = BTreeMap::new();
        for row in rows.values() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            if let Row::InitialReviewStepCompletedV1(step) = row {
                if step.key.connection != key.connection {
                    continue;
                }
                step.validate()?;
                require(
                    step.key == *key && steps.insert(step.step, step.resulting_progress).is_none(),
                )?;
                require(steps.len() <= 4096)?;
            }
        }
        let mut prefix_progress = vec![0];
        for (index, (step, progress)) in steps.into_iter().enumerate() {
            if let Some(context) = context {
                context.check()?;
            }
            self.storage.review_charge(0, 0, 0, 1)?;
            require(step as usize == index)?;
            prefix_progress.push(progress.max(*prefix_progress.last().ok_or(StoreError::Corrupt)?));
        }
        Ok(ReviewCutIndex {
            results,
            prefix_progress,
        })
    }
}
