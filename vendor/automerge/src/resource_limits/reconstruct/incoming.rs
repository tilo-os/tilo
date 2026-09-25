//! Incoming-only reconstruction custody. This permit never authorizes applying
//! changes to retained state; that requires a separate source-specific plan.
use super::*;
use crate::resource_limits::NativeReservation;
use crate::{Change, TextEncoding};

pub(crate) struct ParsedIncoming {
    changes: Vec<Change>,
    reservation: NativeReservation,
}

impl ParsedIncoming {
    pub(crate) fn parse(input: NormalizedStorage<'_>, turn: &NativeWorkTurn) -> Result<Self> {
        if !turn.accepts_reservation(&input._reservation)
            || cfg!(feature = "slow_path_assertions")
            || hexane::diagnostic_exports_enabled()
        {
            return Err(NativeResourceError::Unaccounted);
        }
        let _scratch = turn.reserve(sum(size_of::<Shape>(), product(2, size_of::<Group>())?)?)?;
        let shape = Shape::read(&input)?;
        let plan = shape.incoming_plan()?;
        let peak = turn.reserve(plan.bytes())?;
        let permit = Permit {
            _peak: &peak,
            clock_cache_nodes: 0,
        };
        let changes = crate::storage::load::load_all_changes_bounded(
            crate::storage::parse::Input::new(&input.bytes),
            TextEncoding::platform_default(),
            &permit,
        )?;
        // Parsing and every per-chunk collector have returned. Canonical
        // Changes are static owning values, so input may now die independently.
        let (changes, reservation) =
            crate::resource_limits::retained::mutate::retain_incoming(changes, peak)?;
        Ok(Self {
            changes,
            reservation,
        })
    }

    /// The same strict chunk, column, canonical-change and derived-heads
    /// validation as [`Self::parse`], bounded by the inspected input limits
    /// instead of a worst-case layout reservation. Ordinary-document admission
    /// only: the result carries no retained allocation credit.
    pub(crate) fn parse_limited(input: NormalizedStorage<'_>, turn: &NativeWorkTurn) -> Result<Vec<Change>> {
        if !turn.accepts_reservation(&input._reservation) {
            return Err(NativeResourceError::Unaccounted);
        }
        let marker = turn.reserve(0)?;
        let permit = Permit {
            _peak: &marker,
            clock_cache_nodes: usize::MAX,
        };
        crate::storage::load::load_all_changes_bounded(
            crate::storage::parse::Input::new(&input.bytes),
            TextEncoding::platform_default(),
            &permit,
        )
    }

    pub(crate) fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Private consuming move into the separately charged batch executor. The
    /// caller must keep the returned guard until batch/adoption temporaries die.
    pub(crate) fn into_parts(self) -> (Vec<Change>, NativeReservation) {
        (self.changes, self.reservation)
    }
}

impl Shape {
    fn incoming_plan(&self) -> Result<Budget> {
        use crate::columnar::column_range::generic::GroupedColumnRange;
        use crate::storage::columns::{RawColumn, compression::Uncompressed};
        let mut budget = Budget::default();
        if self.chunks == 0 {
            return Ok(budget);
        }
        // Standalone Change metadata remains in earlier output entries while
        // the next chunk parses. Independent prefix vectors retain minima.
        budget.growing_vec_forest::<crate::ActorId>(self.actors, self.chunks)?;
        budget.add(self.actor_bytes)?;
        budget.growing_vec_forest::<crate::ChangeHash>(self.heads, self.chunks)?;
        budget.growing_vec::<u64>(self.heads)?;
        // Native parse, validation and decompression keep up to three column
        // descriptor owners live. These are concrete descriptor forests.
        for _ in 0..3 {
            budget.growing_vec_forest::<RawColumn<Uncompressed>>(
                self.columns,
                product(self.chunks, 2)?,
            )?;
        }
        budget.growing_vec::<crate::storage::columns::Column>(self.columns)?;
        budget.growing_vec::<GroupedColumnRange>(self.columns)?;
        budget.array::<GroupedColumnRange>(product(self.columns, 4)?)?;
        budget.growing_vec_forest::<u8>(self.raw_change_bytes, self.chunks)?;
        budget.add(self.values)?;
        budget.growing_vec::<u8>(11)?; // checksum type and u64 length prefix
        if self.documents != 0 {
            // This owner's concrete load+index layout also covers the load-only
            // subset used by reconstruct_changes_bounded; no retained-source
            // mutation or actor migration is performed by this parse phase.
            crate::op_set2::OpSet::native_reconstruction_plan(self, &mut budget)?;
            crate::change_graph::ChangeGraphCols::native_incoming_columns_plan(self, &mut budget)?;
        }
        if self.documents != 0 || self.bundles != 0 {
            crate::op_set2::change::ChangeCollector::native_reconstruction_plan(self, &mut budget)?;
        }
        // Stream output can coexist with a complete current Document/Bundle
        // collector result while extend moves that result into the outer Vec.
        budget.growing_vec::<Change>(self.changes)?;
        Ok(budget)
    }
}
