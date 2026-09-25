//! Reconstruction accepts only the checked, normalized storage witness.
pub(crate) mod incoming;
use super::inspect::{Column, Reader, MAX_COLUMNS};
use super::{
    NativeAllocationSet, NativeOwned, NativeResourceError, NativeWorkTurn, NormalizedStorage,
};
use crate::AutoCommit;
use std::mem::size_of;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

/// Checked arithmetic for concrete requested-allocation layouts. Each owner
/// contributes its private collection types; this is never an input ratio.
#[derive(Default)]
pub(crate) struct Budget {
    bytes: usize,
    pub(crate) backings: usize,
    pub(crate) clock_cache_nodes: usize,
}

impl Budget {
    pub(crate) fn add(&mut self, bytes: usize) -> Result {
        self.bytes = sum(self.bytes, bytes)?;
        Ok(())
    }

    pub(crate) fn array<T>(&mut self, count: usize) -> Result {
        self.add(product(count, size_of::<T>())?)
    }

    /// Pinned RawVec amortized growth is max(old*2, required, MIN_NON_ZERO_CAP).
    /// New capacity is at most max(2*final_len, MIN_NON_ZERO_CAP); while it is
    /// reallocated the old backing is at most final_len. This covers both
    /// allocations simultaneously, including a minimum allocation for a tiny
    /// nonempty vector. Exact-size constructors fit within the same bound.
    pub(crate) fn growing_vec<T>(&mut self, count: usize) -> Result {
        if count == 0 || size_of::<T>() == 0 {
            return Ok(());
        }
        let minimum = if size_of::<T>() == 1 {
            8
        } else if size_of::<T>() <= 1024 {
            4
        } else {
            1
        };
        self.array::<T>(sum(product(count, 2)?.max(minimum), count)?)
    }

