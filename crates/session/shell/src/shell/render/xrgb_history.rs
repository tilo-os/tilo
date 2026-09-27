//! Bounded destination-age tracking for the opt-in DRM XRGB partial-write path.
//!
//! This deliberately remembers only revisions and native damage rectangles. It never owns pixel
//! storage: the bridge's settled staging buffer remains the sole source for a partial swizzle.

use vello::FineDamageRect;

const MAX_TARGETS: usize = 2;
const MAX_RECTS: usize = 64;

/// Identity of an XRGB destination contract.
///
/// This is supplied by the bridge and binds a history to one renderer generation, its content
/// epoch, the physical extent, row pitch, and the number of tracked destinations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct XrgbHistoryIdentity {
    pub(crate) bridge_generation: u64,
    pub(crate) bridge_epoch: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pitch: usize,
    pub(crate) targets: usize,
    pub(crate) scanout_epoch: u64,
}

impl XrgbHistoryIdentity {
    #[must_use]
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn with_scanout_epoch(mut self, scanout_epoch: u64) -> Self {
        self.scanout_epoch = scanout_epoch;
        self
    }
}

#[derive(Clone, Debug, Default)]
struct TargetState {
    valid: bool,
    pending: Vec<FineDamageRect>,
}

/// Bounded history for one or two caller-owned XRGB destinations.
///
/// It is intentionally reusable by the single-target benchmark and the double-buffered DRM
/// scanout. There is no pointer identity shortcut: callers must invalidate whenever any other
/// writer can have changed a tracked destination.
#[derive(Debug)]
#[doc(hidden)]
pub struct XrgbTargetHistory {
    targets: [TargetState; MAX_TARGETS],
    active_targets: usize,
    identity: Option<XrgbHistoryIdentity>,
    last_revision: Option<u64>,
    pending_delta: Vec<FineDamageRect>,
    scratch_plan: Vec<FineDamageRect>,
}

/// A staged output update. It is committed only after the destination mapping, bridge write, and
/// any software-cursor write have all succeeded.
#[doc(hidden)]
pub struct XrgbHistoryTransaction<'a> {
    history: &'a mut XrgbTargetHistory,
    target: usize,
    revision: u64,
    previous_revision: Option<u64>,
    prepared_regions: Option<&'a [FineDamageRect]>,
    identity: Option<XrgbHistoryIdentity>,
    source_proven: bool,
    partial_plan: bool,
    committed: bool,
}

/// The bridge-only output write plan produced by a staged transaction.
#[derive(Clone, Copy, Debug)]
pub(crate) enum XrgbWritePlan<'a> {
    Full,
    Rects(&'a [FineDamageRect]),
}

/// Whether the bridge accepted the frame delta as a retained-damage source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum XrgbHistoryCommit {
    /// The bridge completed with a current staging image and accepted the input damage chain.
    DamageAccepted,
    /// The bridge completed a dense/full boundary, so no other target may retain its old chain.
    FullBoundary,
}

impl XrgbTargetHistory {
    /// Construct a bounded history for one benchmark destination or two DRM dumb buffers.
    #[must_use]
    pub fn new(targets: usize) -> Self {
        let active_targets = targets.clamp(1, MAX_TARGETS);
        let target_state = || TargetState {
            valid: false,
            pending: Vec::with_capacity(MAX_RECTS),
        };
        Self {
            targets: [target_state(), target_state()],
            active_targets,
            identity: None,
            last_revision: None,
            pending_delta: Vec::with_capacity(MAX_RECTS),
            scratch_plan: Vec::with_capacity(MAX_RECTS),
        }
    }

    /// Explicitly discard every destination claim after an external write, configuration change,
    /// failed bridge call, or presentation failure.
    pub fn invalidate(&mut self) {
        self.identity = None;
        self.last_revision = None;
        self.pending_delta.clear();
        self.scratch_plan.clear();
        for target in self.targets.iter_mut().take(self.active_targets) {
            target.valid = false;
            target.pending.clear();
        }
    }

