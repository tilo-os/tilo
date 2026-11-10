//! When a governed workspace's publication starts over from a fresh base, and
//! when a change is appended to it: what a newcomer is sent of the
//! workspace's governance, and what each join costs its host.

use super::Publication;

/// Governance publications kept after the last full checkpoint before the
/// workspace's publication is compacted into a fresh one ([`starts_over`]).
pub(super) const MAX_PUBLICATION_TAIL: usize = 256;
/// The changes' weight past which even a small workspace's publication starts
/// over ([`starts_over`]).
pub(super) const MIN_PUBLICATION_TAIL_BYTES: usize = 64 * 1024;

/// What a governance change of a workspace is, as far as starting the
/// workspace's document over for newcomers goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    /// It only admits people ([`super::collaboration`]'s admissions): it takes
    /// nothing back, whatever bookkeeping of the workspace it updates.
    Admission,
    /// Anything else: it takes something back when it overwrites or deletes
    /// a value ([`withdraws`]).
    Other,
}

/// What the workspace published since its document last started over did to
/// what was there ([`super::GovernedWorkspace::since_base`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SinceBase {
    /// It only added.
    Added,
    /// It overwrote or deleted something, or that is not known.
    Withdrew,
}

/// Whether a commit after `outgoing`, the workspace's publication since its
/// document last started over, starts it over again from a fresh base.
///
/// A newcomer is sent the base and every change after it, and a member already
/// in only the changes: so a change is appended, and the workspace starts over
/// once the changes since the base weigh as much as the base itself (or there
/// are [`MAX_PUBLICATION_TAIL`] of them). A newcomer then fetches at most
/// about twice the workspace as it stands, however long it has run, and the
/// host writes a base at most once per its own weight in appended changes:
/// each change's share of those writes is no more than its own size, so a join
/// costs the host the same in a workspace of hundreds as in one of a few.
/// Starting over at every arrival would write and send the whole workspace for
/// each person who came in. A small workspace's base is cheap to write, so it
/// starts over no more often than every [`MIN_PUBLICATION_TAIL_BYTES`]. An
/// arrival after something was taken back starts it over whatever its
/// weight ([`super::GovernedWorkspace::since_base`]).
pub(super) fn starts_over(outgoing: &[Publication]) -> bool {
    let (base, changes) = based(outgoing);
    let weight = |publications: &[Publication]| -> usize {
        publications
            .iter()
            .map(|publication| publication.bytes.len())
            .sum()
    };
    changes.len() >= MAX_PUBLICATION_TAIL
        || weight(changes) >= weight(base).max(MIN_PUBLICATION_TAIL_BYTES)
}

/// Whether `message`, published after the workspace's base, takes back
/// something of the workspace's document: one of its changes overwrites or
/// deletes a value, which a newcomer replayed the change would read as it
/// was. A change that does not read counts as one that does.
pub(super) fn withdraws(message: &neighbour_wire::NeighbourMsg) -> bool {
    let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = message else {
        return false;
    };
    changes.iter().any(|bytes| {
        automerge::Change::try_from(bytes.as_slice()).map_or(true, |change| {
            change
                .decode()
                .operations
                .iter()
                .any(|op| !op.pred.is_empty())
        })
    })
}

/// `outgoing` split into the base it starts from (the checkpoints a start
/// over published) and what was published after it.
pub(super) fn based(outgoing: &[Publication]) -> (&[Publication], &[Publication]) {
    let base = outgoing
        .iter()
        .take_while(|publication| publication.checkpoint)
        .count();
    outgoing.split_at(base)
}