    /// Independent RawVecs retain independent minimum allocations. Summing
    /// one vector's geometric bound loses those minima when most vectors are
    /// short. At most min(elements, vectors) of this forest is nonempty.
    pub(crate) fn growing_vec_forest<T>(&mut self, elements: usize, vectors: usize) -> Result {
        self.growing_vec::<T>(elements)?;
        let minimum = if size_of::<T>() == 1 {
            8
        } else if size_of::<T>() <= 1024 {
            4
        } else {
            1
        };
        self.array::<T>(product(minimum, elements.min(vectors))?)
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Both the old and replacement table may be live during RawTable growth.
    pub(crate) fn hash<K, V>(&mut self, entries: usize) -> Result {
        let table = insertion_table_bytes::<(K, V)>(entries)?;
        self.add(table)?;
        self.add(table)
    }
    pub(crate) fn hash_many<K, V>(&mut self, tables: usize, entries: usize) -> Result {
        let table = insertion_table_bytes::<(K, V)>(entries)?;
        self.add(product(product(tables, 2)?, table)?)
    }

    pub(crate) fn tree<K, V>(&mut self, entries: usize) -> Result {
        self.add(super::retained::btree_bytes::<K, V>(entries)?)
    }
    pub(crate) fn trees<K, V>(&mut self, trees: usize, entries: usize) -> Result {
        self.add(product(
            trees,
            super::retained::btree_bytes::<K, V>(entries)?,
        )?)
    }
    pub(crate) fn arc<T>(&mut self, count: usize) -> Result {
        self.add(product(count, super::retained::arc_bytes::<T>()?)?)
    }

    /// Sum a forest of columns without rounding each caller's cardinalities
    /// down. One combined event/byte plan plus a complete empty-column plan per
    /// column covers independent vector minima, partial slabs and tree roots.
    /// Owners separately include nested value/weight allocations.
    pub(crate) fn columns<C: hexane::ColumnCursor>(
        &mut self,
        columns: usize,
        events: usize,
        encoded_bytes: usize,
    ) -> Result<usize> {
        if columns == 0 {
            return Ok(0);
        }
        let combined = hexane::column_construction_allocation::<C>(events, encoded_bytes)
            .ok_or(NativeResourceError::WorkBytes)?;
        let empty = hexane::column_construction_allocation::<C>(1, 20)
            .ok_or(NativeResourceError::WorkBytes)?;
        self.add(combined.bytes)?;
        self.add(product(columns, empty.bytes)?)?;
        self.backings = sum(
            self.backings,
            sum(combined.backings, product(columns, empty.backings)?)?,
        )?;
        sum(combined.weight_nodes, product(columns, empty.weight_nodes)?)
    }

    pub(crate) fn raw_columns(&mut self, columns: usize, bytes: usize) -> Result {
        if columns == 0 {
            return Ok(());
        }
        let empty =
            hexane::raw_column_construction_allocation(0).ok_or(NativeResourceError::WorkBytes)?;
        self.add(bytes)?;
        self.add(product(columns, empty.bytes)?)?;
        self.backings = sum(self.backings, columns)?;
        Ok(())
    }
}

/// Convert insertion count to reported capacity using pinned hashbrown
/// 0.17.1 capacity_to_buckets, before applying the retained-layout helper.
/// A 16-byte control group bounds all enabled targets, including its larger
/// minimum table for one-/two-/three-byte entries. Four entries need eight
/// buckets, not the four buckets implied by treating four as reported capacity.
fn insertion_table_bytes<T>(entries: usize) -> Result<usize> {
    if entries == 0 {
        return Ok(0);
    }
    let minimum = match size_of::<T>() {
        0..=1 => 14,
        2..=3 => 7,
        _ => 3,
    };
    let capacity = entries.max(minimum);
    let buckets = if capacity < 4 {
        4
    } else if capacity < 8 {
        8
    } else if capacity < 15 {
        16
    } else {
        (product(capacity, 8)? / 7)
            .checked_next_power_of_two()
            .ok_or(NativeResourceError::WorkBytes)?
    };
    let reported = if buckets < 8 {
        buckets - 1
    } else {
        product(buckets / 8, 7)?
    };
    super::retained::table_bytes::<T>(reported)
}

pub(crate) fn sum(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(NativeResourceError::WorkBytes)
}
pub(crate) fn product(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(NativeResourceError::WorkBytes)
}

/// Cardinalities come from the private inspection witness. Additional byte
/// totals below are measured by a borrowed pass over its normalized storage.
/// Overestimates intentionally include every chunk even if native deduplication
/// later discards one. They do not invent additional accepted input forms.
pub(crate) struct Shape {
    pub(crate) operations: usize,
    pub(crate) dense: usize,
    pub(crate) changes: usize,
    pub(crate) edges: usize,
    pub(crate) actors: usize,
    pub(crate) heads: usize,
    pub(crate) values: usize,
    pub(crate) actor_bytes: usize,
    pub(crate) columns: usize,
    pub(crate) column_bytes: usize,
    pub(crate) column_rows: usize,
    pub(crate) column_events: usize,
    pub(crate) raw_column_bytes: usize,
    pub(crate) marks: usize,
    pub(crate) raw_change_bytes: usize,
    pub(crate) extra_bytes: usize,
    pub(crate) chunks: usize,
    pub(crate) documents: usize,
    pub(crate) bundles: usize,
}

impl Shape {
    fn plan(&self) -> Result<Budget> {
        use crate::columnar::column_range::generic::GroupedColumnRange;
        use crate::storage::columns::{compression::Uncompressed, RawColumn};
        let mut budget = Budget::default();
        // Native prefix parse owns actor/hash/column metadata independently of
        // the normalized byte loan. Uncompressed RawColumns are cloned by
        // compression::decompress and again for the OpSet column map.
        budget.growing_vec::<crate::ActorId>(self.actors)?;
        budget.add(self.actor_bytes)?;
        budget.growing_vec::<crate::ChangeHash>(self.heads)?;
        budget.growing_vec::<u64>(self.heads)?; // optional document head indexes
        for _ in 0..3 {
            budget.growing_vec::<RawColumn<Uncompressed>>(self.columns)?;
        }
        budget.growing_vec::<crate::storage::columns::Column>(self.columns)?;
        budget.growing_vec::<GroupedColumnRange>(self.columns)?;
        budget.array::<GroupedColumnRange>(product(self.columns, 4)?)?;
        budget.growing_vec::<u8>(self.raw_change_bytes)?; // into_owned change/bundle storage
        budget.add(self.values)?; // parsed messages and current decoded value
        budget.add(16)?; // detached Automerge's fresh random ActorId
        budget.array::<u8>(product(self.chunks, 5)?)?; // Header hash prefix buffer
        crate::op_set2::OpSet::native_reconstruction_plan(self, &mut budget)?;
        crate::change_graph::ChangeGraph::native_reconstruction_plan(self, &mut budget)?;
        crate::op_set2::change::ChangeCollector::native_reconstruction_plan(self, &mut budget)?;
        // A sole document chunk invokes apply_changes with an empty iterator;
        // that branch allocates no batch state. Other accepted streams may
        // have a first change/bundle or a tail and need the complete batch plan.
        if self.chunks > 1 || self.documents != self.chunks {
            crate::Automerge::native_batch_reconstruction_plan(self, &mut budget)?;
        }
        Ok(budget)
    }

