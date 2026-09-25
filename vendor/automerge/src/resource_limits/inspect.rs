//! Borrowed storage accounting. No native document/operation reconstruction here.
use super::{
    NativeAllowance, NativeLimits, NativeReservation, NativeResourceError as Error, NativeShape,
};
use miniz_oxide::inflate::stream::{inflate as inflate_step, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use sha2::{Digest, Sha256};

pub(super) const MAX_COLUMNS: usize = 128;
// Exact miniz_oxide 0.8.9 has one boxed InflateState: a fixed 32 KiB
// dictionary and fixed Huffman arrays, with no growable owned fields. Charge
// that concrete layout and the scanner's fixed output scratch before creation.
// This does not depend on flate2's workspace-selected backend.
pub(super) const INFLATE_WORK: usize = std::mem::size_of::<InflateState>() + 8_192;

#[derive(Clone, Copy, Default)]
pub(super) struct Column {
    pub(super) spec: u32,
    pub(super) len: usize,
}

pub(super) struct Reader<'a>(pub(super) &'a [u8]);
impl<'a> Reader<'a> {
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let (bytes, rest) = self.0.split_at_checked(n).ok_or(Error::Invalid)?;
        self.0 = rest;
        Ok(bytes)
    }
    pub(super) fn uint(&mut self) -> Result<u64, Error> {
        leb128::read::unsigned(&mut self.0).map_err(|_| Error::Invalid)
    }
    pub(super) fn sint(&mut self) -> Result<i64, Error> {
        leb128::read::signed(&mut self.0).map_err(|_| Error::Invalid)
    }
    pub(super) fn size(&mut self) -> Result<usize, Error> {
        usize::try_from(self.uint()?).map_err(|_| Error::Invalid)
    }
    pub(super) fn blob(&mut self) -> Result<&'a [u8], Error> {
        let n = self.size()?;
        self.take(n)
    }
}

fn add(total: &mut usize, value: usize, maximum: usize, error: Error) -> Result<(), Error> {
    *total = total
        .checked_add(value)
        .filter(|n| *n <= maximum)
        .ok_or(error)?;
    Ok(())
}

pub(super) fn uint_size(mut value: usize) -> usize {
    let mut size = 1;
    while value >= 128 {
        value >>= 7;
        size += 1;
    }
    size
}

pub(super) fn visit_inflated(
    input: &[u8],
    allowance: &NativeAllowance,
    mut visit: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let _scratch = allowance.reserve(INFLATE_WORK)?;
    let mut decoder = InflateState::new_boxed(DataFormat::Raw);
    let mut scratch = [0; 8_192];
    let mut consumed: usize = 0;
    loop {
        let result = inflate_step(
            &mut decoder,
            input.get(consumed..).ok_or(Error::Invalid)?,
            &mut scratch,
            MZFlush::None,
        );
        consumed = consumed
            .checked_add(result.bytes_consumed)
            .ok_or(Error::Invalid)?;
        visit(&scratch[..result.bytes_written])?;
        match result.status {
            Ok(MZStatus::StreamEnd) => {
                return if consumed == input.len() {
                    Ok(())
                } else {
                    Err(Error::Invalid)
                }
            }
            Ok(MZStatus::Ok) | Err(MZError::Buf) => {}
            _ => return Err(Error::Invalid),
        }
        if result.bytes_consumed == 0 && result.bytes_written == 0 {
            return Err(Error::Invalid);
        }
    }
}

pub(super) struct Inflated {
    bytes: Vec<u8>,
    _reservation: NativeReservation,
}
impl std::ops::Deref for Inflated {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(super) fn inflate(
    input: &[u8],
    maximum: usize,
    allowance: &NativeAllowance,
) -> Result<Inflated, Error> {
    // Count with fixed scratch before allocating any output proportional to
    // attacker-controlled expansion. In particular, take(limit).read_to_end is
    // insufficient: its Vec still grows and EOF need not mean a complete stream.
    let mut length = 0;
    visit_inflated(input, allowance, |bytes| {
        add(&mut length, bytes.len(), maximum, Error::NormalizedBytes)
    })?;
    let reservation = allowance.reserve(length)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| Error::WorkBytes)?;
    output.resize(length, 0);
    let mut offset: usize = 0;
    visit_inflated(input, allowance, |bytes| {
        let end = offset.checked_add(bytes.len()).ok_or(Error::Invalid)?;
        output
            .get_mut(offset..end)
            .ok_or(Error::Invalid)?
            .copy_from_slice(bytes);
        offset = end;
        Ok(())
    })?;
    if offset != length {
        return Err(Error::Invalid);
    }
    Ok(Inflated {
        bytes: output,
        _reservation: reservation,
    })
}

