//! Borrowed incoming checks before native narrowing or decoding, and borrowed
//! per-change dimensions for validation budgets.
//!
//! Ported from the 0.8 patch's `reconstruct/incoming/preflight.rs`. automerge
//! 0.12 still narrows object, key and predecessor counters/actors with
//! `OpId::new(u64, usize)`, which panics above `u32::MAX`; every such column
//! is checked here, without allocating, before any native decoder runs. No
//! native decoder is used to validate its own input.
use super::budget::{sum, Charge};
use crate::resource_limits::inspect::{Reader, MAX_COLUMNS};
use crate::resource_limits::NativeResourceError as Error;

type Result<T = ()> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Default)]
struct Column<'a> {
    spec: u32,
    bytes: &'a [u8],
    len: usize,
    rows: usize,
}
struct Columns<'a> {
    columns: [Column<'a>; MAX_COLUMNS],
    len: usize,
}
impl<'a> Columns<'a> {
    fn metadata(reader: &mut Reader<'a>) -> Result<Self> {
        let len = reader.size()?;
        if len > MAX_COLUMNS {
            return Err(Error::Invalid);
        }
        let mut result = Self {
            columns: [Column::default(); MAX_COLUMNS],
            len,
        };
        let mut last = None;
        for col in &mut result.columns[..len] {
            col.spec = u32::try_from(reader.uint()?).map_err(|_| Error::Invalid)?;
            if col.spec & 8 != 0 || last.is_some_and(|last| last >= col.spec) {
                return Err(Error::Invalid);
            }
            last = Some(col.spec);
            col.len = reader.size()?;
        }
        Ok(result)
    }
    fn data(&mut self, reader: &mut Reader<'a>, actors: usize) -> Result {
        for col in &mut self.columns[..self.len] {
            col.bytes = reader.take(col.len)?;
            if col.spec & 7 == 7 {
                continue;
            }
            let mut values = Values::new(*col);
            while let Some(value) = values.next()? {
                col.rows = sum(col.rows, 1).map_err(|_| Error::Invalid)?;
                if col.spec & 7 == 1 {
                    if let Some(actor) = value.number()? {
                        if actor < 0 || actor > u32::MAX as i128 || actor >= actors as i128 {
                            return Err(Error::Invalid);
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn get(&self, spec: u32) -> Column<'a> {
        self.columns[..self.len]
            .iter()
            .find(|c| c.spec == spec)
            .copied()
            .unwrap_or(Column {
                spec,
                ..Column::default()
            })
    }
    fn values(&self, spec: u32) -> Values<'a> {
        Values::new(self.get(spec))
    }
    fn rows(&self, spec: u32) -> usize {
        self.get(spec).rows
    }
    fn aligned(&self, specs: &[u32], rows: usize) -> Result {
        for spec in specs {
            let col = self.get(*spec);
            // An absent/all-omitted optional column has native default values.
            if !col.bytes.is_empty() && col.rows != rows {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Cell {
    Null,
    Number(i128),
    String(usize),
}
impl Cell {
    fn number(self) -> Result<Option<i128>> {
        match self {
            Self::Null => Ok(None),
            Self::Number(n) => Ok(Some(n)),
            Self::String(_) => Err(Error::Invalid),
        }
    }
    fn string_len(self) -> Result<usize> {
        match self {
            Self::Null => Ok(0),
            Self::String(n) => Ok(n),
            Self::Number(_) => Err(Error::Invalid),
        }
    }
}

/// Nullable RLE, Boolean and Delta traversal over checked borrowed slices.
/// Both the encoded run total and each absolute/prefix sum are checked before
/// any native run multiplication or literal summation can execute.
struct Values<'a> {
    reader: Reader<'a>,
    kind: u32,
    remaining: usize,
    literal: bool,
    repeated: Cell,
    absolute: i128,
    run_sum: i128,
    boolean: bool,
}
impl<'a> Values<'a> {
    fn new(col: Column<'a>) -> Self {
        Self {
            reader: Reader(col.bytes),
            kind: col.spec & 7,
            remaining: 0,
            literal: false,
            repeated: Cell::Null,
            absolute: 0,
            run_sum: 0,
            boolean: false,
        }
    }
    fn item(&mut self) -> Result<Cell> {
        match self.kind {
            0 | 1 | 2 | 6 => Ok(Cell::Number(self.reader.uint()? as i128)),
            3 => Ok(Cell::Number(self.reader.sint()? as i128)),
            5 => {
                let bytes = self.reader.blob()?;
                std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?;
                Ok(Cell::String(bytes.len()))
            }
            _ => Err(Error::Invalid),
        }
    }
    fn next(&mut self) -> Result<Option<Cell>> {
        while self.remaining == 0 {
            if self.reader.0.is_empty() {
                return Ok(None);
            }
            self.run_sum = 0;
            if self.kind == 4 {
                self.remaining = self.reader.size()?;
                self.repeated = Cell::Number(i128::from(self.boolean));
                self.boolean = !self.boolean;
                self.literal = false;
            } else {
                let run = self.reader.sint()?;
                self.literal = run < 0;
                self.remaining = if run == 0 {
                    self.reader.size()?
                } else {
                    usize::try_from(run.checked_abs().ok_or(Error::Invalid)?)
                        .map_err(|_| Error::Invalid)?
                };
                self.repeated = if run > 0 { self.item()? } else { Cell::Null };
                if self.kind == 3 && run > 0 {
                    let total = self
                        .repeated
                        .number()?
                        .ok_or(Error::Invalid)?
                        .checked_mul(self.remaining as i128)
                        .ok_or(Error::Invalid)?;
                    signed(total)?;
                    signed(self.absolute.checked_add(total).ok_or(Error::Invalid)?)?;
                }
            }
        }
        self.remaining -= 1;
        let value = if self.literal {
            self.item()?
        } else {
            self.repeated
        };
        if self.kind == 3 {
            if let Some(delta) = value.number()? {
                self.run_sum = signed(self.run_sum.checked_add(delta).ok_or(Error::Invalid)?)?;
                self.absolute = signed(self.absolute.checked_add(delta).ok_or(Error::Invalid)?)?;
                return Ok(Some(Cell::Number(self.absolute)));
            }
        }
        Ok(Some(value))
    }
    fn cell(&mut self) -> Result<Cell> {
        Ok(self.next()?.unwrap_or(Cell::Null))
    }
    fn number(&mut self) -> Result<Option<i128>> {
        self.cell()?.number()
    }
    fn required(&mut self) -> Result<u64> {
        u64::try_from(self.number()?.ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)
    }
}
fn signed(n: i128) -> Result<i128> {
    if n < i64::MIN as i128 || n > i64::MAX as i128 {
        Err(Error::Invalid)
    } else {
        Ok(n)
    }
}
fn counter(n: i128) -> Result {
    if n < 0 || n > u32::MAX as i128 {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
fn hashes(reader: &mut Reader<'_>) -> Result<usize> {
    let n = reader.size()?;
    reader.take(n.checked_mul(32).ok_or(Error::Invalid)?)?;
    Ok(n)
}
fn actors(reader: &mut Reader<'_>) -> Result<usize> {
    let n = reader.size()?;
    if n > u32::MAX as usize {
        return Err(Error::Invalid);
    }
    for _ in 0..n {
        reader.blob()?;
    }
    Ok(n)
}

/// Borrowed dimensions of one change's operation columns.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChangeDims {
    /// Operation rows.
    pub(crate) ops: usize,
    /// Predecessor references.
    pub(crate) preds: usize,
    /// Largest predecessor count of one operation.
    pub(crate) max_preds: usize,
    /// Largest raw value payload of one operation.
    pub(crate) max_value: usize,
    /// Sum of map-key string bytes.
    pub(crate) key_bytes: usize,
    /// Largest map-key string of one operation.
    pub(crate) max_key: usize,
    /// Largest mark name of one operation.
    pub(crate) max_mark_name: usize,
    /// Actor table entries (author plus other actors).
    pub(crate) actors: usize,
}
impl ChangeDims {
    /// Largest transient owned allocation of decoding one storage `ChangeOp`:
    /// its predecessor Vec (grown past `min(n, 100)` by doubling, with realloc
    /// overlap) plus owned value/key/mark-name payloads with a transient copy
    /// each, and fixed SmolStr/Arc headers.
    pub(crate) fn op_decode_bytes(&self) -> std::result::Result<usize, Error> {
        use super::budget::product;
        let preds = product(product(self.max_preds, 3)?, std::mem::size_of::<crate::types::OpId>())?;
        let payload = sum(sum(self.max_value, self.max_key)?, self.max_mark_name)?;
        sum(sum(preds, product(payload, 2)?)?, 256)
    }
}

/// Validate every chunk of normalized storage before any native decoder runs.
/// The only allocation is a charged operation-id Vec per document/bundle
/// chunk, released before the next chunk. Also used by checked load (L3).
pub(crate) fn validate(bytes: &[u8], charge: &mut Charge<'_>) -> Result {
    let mut stream = Reader(bytes);
    while !stream.0.is_empty() {
        let (kind, mut body) = chunk(&mut stream)?;
        match kind {
            0 => {
                let actor_count = actors(&mut body)?;
                hashes(&mut body)?;
                let mut meta = Columns::metadata(&mut body)?;
                let mut ops = Columns::metadata(&mut body)?;
                meta.data(&mut body, actor_count)?;
                ops.data(&mut body, actor_count)?;
                metadata(&meta, false, 0)?;
                operations(&ops, kind, Some(&mut *charge))?;
            }
            1 => {
                change_body(body)?;
            }
            3 => {
                let external = hashes(&mut body)?;
                let actor_count = actors(&mut body)?;
                let mut meta = Columns::metadata(&mut body)?;
                meta.data(&mut body, actor_count)?;
                let mut ops = Columns::metadata(&mut body)?;
                ops.data(&mut body, actor_count)?;
                metadata(&meta, true, external)?;
                operations(&ops, kind, Some(&mut *charge))?;
            }
            // Normalized storage never holds compressed chunks.
            _ => return Err(Error::Invalid),
        }
    }
    Ok(())
}

/// Whether normalized storage holds a document or bundle chunk, whose native
/// expansion needs the load-shaped reservation.
pub(crate) fn has_expanding_chunks(bytes: &[u8]) -> Result<bool> {
    let mut stream = Reader(bytes);
    while !stream.0.is_empty() {
        let (kind, _) = chunk(&mut stream)?;
        if kind != 1 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Dimensions of one complete change chunk (for example `Change::raw_bytes`).
pub(crate) fn change_dims(raw: &[u8]) -> Result<ChangeDims> {
    let mut stream = Reader(raw);
    let (kind, body) = chunk(&mut stream)?;
    if kind != 1 || !stream.0.is_empty() {
        return Err(Error::Invalid);
    }
    change_body(body)
}

fn chunk<'a>(stream: &mut Reader<'a>) -> Result<(u8, Reader<'a>)> {
    stream.take(8)?; // magic and checksum, checked by inspection/parsing
    let kind = stream.take(1)?[0];
    let len = stream.size()?;
    Ok((kind, Reader(stream.take(len)?)))
}

fn change_body(mut body: Reader<'_>) -> Result<ChangeDims> {
    hashes(&mut body)?;
    body.blob()?; // author actor
    body.uint()?; // seq
    body.uint()?; // start op
    body.sint()?; // timestamp
    body.blob()?; // message
    let actor_count = sum(actors(&mut body)?, 1).map_err(|_| Error::Invalid)?;
    let mut ops = Columns::metadata(&mut body)?;
    ops.data(&mut body, actor_count)?;
    let mut dims = operations(&ops, 1, None)?;
    dims.actors = actor_count;
    Ok(dims)
}

fn metadata(cols: &Columns<'_>, bundle: bool, external: usize) -> Result {
    let rows = cols.rows(0x01);
    let (maximum, time, message, counts, deps, extra_meta, extra_raw) = if bundle {
        (0x23, 0x33, 0x45, 0x50, 0x53, 0x60, 0x67)
    } else {
        (0x13, 0x23, 0x35, 0x40, 0x43, 0x56, 0x57)
    };
    cols.aligned(&[0x03, maximum, time, message, counts, extra_meta], rows)?;
    if bundle {
        cols.aligned(&[0x13], rows)?;
    }
    let mut author = cols.values(0x01);
    let mut seq = cols.values(0x03);
    let mut maxima = cols.values(maximum);
    let mut starts = cols.values(0x13);
    let mut groups = cols.values(counts);
    let mut dependencies = cols.values(deps);
    let mut extras = cols.values(extra_meta);
    let mut raw = Reader(cols.get(extra_raw).bytes);
    let mut edges = 0usize;
    for index in 0..rows {
        author.required()?;
        let sequence = seq.required()?;
        if sequence == 0 {
            return Err(Error::Invalid);
        }
        counter(sequence as i128)?;
        let max = maxima.required()?;
        counter(max as i128)?;
        if bundle {
            let start = starts.required()?;
            if start == 0 || start > max.saturating_add(1) {
                return Err(Error::Invalid);
            }
            counter(start as i128)?;
        }
        let n = usize::try_from(groups.number()?.unwrap_or(0)).map_err(|_| Error::Invalid)?;
        edges = edges.checked_add(n).ok_or(Error::Invalid)?;
        for _ in 0..n {
            let dep = usize::try_from(dependencies.required()?).map_err(|_| Error::Invalid)?;
            if dep >= index
                && (!bundle
                    || dep < rows
                    || dep.checked_sub(rows).is_none_or(|d| d >= external))
            {
                return Err(Error::Invalid);
            }
        }
        let extra = u64::try_from(extras.number()?.unwrap_or(0)).map_err(|_| Error::Invalid)?;
        let len = if bundle { extra } else { extra >> 4 };
        raw.take(usize::try_from(len).map_err(|_| Error::Invalid)?)?;
    }
    if cols.rows(deps) != edges || !raw.0.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn operations(cols: &Columns<'_>, kind: u8, mut charge: Option<&mut Charge<'_>>) -> Result<ChangeDims> {
    let rows = cols.rows(0x42);
    let group = if kind == 0 { 0x80 } else { 0x70 };
    let object_counter = if kind == 3 { 0x03 } else { 0x02 };
    cols.aligned(
        &[
            0x01,
            object_counter,
            0x11,
            0x13,
            0x15,
            0x34,
            0x56,
            group,
            0x94,
            0xa5,
        ],
        rows,
    )?;
    if kind != 1 && (cols.rows(0x21) != rows || cols.rows(0x23) != rows) {
        return Err(Error::Invalid);
    }
    // All narrowing references, including optional roots/heads, are inspected
    // before native ActorIdx/OpId constructors or actor-table indexing.
    for spec in [object_counter, 0x13, 0x23, group + 3] {
        let mut values = cols.values(spec);
        while let Some(value) = values.next()? {
            if let Some(n) = value.number()? {
                counter(n)?;
            }
        }
    }
    let mut dims = ChangeDims {
        ops: rows,
        ..ChangeDims::default()
    };
    let mut counts = cols.values(group);
    for _ in 0..rows {
        let n = usize::try_from(counts.number()?.unwrap_or(0)).map_err(|_| Error::Invalid)?;
        dims.preds = dims.preds.checked_add(n).ok_or(Error::Invalid)?;
        dims.max_preds = dims.max_preds.max(n);
    }
    let edges = dims.preds;
    if cols.rows(group + 1) != edges || cols.rows(group + 3) != edges {
        return Err(Error::Invalid);
    }
    let mut edge_actor = cols.values(group + 1);
    let mut edge_counter = cols.values(group + 3);
    for _ in 0..edges {
        edge_actor.required()?;
        if edge_counter.required()? == 0 {
            return Err(Error::Invalid);
        }
    }
    let mut keys = cols.values(0x15);
    let mut marks = cols.values(0xa5);
    let mut actions = cols.values(0x42);
    let mut meta = cols.values(0x56);
    let mut raw = Reader(cols.get(0x57).bytes);
    let mut id_actor = cols.values(0x21);
    let mut id_counter = cols.values(0x23);
    let mut ids = Vec::new();
    if kind != 1 {
        if let Some(charge) = charge.as_deref_mut() {
            charge.grow(super::budget::vec::<(u32, u32)>(rows).map_err(|_| Error::WorkBytes)?)?;
        }
        ids.try_reserve_exact(rows).map_err(|_| Error::WorkBytes)?;
    }
    for _ in 0..rows {
        let action = actions.required()?;
        let value = meta.number()?.unwrap_or(0);
        let value = u64::try_from(value).map_err(|_| Error::Invalid)?;
        let len = usize::try_from(value >> 4).map_err(|_| Error::Invalid)?;
        let bytes = raw.take(len)?;
        // Borrowed ScalarValue decoding validates UTF-8/numeric/float lengths
        // without creating an owning payload. Its result is never retained.
        crate::op_set2::types::ScalarValue::from_raw(value.into(), bytes)
            .map_err(|_| Error::Invalid)?;
        if action > 7 || (action == 5 && !matches!(value & 15, 3 | 4)) {
            return Err(Error::Invalid);
        }
        dims.max_value = dims.max_value.max(len);
        let key = keys.cell()?.string_len()?;
        dims.key_bytes = dims.key_bytes.checked_add(key).ok_or(Error::Invalid)?;
        dims.max_key = dims.max_key.max(key);
        dims.max_mark_name = dims.max_mark_name.max(marks.cell()?.string_len()?);
        if kind != 1 {
            let actor = id_actor.required()?;
            let ctr = id_counter.required()?;
            if ctr == 0 {
                return Err(Error::Invalid);
            }
            ids.push((actor as u32, ctr as u32));
        }
    }
    if !raw.0.is_empty() {
        return Err(Error::Invalid);
    }
    // Document and bundle operation ids must be unique.
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::Invalid);
    }
    Ok(dims)
}