    fn read(storage: &NormalizedStorage<'_>) -> Result<Self> {
        let checked = &storage.shape;
        let mut shape = Self {
            operations: checked.operations,
            dense: checked.dense_operations,
            changes: checked.changes,
            edges: checked.edges,
            actors: checked.actors,
            heads: checked.heads,
            values: checked.value_bytes,
            actor_bytes: 0,
            columns: 0,
            column_bytes: 0,
            column_rows: 0,
            column_events: 0,
            raw_column_bytes: 0,
            marks: 0,
            raw_change_bytes: 0,
            extra_bytes: 0,
            chunks: 0,
            documents: 0,
            bundles: 0,
        };
        let mut input = Reader(&storage.bytes);
        while !input.0.is_empty() {
            let start = input.0.len();
            if input.take(4)? != crate::storage::MAGIC_BYTES {
                return Err(NativeResourceError::Invalid);
            }
            input.take(4)?;
            let kind = input.take(1)?[0];
            let length = input.size()?;
            let mut body = Reader(input.take(length)?);
            shape.chunks = sum(shape.chunks, 1)?;
            match kind {
                0 => {
                    shape.documents = sum(shape.documents, 1)?;
                    shape.actors(&mut body)?;
                    shape.hashes(&mut body)?;
                    let changes = shape.columns(&mut body)?;
                    let ops = shape.columns(&mut body)?;
                    shape.column_data(&changes, &mut body, false)?;
                    shape.column_data(&ops, &mut body, true)?;
                    // Remaining head indexes are already validated by the
                    // witness; they own no native byte payload.
                }
                1 => {
                    shape.raw_change_bytes = sum(shape.raw_change_bytes, start - input.0.len())?;
                    shape.hashes(&mut body)?;
                    shape.actor(&mut body)?;
                    body.uint()?; // sequence
                    body.uint()?; // start operation
                    body.sint()?; // timestamp
                    body.blob()?; // message: already in expanded values
                    shape.actors(&mut body)?;
                    let ops = shape.columns(&mut body)?;
                    shape.column_data(&ops, &mut body, true)?;
                    shape.extra_bytes = sum(shape.extra_bytes, body.0.len())?;
                }
                3 => {
                    shape.bundles = sum(shape.bundles, 1)?;
                    shape.raw_change_bytes = sum(shape.raw_change_bytes, start - input.0.len())?;
                    shape.hashes(&mut body)?;
                    shape.actors(&mut body)?;
                    let changes = shape.columns(&mut body)?;
                    shape.column_data(&changes, &mut body, false)?;
                    let ops = shape.columns(&mut body)?;
                    shape.column_data(&ops, &mut body, true)?;
                    if !body.0.is_empty() {
                        return Err(NativeResourceError::Invalid);
                    }
                }
                _ => return Err(NativeResourceError::Invalid),
            }
        }
        Ok(shape)
    }