fn checksum(kind: u8, body: &[u8], expected: &[u8]) -> bool {
    let mut hash = Sha256::new();
    hash.update([kind]);
    let mut n = body.len() as u64;
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        hash.update([b | if n == 0 { 0 } else { 128 }]);
        if n == 0 {
            break;
        }
    }
    hash.update(body);
    &hash.finalize()[..4] == expected
}

pub(super) fn inspect(
    input: &[u8],
    limits: &NativeLimits,
    allowance: &NativeAllowance,
) -> Result<NativeShape, Error> {
    if limits.max_columns > MAX_COLUMNS {
        return Err(Error::Collection);
    }
    let _scratch = allowance.reserve(std::mem::size_of::<[Column; MAX_COLUMNS]>() * 2)?;
    let mut scan = Scan {
        limits,
        allowance,
        shape: NativeShape::empty(),
    };
    let mut reader = Reader(input);
    let mut chunks = 0;
    while !reader.0.is_empty() {
        add(&mut chunks, 1, limits.max_chunks, Error::Collection)?;
        if reader.take(4)? != crate::storage::MAGIC_BYTES {
            return Err(Error::Invalid);
        }
        let expected = reader.take(4)?;
        let kind = reader.take(1)?[0];
        let length = reader.size()?;
        let body = reader.take(length)?;
        let chunk_start = (
            scan.shape.normalized_bytes,
            scan.shape.operations,
            scan.shape.changes,
        );
        scan.normalized(9)?;
        let body_start = scan.shape.normalized_bytes;
        match kind {
            0 | 1 | 3 => {
                if !checksum(kind, body, expected) {
                    return Err(Error::Invalid);
                }
                scan.body(kind, body)?;
            }
            2 => {
                let remaining = limits
                    .max_normalized_bytes
                    .checked_sub(scan.shape.normalized_bytes)
                    .ok_or(Error::NormalizedBytes)?;
                let decoded = inflate(body, remaining, allowance)?;
                if !checksum(1, &decoded, expected) {
                    return Err(Error::Invalid);
                }
                scan.body(1, &decoded)?;
            }
            _ => return Err(Error::Invalid),
        }
        scan.normalized(uint_size(scan.shape.normalized_bytes - body_start))?;
        if chunks == 1 && kind == 0 {
            // The leading document chunk loads through column reconstruction,
            // every later op through batch apply; load bounds need the split.
            scan.shape.leading_document_bytes = scan.shape.normalized_bytes - chunk_start.0;
            scan.shape.leading_document_operations = scan.shape.operations - chunk_start.1;
            scan.shape.leading_document_changes = scan.shape.changes - chunk_start.2;
        }
    }
    Ok(scan.shape)
}

