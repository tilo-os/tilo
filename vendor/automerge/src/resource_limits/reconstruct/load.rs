//! Strict load of normalized storage with automerge 0.12's own primitives.
//!
//! Semantics follow the 0.8 patch's `Automerge::load_checked_native`, which
//! is stricter than 0.12's `load_with_options(OnPartialLoad::Error,
//! VerificationMode::Check)` in two places, both kept here:
//!
//! - every document chunk's declared heads are verified, including document
//!   chunks after the first (0.12 verifies only the first);
//! - compressed change chunks are refused: normalization has already
//!   inflated them, so one here is not normalized storage.
//!
//! As in both 0.8 and 0.12, queued (orphan) changes are accepted only when the
//! first chunk is a document chunk; mark ordering is always validated.
use crate::change_graph::ChangeGraphCols;
use crate::op_set2::change::ChangeCollector;
use crate::op_set2::op_set::MarkOrderValidator;
use crate::op_set2::OpSet;
use crate::resource_limits::NativeResourceError as Error;
use crate::storage::{self, parse, Bundle, Chunk, VerificationMode};
use crate::{Automerge, Change, TextEncoding};

type Result<T> = std::result::Result<T, Error>;

pub(super) fn load_strict(data: &[u8]) -> Result<Automerge> {
    let encoding = TextEncoding::platform_default();
    if data.is_empty() {
        return Ok(Automerge::new_with_encoding(encoding));
    }
    let (remaining, first) = Chunk::parse(parse::Input::new(data)).map_err(|_| Error::Invalid)?;
    if !first.checksum_valid() {
        return Err(Error::Invalid);
    }
    let first_document = matches!(first, Chunk::Document(_));
    let mut changes = Vec::new();
    let mut doc = match first {
        Chunk::Document(document) => document
            .reconstruct(VerificationMode::Check, encoding)
            .map_err(|_| Error::Invalid)?,
        other => {
            push_changes(other, &mut changes)?;
            Automerge::new_with_encoding(encoding)
        }
    };
    let mut input = remaining.reset();
    while !input.is_empty() {
        let (rest, chunk) = Chunk::parse(input).map_err(|_| Error::Invalid)?;
        if !chunk.checksum_valid() {
            return Err(Error::Invalid);
        }
        match chunk {
            Chunk::Document(document) => {
                if !document
                    .heads()
                    .iter()
                    .all(|head| doc.change_graph.has_change(head))
                {
                    changes.extend(document_changes(&document, encoding)?);
                }
            }
            other => push_changes(other, &mut changes)?,
        }
        input = rest.reset();
    }
    // A sole document chunk needs no batch apply (and allocates no batch state).
    if !changes.is_empty() {
        doc.apply_changes(changes).map_err(|_| Error::Invalid)?;
    }
    if !first_document && !doc.queue.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(doc)
}

fn push_changes(chunk: Chunk<'_>, changes: &mut Vec<Change>) -> Result<()> {
    match chunk {
        Chunk::Change(change) => changes.push(
            Change::new_from_unverified(change.into_owned(), None).map_err(|_| Error::Invalid)?,
        ),
        Chunk::Bundle(bundle) => {
            let bundle =
                Bundle::new_from_unverified(bundle.into_owned()).map_err(|_| Error::Invalid)?;
            changes.extend(bundle.to_changes().map_err(|_| Error::Invalid)?);
        }
        // Normalization has already inflated compressed change chunks.
        Chunk::CompressedChange(..) => return Err(Error::Invalid),
        // Callers route document chunks separately.
        Chunk::Document(_) => return Err(Error::Invalid),
    }
    Ok(())
}

/// `Document::reconstruct_changes` plus the declared-heads check that the
/// 0.8 bounded path (`reconstruct_changes_bounded`) applied to every chunk.
fn document_changes(
    document: &storage::Document<'_>,
    encoding: TextEncoding,
) -> Result<Vec<Change>> {
    let op_set = OpSet::load(document, encoding).map_err(|_| Error::Invalid)?;
    let change_cols = ChangeGraphCols::load(document).map_err(|_| Error::Invalid)?;
    let mut mark_order = MarkOrderValidator::default();
    let mut collector =
        ChangeCollector::try_new(&change_cols, &op_set).map_err(|_| Error::WorkBytes)?;
    collector
        .process_ops(&op_set, &mut mark_order)
        .map_err(|_| Error::Invalid)?;
    let collected = collector.collect(&op_set).map_err(|_| Error::Invalid)?;
    if mark_order.take_error().is_some() || !document.heads().iter().eq(collected.heads.iter()) {
        return Err(Error::Invalid);
    }
    Ok(collected.changes)
}
