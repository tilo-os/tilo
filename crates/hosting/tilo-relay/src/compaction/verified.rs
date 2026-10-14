//! What compaction may rely on in a retained entry, and the signatures it
//! has already verified.
//!
//! The relay's admission authenticates the device that sent a delta and
//! authorises it for the workspace, but it does not open the `AppDocument`
//! inside or verify its `SignedOp`; the host's payload adapter
//! ([`ShellPayloads::open_document`]) is what reads the route and envelope. Compaction is where the host first
//! relies on that signature, so every entry it folds, counts as sealed or
//! removes has to have verified **here**, as its lineage's author, for its
//! route.
//!
//! A channel's entries are read from disk again every round, and most of
//! them were already there the round before. Verifying unchanged bytes again
//! cannot change the answer, so [`Signatures`] remembers which entry bodies
//! verified, by the BLAKE3 hash of the exact bytes the envelope was decoded
//! from:
//!
//! - The hash is taken in [`Signatures::check`] from the same buffer it
//!   decodes, so nothing outside can pair a hash with other bytes. A file
//!   changed on disk between rounds hashes differently and is verified again.
//! - Only a signature that verified is remembered; a forged or damaged entry
//!   is verified, and refused, every round.
//! - The signer and route checks are not remembered: they run every round,
//!   so a verified body copied into another author's lineage is refused.
//! - The record lives in this process's memory only. After a restart, and
//!   for every entry a peer supplied since the last round, everything is
//!   verified again.
//! - It keeps only the bodies the last round read, so it is never larger
//!   than the channel.
//!
//! The verified status travels as a [`Checked`], which only
//! [`Signatures::check`] constructs: the fold and the sealed-tail scan read
//! nothing else.

use std::collections::HashSet;
use std::sync::Arc;

use super::Route;
use crate::payloads::ShellPayloads;
use crate::store::compaction::ChannelEntry;
use collab_types::{DocumentFrame, DocumentMutation, DocumentSegment};

/// BLAKE3 of a retained entry's body.
type BodyHash = [u8; 32];

/// Entry bodies of one channel whose document signature verified in this
/// process.
pub(super) struct Signatures {
    /// Reads the route and envelope of a document frame.
    payloads: Arc<dyn ShellPayloads>,
    /// Verified bodies, as of the previous round.
    verified: HashSet<BodyHash>,
    /// Verified bodies this round has read so far.
    current: HashSet<BodyHash>,
}

/// Tests read the desktop's frames, as a relay started with
/// [`Relay::start`](crate::daemon::Relay::start) does.
#[cfg(test)]
impl Default for Signatures {
    fn default() -> Self {
        Self::with_payloads(crate::payloads::neighbour())
    }
}

/// What one entry carries, as far as the host can read it.
pub(super) enum Piece {
    Whole(DocumentMutation),
    Segment(DocumentSegment),
    Sealed,
    Opaque,
}

/// A retained entry, decoded and checked the way a receiving device checks
/// a document frame before it merges anything. Its fields are private: only
/// [`Signatures::check`] constructs one, so a readable or sealed piece here
/// always passed those checks.
pub(super) struct Checked<'a> {
    entry: &'a ChannelEntry,
    route: Option<Route>,
    piece: Piece,
}

impl<'a> Checked<'a> {
    /// The retained entry.
    pub(super) const fn entry(&self) -> &'a ChannelEntry {
        self.entry
    }

    /// The route the entry names, when it is a document frame at all.
    pub(super) const fn route(&self) -> Option<&Route> {
        self.route.as_ref()
    }

    /// Whether the entry is a verified sealed frame.
    pub(super) const fn is_sealed(&self) -> bool {
        matches!(self.piece, Piece::Sealed)
    }

    /// The route and the piece.
    pub(super) fn into_parts(self) -> (Option<Route>, Piece) {
        (self.route, self.piece)
    }
}

impl Signatures {
    /// A record that reads document frames with `payloads`.
    pub(super) fn with_payloads(payloads: Arc<dyn ShellPayloads>) -> Self {
        Self {
            payloads,
            verified: HashSet::new(),
            current: HashSet::new(),
        }
    }

    /// Decode `entry` and check it: its signature verifies (unless these
    /// exact body bytes verified before), the signer is the lineage's author,
    /// and the envelope names the route it is framed for.
    pub(super) fn check<'a>(&mut self, entry: &'a ChannelEntry) -> Checked<'a> {
        let opaque = |route| Checked {
            entry,
            route,
            piece: Piece::Opaque,
        };
        let Some((route, envelope)) = self.payloads.open_document(&entry.body) else {
            return opaque(None);
        };
        let artifact =
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        if *envelope.author.as_bytes() != entry.author || envelope.artifact != artifact {
            return opaque(Some(route));
        }
        let body = *blake3::hash(&entry.body).as_bytes();
        if !self.verified.contains(&body) && !self.current.contains(&body) {
            super::count_signature_check();
            if collab_types::verify_op(&envelope).is_err() {
                return opaque(Some(route));
            }
        }
        self.current.insert(body);
        let piece = if envelope.encryption.is_some() {
            Piece::Sealed
        } else {
            match DocumentFrame::decode(&envelope.payload) {
                Ok(DocumentFrame::Whole(mutation)) => Piece::Whole(mutation),
                Ok(DocumentFrame::Segment(segment)) => Piece::Segment(segment),
                Err(_) => Piece::Opaque,
            }
        };
        Checked {
            entry,
            route: Some(route),
            piece,
        }
    }

    /// End a round: remember what it verified, and forget bodies it did not
    /// read (removed, rewritten, or compacted away).
    pub(super) fn end_round(&mut self) {
        self.verified = std::mem::take(&mut self.current);
    }

    /// Bodies remembered as verified.
    #[cfg(test)]
    pub(super) fn remembered(&self) -> usize {
        self.verified.len()
    }
}