    /// Stage one scene delta and any destination-only restoration rectangles, such as the old
    /// software cursor footprint. No destination validity or revision claim advances until
    /// [`XrgbHistoryTransaction::commit`]; dropping or aborting the transaction invalidates the
    /// staged age chain.
    pub fn prepare<'a>(
        &'a mut self,
        target: usize,
        identity: Option<XrgbHistoryIdentity>,
        revision: u64,
        previous_revision: Option<u64>,
        regions: Option<&'a [FineDamageRect]>,
        extra_rects: &[FineDamageRect],
    ) -> XrgbHistoryTransaction<'a> {
        if target >= self.active_targets {
            self.invalidate();
        }
        self.pending_delta.clear();
        self.scratch_plan.clear();

        let identity_matches = identity.is_some_and(|identity| {
            identity.targets == self.active_targets && self.identity == Some(identity)
        });
        let delta_valid = identity.is_some()
            && previous_revision == self.last_revision
            && Self::normalize_into(&mut self.pending_delta, regions, identity);
        let source_proven = identity_matches && delta_valid;

        let target_valid = self.targets.get(target).is_some_and(|state| state.valid);
        let plan_partial =
            source_proven && target_valid && self.extend_plan(target, extra_rects, identity);

        XrgbHistoryTransaction {
            history: self,
            target,
            revision,
            previous_revision,
            prepared_regions: regions,
            identity,
            source_proven,
            partial_plan: source_proven && plan_partial,
            committed: false,
        }
    }

    fn commit_staged(
        &mut self,
        target_index: usize,
        revision: u64,
        identity: Option<XrgbHistoryIdentity>,
        source_proven: bool,
        outcome: XrgbHistoryCommit,
    ) {
        let Some(identity) = identity else {
            self.invalidate();
            return;
        };
        let preserve_other_targets = source_proven
            && outcome == XrgbHistoryCommit::DamageAccepted
            && self.identity == Some(identity);
        if preserve_other_targets {
            for (index, target) in self
                .targets
                .iter_mut()
                .enumerate()
                .take(self.active_targets)
            {
                if index != target_index
                    && target.valid
                    && !Self::append_rects(&mut target.pending, &self.pending_delta, identity)
                {
                    target.valid = false;
                    target.pending.clear();
                }
            }
        } else {
            self.invalidate();
            self.identity = Some(identity);
        }
        if let Some(target) = self.targets.get_mut(target_index) {
            target.valid = true;
            target.pending.clear();
        }
        self.identity = Some(identity);
        self.last_revision = Some(revision);
        self.pending_delta.clear();
        self.scratch_plan.clear();
    }

    fn extend_plan(
        &mut self,
        target: usize,
        extra_rects: &[FineDamageRect],
        identity: Option<XrgbHistoryIdentity>,
    ) -> bool {
        let Some(identity) = identity else {
            return false;
        };
        let Some(state) = self.targets.get(target) else {
            return false;
        };
        if !Self::append_rects(&mut self.scratch_plan, &state.pending, identity)
            || !Self::append_rects(&mut self.scratch_plan, &self.pending_delta, identity)
            || !Self::append_rects(&mut self.scratch_plan, extra_rects, identity)
        {
            self.scratch_plan.clear();
            return false;
        }
        !self.scratch_plan.is_empty()
    }

    fn normalize_into(
        output: &mut Vec<FineDamageRect>,
        regions: Option<&[FineDamageRect]>,
        identity: Option<XrgbHistoryIdentity>,
    ) -> bool {
        let (Some(regions), Some(identity)) = (regions, identity) else {
            return false;
        };
        if regions.is_empty() || regions.len() > MAX_RECTS {
            return false;
        }
        Self::append_rects(output, regions, identity)
    }

    fn append_rects(
        output: &mut Vec<FineDamageRect>,
        rects: &[FineDamageRect],
        identity: XrgbHistoryIdentity,
    ) -> bool {
        if rects.len() > MAX_RECTS {
            return false;
        }
        for &rect in rects {
            if !Self::valid_rect(rect, identity) {
                return false;
            }
            let mut contained = false;
            let mut index = 0;
            while let Some(existing) = output.get(index).copied() {
                if Self::contains(existing, rect) {
                    contained = true;
                    break;
                }
                if Self::contains(rect, existing) {
                    output.swap_remove(index);
                } else {
                    index = index.saturating_add(1);
                }
            }
            if !contained {
                if output.len() >= MAX_RECTS {
                    return false;
                }
                output.push(rect);
            }
        }
        let dirty = output.iter().fold(0_u64, |total, rect| {
            total.saturating_add(u64::from(rect.width).saturating_mul(u64::from(rect.height)))
        });
        let full = u64::from(identity.width).saturating_mul(u64::from(identity.height));
        full != 0 && dirty.saturating_mul(2) < full
    }

    fn valid_rect(rect: FineDamageRect, identity: XrgbHistoryIdentity) -> bool {
        rect.width > 0
            && rect.height > 0
            && rect.x < identity.width
            && rect.y < identity.height
            && rect
                .x
                .checked_add(rect.width)
                .is_some_and(|right| right <= identity.width)
            && rect
                .y
                .checked_add(rect.height)
                .is_some_and(|bottom| bottom <= identity.height)
    }

    fn contains(outer: FineDamageRect, inner: FineDamageRect) -> bool {
        let outer_right = outer.x.saturating_add(outer.width);
        let outer_bottom = outer.y.saturating_add(outer.height);
        let inner_right = inner.x.saturating_add(inner.width);
        let inner_bottom = inner.y.saturating_add(inner.height);
        outer.x <= inner.x
            && outer.y <= inner.y
            && outer_right >= inner_right
            && outer_bottom >= inner_bottom
    }
}