struct Scan<'a> {
    limits: &'a NativeLimits,
    allowance: &'a NativeAllowance,
    shape: NativeShape,
}
impl Scan<'_> {
    fn normalized(&mut self, count: usize) -> Result<(), Error> {
        add(
            &mut self.shape.normalized_bytes,
            count,
            self.limits.max_normalized_bytes,
            Error::NormalizedBytes,
        )
    }
    fn actors(&mut self, reader: &mut Reader<'_>) -> Result<usize, Error> {
        let count = reader.size()?;
        add(
            &mut self.shape.actors,
            count,
            self.limits.max_actors,
            Error::Collection,
        )?;
        for _ in 0..count {
            let actor = reader.blob()?;
            self.shape.max_actor_bytes = self.shape.max_actor_bytes.max(actor.len());
        }
        self.shape.max_chunk_actors = self.shape.max_chunk_actors.max(count);
        Ok(count)
    }
    fn hashes(&mut self, reader: &mut Reader<'_>) -> Result<usize, Error> {
        let count = reader.size()?;
        add(
            &mut self.shape.heads,
            count,
            self.limits.max_heads,
            Error::Collection,
        )?;
        reader.take(count.checked_mul(32).ok_or(Error::Collection)?)?;
        Ok(count)
    }
    fn metadata(
        &self,
        reader: &mut Reader<'_>,
    ) -> Result<([Column; MAX_COLUMNS], usize, usize), Error> {
        let before = reader.0.len();
        let count = reader.size()?;
        if count > self.limits.max_columns {
            return Err(Error::Collection);
        }
        let mut columns = [Column::default(); MAX_COLUMNS];
        let mut last = None;
        for column in &mut columns[..count] {
            column.spec = u32::try_from(reader.uint()?).map_err(|_| Error::Invalid)?;
            column.len = reader.size()?;
            let normalized = column.spec & !8;
            if last.is_some_and(|old| old >= normalized) {
                return Err(Error::Invalid);
            }
            last = Some(normalized);
        }
        Ok((columns, count, before - reader.0.len()))
    }
    fn body(&mut self, kind: u8, body: &[u8]) -> Result<(), Error> {
        let before = self.shape.normalized_bytes;
        let mut reader = Reader(body);
        match kind {
            0 => {
                let actors = self.actors(&mut reader)?;
                let heads = self.hashes(&mut reader)?;
                let (changes, changes_len, change_meta) = self.metadata(&mut reader)?;
                let (ops, ops_len, op_meta) = self.metadata(&mut reader)?;
                super::structure::document(&changes[..changes_len], reader.0,
                    actors, self.limits, self.allowance, &mut self.shape)?;
                self.normalized(body.len() - reader.0.len() - change_meta - op_meta)?;
                self.columns(&changes[..changes_len], &mut reader, false, true)?;
                self.columns(&ops[..ops_len], &mut reader, true, true)?;
                if !reader.0.is_empty() {
                    let suffix = reader.0.len();
                    for _ in 0..heads {
                        reader.uint()?;
                    }
                    if !reader.0.is_empty() {
                        return Err(Error::Invalid);
                    }
                    self.normalized(suffix)?;
                }
            }
            1 => {
                let dependencies = self.hashes(&mut reader)?;
                add(
                    &mut self.shape.edges,
                    dependencies,
                    self.limits.max_edges,
                    Error::Collection,
                )?;
                add(
                    &mut self.shape.actors,
                    1,
                    self.limits.max_actors,
                    Error::Collection,
                )?;
                let author = reader.blob()?;
                self.shape.max_actor_bytes = self.shape.max_actor_bytes.max(author.len());
                reader.uint()?; // sequence
                let start_op = reader.uint()?;
                if start_op == 0 {
                    return Err(Error::Invalid);
                }
                reader.sint()?; // timestamp
                let message = reader.blob()?;
                std::str::from_utf8(message).map_err(|_| Error::Invalid)?;
                add(
                    &mut self.shape.value_bytes,
                    message.len(),
                    self.limits.max_value_bytes,
                    Error::ValueBytes,
                )?;
                let others = self.actors(&mut reader)?;
                // A change chunk's actor table is its author plus the others.
                self.shape.max_chunk_actors = self.shape.max_chunk_actors.max(others + 1);
                let (ops, ops_len, op_meta) = self.metadata(&mut reader)?;
                self.normalized(body.len() - reader.0.len() - op_meta)?;
                let before_ops = self.shape.operations;
                self.columns(&ops[..ops_len], &mut reader, true, false)?;
                super::structure::change(start_op, self.shape.operations - before_ops,
                    self.limits, &mut self.shape)?;
                self.normalized(reader.0.len())?; // opaque extra bytes
                add(
                    &mut self.shape.changes,
                    1,
                    self.limits.max_changes,
                    Error::Collection,
                )?;
            }
            3 => {
                self.hashes(&mut reader)?;
                self.actors(&mut reader)?;
                let (changes, changes_len, change_meta) = self.metadata(&mut reader)?;
                super::structure::bundle(&changes[..changes_len], reader.0,
                    self.limits, self.allowance, &mut self.shape)?;
                self.normalized(body.len() - reader.0.len() - change_meta)?;
                self.columns(&changes[..changes_len], &mut reader, false, false)?;
                let (ops, ops_len, _) = self.metadata(&mut reader)?;
                self.columns(&ops[..ops_len], &mut reader, true, false)?;
                if !reader.0.is_empty() {
                    return Err(Error::Invalid);
                }
            }
            _ => return Err(Error::Invalid),
        }
        if self.shape.normalized_bytes < before {
            return Err(Error::Invalid);
        }
        if kind == 1 || kind == 3 {
            // Preserve change/bundle bodies byte-for-byte. Rewriting even a
            // nonminimal metadata encoding could change native change hashes.
            let measured = self.shape.normalized_bytes - before;
            self.normalized(body.len().checked_sub(measured).ok_or(Error::Invalid)?)?;
        }
        Ok(())
    }
    fn columns(
        &mut self,
        columns: &[Column],
        reader: &mut Reader<'_>,
        operations: bool,
        may_compress: bool,
    ) -> Result<(), Error> {
        let mut rows = 0;
        self.normalized(uint_size(columns.len()))?;
        for column in columns {
            let bytes = reader.take(column.len)?;
            let decoded;
            let bytes = if column.spec & 8 != 0 {
                if !may_compress {
                    return Err(Error::Invalid);
                }
                let remaining = self
                    .limits
                    .max_normalized_bytes
                    .checked_sub(self.shape.normalized_bytes)
                    .ok_or(Error::NormalizedBytes)?;
                decoded = inflate(bytes, remaining, self.allowance)?;
                &decoded[..]
            } else {
                bytes
            };
            self.normalized(uint_size((column.spec & !8) as usize) + uint_size(bytes.len()))?;
            self.normalized(bytes.len())?;
            let kind = column.spec & 7;
            let grouped = columns
                .iter()
                .any(|group| group.spec >> 4 == column.spec >> 4 && group.spec & 7 == 0);
            let row_limit = if grouped && kind != 0 {
                self.limits.max_edges
            } else if operations {
                self.limits.max_operations
            } else {
                self.limits.max_changes
            };
            let values = self.column(bytes, kind, row_limit, operations)?;
            if !grouped && kind != 7 {
                rows = rows.max(values);
            }
            if kind == 0 {
                rows = rows.max(values);
            }
        }
        if operations {
            add(
                &mut self.shape.operations,
                rows,
                self.limits.max_operations,
                Error::Operations,
            )
        } else {
            add(
                &mut self.shape.changes,
                rows,
                self.limits.max_changes,
                Error::Collection,
            )
        }
    }
    fn column(
        &mut self,
        bytes: &[u8],
        kind: u32,
        maximum: usize,
        operations: bool,
    ) -> Result<usize, Error> {
        if kind == 7 {
            return Ok(0);
        }
        let mut reader = Reader(bytes);
        let mut rows = 0;
        let row_error = || {
            if operations {
                Error::Operations
            } else {
                Error::Collection
            }
        };
        while !reader.0.is_empty() {
            if kind == 4 {
                let count = reader.size()?;
                add(&mut rows, count, maximum, row_error())?;
                continue;
            }
            let run = reader.sint()?;
            if run == 0 {
                let count = reader.size()?;
                add(&mut rows, count, maximum, row_error())?;
            } else if run > 0 {
                let count = usize::try_from(run).map_err(|_| row_error())?;
                add(&mut rows, count, maximum, row_error())?;
                self.value(&mut reader, kind, count)?;
            } else {
                let count = usize::try_from(run.checked_neg().ok_or_else(row_error)?)
                    .map_err(|_| row_error())?;
                add(&mut rows, count, maximum, row_error())?;
                for _ in 0..count {
                    self.value(&mut reader, kind, 1)?;
                }
            }
        }
        Ok(rows)
    }
    fn value(&mut self, reader: &mut Reader<'_>, kind: u32, count: usize) -> Result<(), Error> {
        let bytes = match kind {
            0 => {
                let value = reader.size()?;
                let count = value.checked_mul(count).ok_or(Error::Collection)?;
                add(
                    &mut self.shape.edges,
                    count,
                    self.limits.max_edges,
                    Error::Collection,
                )?;
                0
            }
            1 | 2 => {
                reader.uint()?;
                0
            }
            3 => {
                reader.sint()?;
                0
            }
            5 => {
                let bytes = reader.blob()?;
                std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?;
                bytes.len().checked_mul(count).ok_or(Error::ValueBytes)?
            }
            6 => {
                let value = usize::try_from(reader.uint()? >> 4).map_err(|_| Error::ValueBytes)?;
                value.checked_mul(count).ok_or(Error::ValueBytes)?
            }
            _ => return Err(Error::Invalid),
        };
        add(
            &mut self.shape.value_bytes,
            bytes,
            self.limits.max_value_bytes,
            Error::ValueBytes,
        )
    }
}
