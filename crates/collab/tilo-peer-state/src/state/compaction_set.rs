//! A compaction whose snapshot spans several ops: the replica gathers its
//! pieces, the signed compaction and every segment it names, and takes them
//! only as a whole, at once. A piece alone changes nothing a replica holds.
use std::sync::Arc;

use collab_types::{SealedCompaction, SealedCompactionSegment, SignedOp, is_compaction_segment};

use super::{Route, StatePeer, err};

/// Most bytes of pieces gathered for one record: a compaction's snapshot as
/// large as one document transfer, with room for its pieces' envelopes.
const MAX_GATHERED_BYTES: usize =
    collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES + 4 * 1024 * 1024;
/// Most records gathering a compaction at once; the one gathering longest
/// goes first.
const MAX_GATHERING_RECORDS: usize = 4;

/// The pieces of segmented compactions this replica holds before it takes
/// them, per record, oldest first. Never persisted: a store sends a
/// compaction with its segments again to a replica that lacks them.
#[derive(Clone, Default)]
pub(super) struct Gathering(Arc<Vec<(Route, Vec<Arc<SignedOp>>)>>);

impl Gathering {
    fn put(&mut self, route: &Route, op: SignedOp) {
        let records = Arc::make_mut(&mut self.0);
        if !records.iter().any(|(held, _)| held == route) {
            if records.len() >= MAX_GATHERING_RECORDS {
                records.remove(0);
            }
            records.push((route.clone(), Vec::new()));
        }
        let Some((_, pieces)) = records.iter_mut().find(|(held, _)| held == route) else {
            return;
        };
        if pieces.iter().any(|held| held.signature == op.signature) {
            return;
        }
        pieces.push(Arc::new(op));
        while pieces
            .iter()
            .map(|piece| piece.payload.len())
            .sum::<usize>()
            > MAX_GATHERED_BYTES
        {
            pieces.remove(0);
        }
    }

    /// The segments `head` names, in order, when every one is gathered.
    fn segments(&self, route: &Route, head: &SignedOp) -> Option<Vec<SignedOp>> {
        let compaction = SealedCompaction::parse(head).ok()?;
        let (_, pieces) = self.0.iter().find(|(held, _)| held == route)?;
        (0..compaction.segments.len())
            .map(|index| {
                pieces
                    .iter()
                    .find(|piece| compaction.names_segment(head, index, piece))
                    .map(|piece| (**piece).clone())
            })
            .collect()
    }

    /// The newest gathered compaction of `route` whose segments are all here.
    fn complete(&self, route: &Route) -> Option<SignedOp> {
        let (_, pieces) = self.0.iter().find(|(held, _)| held == route)?;
        pieces
            .iter()
            .rev()
            .filter(|piece| !is_compaction_segment(piece))
            .find(|head| self.segments(route, head).is_some())
            .map(|head| (**head).clone())
    }

    /// Let go of what `route` gathered: its compaction was taken.
    fn forget(&mut self, route: &Route) {
        if self.0.iter().any(|(held, _)| held == route) {
            Arc::make_mut(&mut self.0).retain(|(held, _)| held != route);
        }
    }
}

impl StatePeer {
    /// Gather `op`, a piece of a segmented compaction of `route` signed for it,
    /// and answer the compaction once every piece of it is here: then it is
    /// admitted like any compaction, its segments with it
    /// ([`Self::compaction_segments`]).
    pub(super) fn gather_compaction_piece(
        &mut self,
        (app, record): (&str, &str),
        op: SignedOp,
    ) -> Result<Option<SignedOp>, String> {
        collab_types::validate_whole_document_payload_route(app, &self.core.workspace, record, &op)
            .map_err(err)?;
        if is_compaction_segment(&op) {
            SealedCompactionSegment::parse(&op).map_err(err)?;
        } else {
            SealedCompaction::parse(&op).map_err(err)?;
        }
        collab_types::verify_op(&op).map_err(err)?;
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        self.gathering.put(&route, op);
        Ok(self.gathering.complete(&route))
    }

    /// The segments of `head`, a compaction of `route`, in order: none for a
    /// compaction in one op.
    ///
    /// # Errors
    /// A segment it names is not gathered here.
    pub(super) fn compaction_segments(
        &self,
        route: &Route,
        head: &SignedOp,
    ) -> Result<Vec<SignedOp>, String> {
        if !collab_types::is_compaction_piece(head) {
            return Ok(Vec::new());
        }
        self.gathering
            .segments(route, head)
            .ok_or_else(|| "a segment of the sealed compaction is not here".into())
    }

    /// `route` took a compaction: what it gathered goes.
    pub(super) fn forget_gathered(&mut self, route: &Route) {
        self.gathering.forget(route);
    }
}

/// The segments `history`, a record's sealed history, holds behind its first
/// op when that is a segmented compaction, checked against what it names.
///
/// # Errors
/// They are not exactly the segments it names, in order.
pub(super) fn held_segments(history: &[SignedOp]) -> Result<usize, String> {
    let Some(head) = history
        .first()
        .filter(|first| first.kind == collab_types::OpKind::Compaction)
    else {
        return Ok(0);
    };
    let compaction = SealedCompaction::parse(head).map_err(err)?;
    for index in 0..compaction.segments.len() {
        let segment = history
            .get(index.saturating_add(1))
            .ok_or("a stored sealed compaction lacks a segment")?;
        if !compaction.names_segment(head, index, segment) {
            return Err("a stored sealed compaction's segment is not the one it names".into());
        }
    }
    Ok(compaction.segments.len())
}