impl XrgbHistoryTransaction<'_> {
    pub(crate) fn write_plan(&self) -> XrgbWritePlan<'_> {
        if self.partial_plan {
            XrgbWritePlan::Rects(&self.history.scratch_plan)
        } else {
            XrgbWritePlan::Full
        }
    }

    pub(crate) fn prepared_identity(&self) -> Option<XrgbHistoryIdentity> {
        self.identity
    }

    pub(crate) fn matches_frame(
        &self,
        revision: u64,
        previous_revision: Option<u64>,
        regions: Option<&[FineDamageRect]>,
    ) -> bool {
        revision == self.revision
            && previous_revision == self.previous_revision
            && Self::prepared_regions_match(self.prepared_regions, regions)
    }

    fn prepared_regions_match(
        prepared: Option<&[FineDamageRect]>,
        actual: Option<&[FineDamageRect]>,
    ) -> bool {
        match (prepared, actual) {
            (None, None) => true,
            (Some(prepared), Some(actual)) if prepared.len() <= MAX_RECTS => {
                actual.len() <= MAX_RECTS && prepared == actual
            }
            // Oversized lists always force a full output plan, but the transaction still needs
            // to permit its own original frame through to that conservative fallback. It holds
            // the caller's immutable slice for its lifetime, so pointer+length is a bounded
            // same-input check here, not a destination-identity claim.
            (Some(prepared), Some(actual)) => {
                prepared.len() == actual.len() && std::ptr::eq(prepared.as_ptr(), actual.as_ptr())
            }
            _ => false,
        }
    }

    /// Commit only after the destination bytes and any software cursor have been written.
    pub fn commit(mut self, outcome: XrgbHistoryCommit) {
        self.history.commit_staged(
            self.target,
            self.revision,
            self.identity,
            self.source_proven,
            outcome,
        );
        self.committed = true;
    }

    /// Explicitly discard the entire age chain after a failed bridge/map/presentation operation.
    pub fn abort(mut self) {
        self.history.invalidate();
        self.committed = true;
    }
}

impl Drop for XrgbHistoryTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.history.invalidate();
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the bounded history fixtures use fixed, in-bounds test coordinates and dimensions"
)]
mod tests {
    use super::*;

    fn identity(targets: usize) -> XrgbHistoryIdentity {
        XrgbHistoryIdentity {
            bridge_generation: 11,
            bridge_epoch: 7,
            width: 64,
            height: 48,
            pitch: 64 * 4 + 16,
            targets,
            scanout_epoch: 3,
        }
    }

