//! Fixed-capacity native export. Existing signed/raw evidence is never rewritten.
use super::save_columns::{Column, Columns, Pass, Sink};
use super::{add, NativeCloneCandidate, NativeOwned, NativeWorkTurn, Result};
use crate::resource_limits::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::storage::{ChunkType, Header};
use crate::{AutoCommit, SaveOptions};
use std::mem::size_of;

pub(crate) fn document(
    ops: &crate::op_set2::OpSet,
    graph: &crate::change_graph::ChangeGraph,
    queue: &crate::change_queue::ChangeQueue,
    allowance: &NativeAllowance,
    options: SaveOptions,
    maximum_output_bytes: usize,
) -> Result<NativeSaved> {
    let mut columns = Columns::new(allowance)?;
    {
        let mut pass = Pass {
            allowance,
            sink: Sink {
                output: None,
                len: 0,
                limit: MAX_EXPORT,
            },
            columns: &mut columns,
            writing: false,
            index: 0,
        };
        graph.native_export(&mut pass)?;
        pass.columns.change_count = pass.index;
        ops.native_export(&mut pass)?;
        pass.columns.bytes = pass.sink.len;
    }
    let mut raw = buffer(columns.bytes, allowance)?;
    {
        let limit = columns.bytes;
        let mut pass = Pass {
            allowance,
            sink: Sink {
                output: Some(&mut raw.bytes),
                len: 0,
                limit,
            },
            columns: &mut columns,
            writing: true,
            index: 0,
        };
        graph.native_export(&mut pass)?;
        if pass.index != pass.columns.change_count {
            return Err(NativeResourceError::Unaccounted);
        }
        ops.native_export(&mut pass)?;
        if pass.index != pass.columns.columns.len() || pass.sink.len != limit {
            return Err(NativeResourceError::Unaccounted);
        }
    }
    let plain = body(ops, graph, &columns, raw.bytes(), allowance)?;
    // Header::hash uses Vec::with_capacity(5). A <=64MiB body needs at most
    // four LEB length bytes plus the one-byte chunk type. Sha256 is inline.
    let _header_credit = allowance.reserve(size_of::<Header>() + 5)?;
    let mut header = Box::new(Header::new(ChunkType::Document, plain.bytes()));
    let compressed;
    let output_body = if options.deflate {
        let (compressed_columns, compressed_raw) =
            compress_columns(&columns, raw.bytes(), allowance)?;
        compressed = body(
            ops,
            graph,
            &compressed_columns,
            compressed_raw.bytes(),
            allowance,
        )?;
        // Document checksums cover the actual compressed column body. The
        // with_data checksum-preserving rule belongs to compressed changes.
        *header = Header::new(ChunkType::Document, compressed.bytes());
        compressed.bytes()
    } else {
        plain.bytes()
    };
    let mut length = add(header.len(), output_body.len())?;
    if options.retain_orphans {
        for change in queue.iter() {
            length = add(length, change.raw_bytes().len())?;
        }
    }
    // Complete chosen representation: document header/body plus any retained
    // raw changes. Refuse before reserving or allocating the destination.
    if length > maximum_output_bytes {
        return Err(NativeResourceError::WorkBytes);
    }
    let mut saved = buffer(length, allowance)?;
    header.write(&mut saved.bytes);
    saved.bytes.extend_from_slice(output_body);
    if options.retain_orphans {
        for change in queue.iter() {
            saved.bytes.extend_from_slice(change.raw_bytes());
        }
    }
    if saved.bytes.len() != length {
        return Err(NativeResourceError::Unaccounted);
    }
    Ok(saved)
}

const MAX_EXPORT: usize = 64 * 1024 * 1024;

fn buffer(bytes: usize, allowance: &NativeAllowance) -> Result<NativeSaved> {
    if bytes > MAX_EXPORT {
        return Err(NativeResourceError::WorkBytes);
    }
    let reservation = allowance.reserve(bytes)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes)
        .map_err(|_| NativeResourceError::WorkBytes)?;
    if output.capacity() > reservation.bytes() {
        return Err(NativeResourceError::Unaccounted);
    }
    Ok(NativeSaved {
        bytes: output,
        _reservation: reservation,
    })
}

fn body(
    ops: &crate::op_set2::OpSet,
    graph: &crate::change_graph::ChangeGraph,
    columns: &Columns,
    raw: &[u8],
    allowance: &NativeAllowance,
) -> Result<NativeSaved> {
    let mut count = Sink {
        output: None,
        len: 0,
        limit: MAX_EXPORT,
    };
    write_body(ops, graph, columns, raw, &mut count)?;
    let length = count.len;
    let mut output = buffer(length, allowance)?;
    let mut sink = Sink {
        output: Some(&mut output.bytes),
        len: 0,
        limit: length,
    };
    write_body(ops, graph, columns, raw, &mut sink)?;
    if sink.len != length {
        return Err(NativeResourceError::Unaccounted);
    }
    Ok(output)
}

fn write_body(
    ops: &crate::op_set2::OpSet,
    graph: &crate::change_graph::ChangeGraph,
    columns: &Columns,
    raw: &[u8],
    sink: &mut Sink<'_>,
) -> Result {
    sink.pack(&(ops.actors.len() as u64))?;
    for actor in &ops.actors {
        sink.pack(&(actor.to_bytes().len() as u64))?;
        sink.bytes(actor.to_bytes())?;
    }
    sink.pack(&(graph.heads().count() as u64))?;
    for head in graph.heads() {
        sink.bytes(head.as_bytes())?;
    }
    for group in [
        &columns.columns[..columns.change_count],
        &columns.columns[columns.change_count..],
    ] {
        sink.pack(&(group.len() as u64))?;
        for column in group {
            sink.pack(&u32::from(column.spec))?;
            sink.pack(&(column.range.len() as u64))?;
        }
    }
    sink.bytes(raw)?;
    for index in graph.head_indexes() {
        sink.pack(&index)?;
    }
    Ok(())
}

