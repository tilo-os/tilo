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
use crate::author::Authors;
use crate::change_graph::ChangeGraphCols;
use crate::op_set2::change::batch::native_validation::validate_incoming;
use crate::op_set2::change::ChangeCollector;
use crate::op_set2::op_set::MarkOrderValidator;
use crate::op_set2::OpSet;
use crate::resource_limits::apply::budget::Charge;
use crate::resource_limits::dims;
use crate::resource_limits::NativeMutationError;
use crate::resource_limits::NativeResourceError as Error;
use crate::resource_limits::{NativeAllowance, NativeReservation};
use crate::storage::{self, parse, Bundle, Chunk};
use crate::{Automerge, Change, TextEncoding};

type Result<T> = std::result::Result<T, Error>;

/// Fragment-cache growth reserved on the load peak.
///
/// 0.12's `ChangeGraph::cache_fragment` pushes one `FragmentNode` for every
/// change whose hash has `fragment_level() >= 1`, with a dependency list that
/// can name every earlier fragment and a clock of actor width. Hashes can be
/// ground, so the count must be the exact one of the verified changes, known
/// only after reconstruction; it is reserved here before the change graph is
/// finalized (document chunk) or grown (queued-apply of change chunks).
struct Fragments<'a> {
    allowance: &'a NativeAllowance,
    peak: &'a mut NativeReservation,
    /// Level >= 1 fragments already reserved (the graph's existing ones).
    reserved: usize,
}
impl Fragments<'_> {
    fn reserve(&mut self, changes: &[Change], actors: usize, graph_changes: usize) -> Result<()> {
        let f_in = changes
            .iter()
            .filter(|change| change.hash().fragment_level() >= 1)
            .count();
        if f_in == 0 {
            return Ok(());
        }
        let bytes = dims::fragment_growth_bound(self.reserved, f_in, actors, graph_changes)?;
        self.peak.absorb(self.allowance.reserve(bytes)?)?;
        self.reserved = self.reserved.checked_add(f_in).ok_or(Error::WorkBytes)?;
        Ok(())
    }
}

pub(super) fn load_strict(
    data: &[u8],
    allowance: &NativeAllowance,
    peak: &mut NativeReservation,
) -> Result<Automerge> {
    let mut fragments = Fragments {
        allowance,
        peak,
        reserved: 0,
    };
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
        Chunk::Document(document) => reconstruct_document(&document, encoding, &mut fragments)?,
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
        let mut actors = doc.ops.actors.len();
        for change in &changes {
            actors = actors
                .checked_add(1 + change.other_actor_ids().len())
                .ok_or(Error::WorkBytes)?;
        }
        let graph_changes = doc
            .change_graph
            .len()
            .checked_add(changes.len())
            .ok_or(Error::WorkBytes)?;
        fragments.reserve(&changes, actors, graph_changes)?;
        // L2's semantic preflight simulates the exact batch (queue plus these
        // changes) so a hostile change or orphan chunk is refused before
        // 0.12's batch apply could error midway or panic. Its tables are
        // charged to this turn and released after the apply.
        let mut charge = Charge::new(fragments.allowance);
        validate_incoming(&doc, &changes, &mut charge).map_err(|error| match error {
            NativeMutationError::Resource(error) => error,
            _ => Error::Invalid,
        })?;
        doc.apply_changes(changes).map_err(|_| Error::Invalid)?;
        drop(charge);
    }
    if !first_document && !doc.queue.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(doc)
}

pub(crate) fn push_changes(chunk: Chunk<'_>, changes: &mut Vec<Change>) -> Result<()> {
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

/// `Document::reconstruct(VerificationMode::Check, ..)` with the fragment
/// reservation between change verification and graph finalization (which
/// runs `cache_fragments`). Mark order is validated strictly.
fn reconstruct_document(
    document: &storage::Document<'_>,
    encoding: TextEncoding,
    fragments: &mut Fragments<'_>,
) -> Result<Automerge> {
    let mut op_set = OpSet::load(document, encoding).map_err(|_| Error::Invalid)?;
    let change_cols = ChangeGraphCols::load(document).map_err(|_| Error::Invalid)?;
    let mut index = op_set.index_builder();
    let collector =
        ChangeCollector::try_new(&change_cols, &op_set).map_err(|_| Error::WorkBytes)?;
    let mut collector = collector.with_index(&mut index);
    collector.process_ops(&op_set).map_err(|_| Error::Invalid)?;
    let collected = collector.collect(&op_set).map_err(|_| Error::Invalid)?;
    if !document.heads().iter().eq(collected.heads.iter()) {
        return Err(Error::Invalid);
    }
    let (indexes, mut mark_order) = index.finish();
    if mark_order.take_error().is_some() {
        return Err(Error::Invalid);
    }
    fragments.reserve(
        &collected.changes,
        document.actors().len(),
        collected.changes.len(),
    )?;
    op_set.set_indexes(indexes);
    let mut authors = Authors::with_actors(change_cols.len());
    let graph = change_cols.finalize(&collected.changes, &mut authors);
    Ok(Automerge::from_parts(op_set, graph, authors))
}

/// `Document::reconstruct_changes` plus the declared-heads check that the
/// 0.8 bounded path (`reconstruct_changes_bounded`) applied to every chunk.
pub(crate) fn document_changes(
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