    fn rect(x: u32, y: u32) -> FineDamageRect {
        FineDamageRect {
            x,
            y,
            width: 4,
            height: 4,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn xrgb_history_reuses_a_delta_only_for_the_proven_target() {
        // Phase `transaction_binds_the_exact_prepared_delta_without_rejecting_a_gap`.
        {
            let mut history = XrgbTargetHistory::new(1);
            let regions = [rect(2, 3)];
            let transaction =
                history.prepare(0, Some(identity(1)), 9, Some(4), Some(&regions), &[]);
            assert!(transaction.matches_frame(9, Some(4), Some(&regions)));
            assert!(
                !transaction.matches_frame(9, Some(8), Some(&regions)),
                "actual previous revision must be the one prepared, not a mutable history value"
            );
            assert!(
                !transaction.matches_frame(10, Some(4), Some(&regions)),
                "revision changes invalidate the staged frame"
            );
            let different = [rect(3, 3)];
            assert!(
                !transaction.matches_frame(9, Some(4), Some(&different)),
                "regions are an exact bounded frame-input claim"
            );
            transaction.abort();
        }

        // Phase `accepted_delta_is_partial_only_for_a_proven_target_and_identity`.
        {
            let mut history = XrgbTargetHistory::new(1);
            let regions = [rect(2, 3)];
            let transaction = history.prepare(0, Some(identity(1)), 1, None, Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            let transaction =
                history.prepare(0, Some(identity(1)), 2, Some(1), Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Rects(_)));
            transaction.commit(XrgbHistoryCommit::DamageAccepted);

            let changed_identity = XrgbHistoryIdentity {
                bridge_epoch: 8,
                ..identity(1)
            };
            let transaction =
                history.prepare(0, Some(changed_identity), 3, Some(2), Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();
        }

        // Phase `two_targets_accumulate_intervening_delta_before_partial_reuse`.
        {
            let mut history = XrgbTargetHistory::new(2);
            let first = [rect(2, 3)];
            let second = [rect(10, 11)];
            let transaction = history.prepare(0, Some(identity(2)), 1, None, Some(&first), &[]);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            let transaction = history.prepare(0, Some(identity(2)), 2, Some(1), Some(&second), &[]);
            transaction.commit(XrgbHistoryCommit::DamageAccepted);

            // Target one has no bytes yet, so it must be filled completely. Its accepted commit
            // carries the missed delta onto target zero's pending list.
            let transaction = history.prepare(1, Some(identity(2)), 3, Some(2), Some(&first), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.commit(XrgbHistoryCommit::DamageAccepted);

            let transaction = history.prepare(0, Some(identity(2)), 4, Some(3), Some(&second), &[]);
            let XrgbWritePlan::Rects(plan) = transaction.write_plan() else {
                panic!("a valid target must repair its accumulated missed deltas");
            };
            assert!(
                plan.len() >= 2,
                "pending + current delta remain separate exact rects"
            );
            transaction.abort();
        }

        // Phase `uncommitted_transaction_invalidates_instead_of_replaying`.
        {
            let mut history = XrgbTargetHistory::new(1);
            let regions = [rect(2, 3)];
            {
                let transaction =
                    history.prepare(0, Some(identity(1)), 1, None, Some(&regions), &[]);
                transaction.commit(XrgbHistoryCommit::FullBoundary);
            }
            {
                let transaction =
                    history.prepare(0, Some(identity(1)), 2, Some(1), Some(&regions), &[]);
                assert!(matches!(transaction.write_plan(), XrgbWritePlan::Rects(_)));
                // Drop without committing: RAII clears the chain.
            }
            let transaction =
                history.prepare(0, Some(identity(1)), 3, Some(2), Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();
        }

        // Phase `accumulated_other_target_rectangles_cap_and_invalidate_that_target`.
        {
            let mut history = XrgbTargetHistory::new(2);
            let initial = [rect(2, 3)];
            let transaction = history.prepare(0, Some(identity(2)), 1, None, Some(&initial), &[]);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            // The first 64 in-bounds deltas remain representable for target zero. The 65th must
            // invalidate it rather than truncating a destination-age repair plan.
            for revision in 2_u64..=65 {
                let index = u32::try_from(revision - 2).expect("bounded test index");
                let regions = [FineDamageRect {
                    x: index % 32,
                    y: 20_u32.checked_add(index / 32).expect("bounded test row"),
                    width: 1,
                    height: 1,
                }];
                let transaction = history.prepare(
                    1,
                    Some(identity(2)),
                    revision,
                    Some(revision - 1),
                    Some(&regions),
                    &[],
                );
                transaction.commit(XrgbHistoryCommit::DamageAccepted);
            }
            assert!(
                history.targets[0].valid,
                "64 pending rects retain the stale target"
            );
            assert_eq!(history.targets[0].pending.len(), MAX_RECTS);

            let regions = [FineDamageRect {
                x: 0,
                y: 22,
                width: 1,
                height: 1,
            }];
            let transaction =
                history.prepare(1, Some(identity(2)), 66, Some(65), Some(&regions), &[]);
            transaction.commit(XrgbHistoryCommit::DamageAccepted);
            assert!(
                !history.targets[0].valid,
                "the 65th in-bounds accumulated delta invalidates rather than truncates"
            );

            let next = [rect(2, 3)];
            let transaction = history.prepare(0, Some(identity(2)), 67, Some(66), Some(&next), &[]);
            assert!(
                matches!(transaction.write_plan(), XrgbWritePlan::Full),
                "the 65th accumulated delta invalidates the stale target instead of truncating it"
            );
            transaction.abort();
        }
    }

    #[test]
    fn xrgb_history_turns_oversized_invalid_and_epoch_changes_into_full_boundaries() {
        // Phase `oversized_regions_match_then_choose_a_full_boundary`.
        {
            let mut history = XrgbTargetHistory::new(1);
            let regions = vec![rect(2, 3); MAX_RECTS + 1];
            let transaction = history.prepare(0, Some(identity(1)), 1, None, Some(&regions), &[]);
            assert!(transaction.matches_frame(1, None, Some(&regions)));
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();
        }

        // Phase `oversized_destination_only_rects_are_rejected_without_scanning`.
        {
            let mut history = XrgbTargetHistory::new(1);
            let seed = [rect(2, 3)];
            let transaction = history.prepare(0, Some(identity(1)), 1, None, Some(&seed), &[]);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            let extras = vec![rect(4, 5); MAX_RECTS + 1];
            let transaction =
                history.prepare(0, Some(identity(1)), 2, Some(1), Some(&seed), &extras);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();
        }

        // Phase `layout_scanout_epoch_and_invalid_target_are_full_boundaries`.
        {
            let mut history = XrgbTargetHistory::new(2);
            let regions = [rect(2, 3)];
            let transaction = history.prepare(0, Some(identity(2)), 1, None, Some(&regions), &[]);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            let changed_layout = XrgbHistoryIdentity {
                pitch: identity(2).pitch.saturating_add(16),
                ..identity(2)
            };
            let transaction =
                history.prepare(0, Some(changed_layout), 2, Some(1), Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();

            let transaction = history.prepare(0, Some(identity(2)), 1, None, Some(&regions), &[]);
            transaction.commit(XrgbHistoryCommit::FullBoundary);
            let transaction = history.prepare(
                0,
                Some(identity(2).with_scanout_epoch(4)),
                2,
                Some(1),
                Some(&regions),
                &[],
            );
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();

            let transaction =
                history.prepare(9, Some(identity(2)), 2, Some(1), Some(&regions), &[]);
            assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
            transaction.abort();
        }

        // Phase `empty_large_and_invalid_delta_inputs_are_full_boundaries`.
        {
            let cases = [
                None,
                Some(Vec::new()),
                Some(vec![FineDamageRect {
                    x: 63,
                    y: 0,
                    width: 2,
                    height: 1,
                }]),
                Some(vec![FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 48,
                }]),
            ];
            for regions in cases {
                let mut history = XrgbTargetHistory::new(1);
                let seed = [rect(2, 3)];
                let transaction = history.prepare(0, Some(identity(1)), 1, None, Some(&seed), &[]);
                transaction.commit(XrgbHistoryCommit::FullBoundary);
                let transaction =
                    history.prepare(0, Some(identity(1)), 2, Some(1), regions.as_deref(), &[]);
                assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
                transaction.abort();
            }
        }
    }
}
