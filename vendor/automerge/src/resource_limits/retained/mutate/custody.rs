//! One disposable native candidate, its parsed Changes, and inseparable work.
use super::*;
use crate::resource_limits::reconstruct::incoming::ParsedIncoming;
use crate::resource_limits::retained::owned::OwnedGrowth;
use crate::resource_limits::retained::{
    NativeRetention, Plan, add, retain_shared_plan,
    retention_layout_with_owned_credit,
};
use std::sync::Arc;

pub(super) struct PendingMutation {
    document: Option<NativeOwned<AutoCommit>>,
    changes: Vec<Change>,
    input_credit: NativeReservation,
    growth: Option<OwnedGrowth>,
    peak: NativeReservation,
}

impl PendingMutation {
    pub(super) fn new(
        document: NativeOwned<AutoCommit>,
        parsed: ParsedIncoming,
        peak: NativeReservation,
        turn: &NativeWorkTurn,
    ) -> Result<Self> {
        let (changes, input_credit) = parsed.into_parts();
        let mut pending = Self {
            document: Some(document),
            changes,
            input_credit,
            growth: None,
            peak,
        };
        pending.document().check_turn(turn)?;
        if !turn.accepts_reservation(&pending.input_credit)
            || !turn.accepts_reservation(&pending.peak)
        {
            return Err(NativeResourceError::Unaccounted);
        }
        pending.growth = Some(
            pending.document.as_ref().unwrap()._retention._owned
                .prepare_growth(&mut pending.peak)?,
        );
        Ok(pending)
    }

    pub(super) fn document(&self) -> &NativeOwned<AutoCommit> {
        self.document.as_ref().unwrap()
    }

    /// Only the separately planned/validated orchestration may call this.
    /// The old native value and every incoming owner remain funded on error.
    pub(super) fn apply(&mut self) -> std::result::Result<(), NativeMutationError> {
        let changes = std::mem::take(&mut self.changes);
        self.document.as_mut().unwrap().value.doc
            .apply_changes_batch_log_patches(changes, &mut crate::PatchLog::native_discard())
            .map_err(|_| NativeMutationError::InvalidOperation)
    }

    pub(super) fn finish(
        mut self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeOwned<AutoCommit>> {
        allocations.check_turn(turn)?;
        self.document().check_turn(turn)?;
        if !self.changes.is_empty()
            || !Arc::ptr_eq(&self.peak.ledger, &self.input_credit.ledger)
            || !turn.accepts_reservation(&self.peak)
        {
            return Err(NativeResourceError::Unaccounted);
        }
        // Parsed raw owners have either died or moved into this exact native
        // queue. Reassign their same-ledger credit without release/reacquire;
        // unique retained growth need not reserve those bytes a second time.
        let combined = add(self.peak.bytes, self.input_credit.bytes)?;
        self.peak.bytes = combined;
        self.input_credit.bytes = 0;

        let value = &self.document.as_ref().unwrap().value;
        let plan = Plan::inspect_with(value, |bytes| {
            if bytes > self.peak.bytes() {
                return Err(NativeResourceError::WorkBytes);
            }
            Ok(split(&mut self.peak, bytes))
        })?;
        let old_owned = self.document()._retention._owned.bytes();
        let layout = retention_layout_with_owned_credit(&plan, allocations, old_owned)?;
        let (domain, shared) =
            retain_shared_plan(&plan, allocations, turn, &mut self.peak, layout)?;
        self.document.as_mut().unwrap()._retention._owned.settle(
            plan.owned,
            &mut self.peak,
            self.growth.as_mut().unwrap(),
        )?;

        // No fallible operation follows. The sole owned-credit holder moves
        // with the sole candidate value; another separately owned clone never
        // contributes its credits. New shared leases exist before old ones die.
        let document = self.document.take().unwrap();
        let NativeRetention {
            _leases: old_leases,
            _owned: owned,
            _metadata: old_metadata,
        } = document._retention;
        let result = NativeOwned {
            value: document.value,
            domain,
            _retention: NativeRetention {
                _leases: shared.leases,
                _owned: owned,
                _metadata: shared.metadata,
            },
        };
        drop(document.domain);
        drop(old_leases);
        drop(old_metadata);
        drop(plan);
        allocations.reconcile();
        Ok(result)
    }
}