    fn actor(&mut self, input: &mut Reader<'_>) -> Result {
        self.actor_bytes = sum(self.actor_bytes, input.blob()?.len())?;
        Ok(())
    }
    fn actors(&mut self, input: &mut Reader<'_>) -> Result {
        let count = input.size()?;
        for _ in 0..count {
            self.actor(input)?;
        }
        Ok(())
    }
    fn hashes(&self, input: &mut Reader<'_>) -> Result {
        let count = input.size()?;
        input.take(product(count, size_of::<crate::ChangeHash>())?)?;
        Ok(())
    }
    fn columns(&mut self, input: &mut Reader<'_>) -> Result<Group> {
        let count = input.size()?;
        if count > MAX_COLUMNS {
            return Err(NativeResourceError::Invalid);
        }
        self.columns = sum(self.columns, count)?;
        let mut group = Group {
            columns: [Column::default(); MAX_COLUMNS],
            count,
        };
        for column in &mut group.columns[..count] {
            let spec = input.uint()?;
            if spec & 8 != 0 {
                return Err(NativeResourceError::Invalid);
            }
            column.spec = u32::try_from(spec).map_err(|_| NativeResourceError::Invalid)?;
            column.len = input.size()?;
            self.column_bytes = sum(self.column_bytes, column.len)?;
        }
        Ok(group)
    }

    fn column_data(&mut self, group: &Group, input: &mut Reader<'_>, operations: bool) -> Result {
        for column in &group.columns[..group.count] {
            let bytes = input.take(column.len)?;
            let kind = column.spec & 7;
            if kind == 7 {
                self.raw_column_bytes = sum(self.raw_column_bytes, bytes.len())?;
                continue;
            }
            let mut values = Reader(bytes);
            while !values.0.is_empty() {
                self.column_events = sum(self.column_events, 1)?;
                if kind == 4 {
                    self.column_rows = sum(self.column_rows, values.size()?)?;
                    continue;
                }
                let run = values.sint()?;
                if run == 0 {
                    self.column_rows = sum(self.column_rows, values.size()?)?;
                    continue;
                }
                let rows = usize::try_from(run.checked_abs().ok_or(NativeResourceError::Invalid)?)
                    .map_err(|_| NativeResourceError::Invalid)?;
                self.column_rows = sum(self.column_rows, rows)?;
                let encoded = if run < 0 { rows } else { 1 };
                self.column_events = sum(self.column_events, encoded)?;
                for _ in 0..encoded {
                    match kind {
                        0 | 1 | 2 | 6 => {
                            let value = values.uint()?;
                            // Native action column 4/integer, Action::Mark=7.
                            if operations && column.spec == 0x42 && value == 7 {
                                self.marks = sum(self.marks, if run > 0 { rows } else { 1 })?;
                            }
                        }
                        3 => {
                            values.sint()?;
                        }
                        5 => {
                            values.blob()?;
                        }
                        _ => return Err(NativeResourceError::Invalid),
                    }
                }
            }
        }
        Ok(())
    }
}

struct Group {
    columns: [Column; MAX_COLUMNS],
    count: usize,
}

/// Minted only after the complete concrete reconstruction peak is reserved.
/// It is neither serialized nor exposed outside the native implementation.
pub(crate) struct Permit<'credit> {
    _peak: &'credit super::NativeReservation,
    clock_cache_nodes: usize,
}

impl Permit<'_> {
    pub(crate) fn check_clock_cache(&self, actual_nodes: usize) -> Result {
        if actual_nodes > self.clock_cache_nodes {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(())
    }
}

impl NormalizedStorage<'_> {
    /// Reconstruct a detached native document under the current owner turn.
    /// The resulting document exposes only charged native operations.
    pub fn reconstruct(
        self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeOwned<AutoCommit>> {
        let allowance = allocations.check_turn(turn)?.clone();
        if !turn.accepts_reservation(&self._reservation) {
            return Err(NativeResourceError::Unaccounted);
        }
        if hexane::diagnostic_exports_enabled() {
            return Err(NativeResourceError::Unaccounted);
        }
        let _scratch =
            allowance.reserve(sum(size_of::<Shape>(), product(2, size_of::<Group>())?)?)?;
        let shape = Shape::read(&self)?;
        let mut plan = shape.plan()?;
        plan.add(super::retained::adoption_extra_bytes(
            plan.backings,
            allocations,
        )?)?;
        let peak = allowance.reserve(plan.bytes())?;
        let permit = Permit {
            _peak: &peak,
            clock_cache_nodes: plan.clock_cache_nodes,
        };
        let value = AutoCommit::load_checked_native(&self.bytes, &permit)?;
        // The complete native load call has returned, so its temporary parse,
        // collector, encoder, index and queued-apply buffers have died. Keep
        // peak held until the actual retained allocation visitor transfers it.
        super::retained::adopt_loaded(value, allocations, turn, peak)
    }
}