fn deflate(input: &[u8], sink: &mut Sink<'_>, allowance: &NativeAllowance) -> Result {
    use miniz_oxide::deflate::core::{
        compress_to_output, create_comp_flags_from_zip_params, CompressorOxide, TDEFLFlush,
        TDEFLStatus,
    };
    // Pinned miniz_oxide 0.8.9: HuffmanOxide is 3x288 u16 counts,
    // 3x288 u16 codes and 3x288 u8 sizes; ParamsOxide owns LocalBuf;
    // DictOxide owns exact boxed dict, next and hash arrays (buffer.rs).
    // No literal/output Vec or other heap workspace is created by core.
    const HUFFMAN: usize = 3 * 288 * (2 + 2 + 1);
    const LOCAL: usize = (65_536 * 13) / 10;
    const DICTIONARY: usize = 32_768 + 258;
    const HASH_CHAINS: usize = 2 * 32_768 * size_of::<u16>();
    let _credit = allowance
        .reserve(size_of::<CompressorOxide>() + HUFFMAN + LOCAL + DICTIONARY + HASH_CHAINS)?;
    let mut compressor = Box::new(CompressorOxide::new(create_comp_flags_from_zip_params(
        6, 0, 0,
    )));
    let mut failure = None;
    let (status, consumed) = compress_to_output(
        &mut compressor,
        input,
        TDEFLFlush::Finish,
        |bytes| match sink.bytes(bytes) {
            Ok(()) => true,
            Err(error) => {
                failure = Some(error);
                false
            }
        },
    );
    if let Some(error) = failure {
        return Err(error);
    }
    if status != TDEFLStatus::Done || consumed != input.len() {
        return Err(NativeResourceError::Unaccounted);
    }
    Ok(())
}

fn compress_columns(
    columns: &Columns,
    raw: &[u8],
    allowance: &NativeAllowance,
) -> Result<(Columns, NativeSaved)> {
    let mut compressed = Columns::new(allowance)?;
    compressed.change_count = columns.change_count;
    let mut count = Sink {
        output: None,
        len: 0,
        limit: MAX_EXPORT,
    };
    for column in &columns.columns {
        let start = count.len;
        let input = raw
            .get(column.range.clone())
            .ok_or(NativeResourceError::Unaccounted)?;
        let spec = if input.len() >= crate::storage::change::DEFLATE_MIN_SIZE {
            deflate(input, &mut count, allowance)?;
            column.spec.deflated()
        } else {
            count.bytes(input)?;
            column.spec
        };
        if compressed.columns.len() == compressed.columns.capacity() {
            return Err(NativeResourceError::Unaccounted);
        }
        compressed.columns.push(Column {
            spec,
            range: start..count.len,
        });
    }
    compressed.bytes = count.len;
    let mut output = buffer(count.len, allowance)?;
    let mut sink = Sink {
        output: Some(&mut output.bytes),
        len: 0,
        limit: count.len,
    };
    for (column, expected) in columns.columns.iter().zip(&compressed.columns) {
        let input = raw
            .get(column.range.clone())
            .ok_or(NativeResourceError::Unaccounted)?;
        if expected.spec.deflate() {
            deflate(input, &mut sink, allowance)?;
        } else {
            sink.bytes(input)?;
        }
        if sink.len != expected.range.end {
            return Err(NativeResourceError::Unaccounted);
        }
    }
    Ok((compressed, output))
}

/// Newly exported bytes and their inseparable allocation credit.
/// This is neither original checkpoint evidence nor native qualification.
pub struct NativeSaved {
    bytes: Vec<u8>,
    _reservation: NativeReservation,
}
impl NativeSaved {
    /// Borrows the export while its allocation custody remains alive.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl NativeOwned<AutoCommit> {
    /// Exports under an additional complete-output ceiling, checked before
    /// allocating the destination. Workspace uses the current turn's credit.
    pub fn save_bounded_with_limit(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
        maximum_output_bytes: usize,
    ) -> Result<NativeSaved> {
        self.value
            .native_save(self.check_turn(turn)?, options, maximum_output_bytes)
    }

    /// Exports a closed document using the owner's current work turn.
    pub fn save_bounded(&self, turn: &NativeWorkTurn, options: SaveOptions) -> Result<NativeSaved> {
        self.save_bounded_with_limit(turn, options, MAX_EXPORT)
    }
}
impl NativeCloneCandidate<AutoCommit> {
    /// Exports an unqualified candidate under a complete-output ceiling.
    pub fn save_bounded_with_limit(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
        maximum_output_bytes: usize,
    ) -> Result<NativeSaved> {
        self.value
            .native_save(self.check_turn(turn)?, options, maximum_output_bytes)
    }

    /// Exports an unqualified candidate; this does not qualify its source.
    pub fn save_bounded(&self, turn: &NativeWorkTurn, options: SaveOptions) -> Result<NativeSaved> {
        self.save_bounded_with_limit(turn, options, MAX_EXPORT)
    }
}
