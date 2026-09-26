//! Closed delivery data capability assignments; constants never imply installation.
/// Canonical state upload, replay and cursor acknowledgement.
pub const STATE_REPLAY_V1: u64 = 1;
/// Ordinary private broker commands and status.
pub const PRIVATE_COMMANDS_V1: u64 = 2;
/// Disposable certified agent reads.
pub const PRIVATE_READS_V1: u64 = 4;
/// Pinned snapshot plus suffix recovery.
pub const SNAPSHOT_V1: u64 = 8;
/// Bounded resumable canonical state transfer.
pub const RESUMABLE_STATE_V1: u64 = 16;
/// Required full HTTPS data product contract; media uses a separate mask.
pub const PRODUCT_DATA_V1: u64 = 31;
/// Optional transcript-only extension: one event poll page may carry several
/// complete replay groups (`EventChunk* CommitBoundary`, repeated) in strictly
/// increasing commit order, bounded by [`MULTI_GROUP_PAGE_FRAMES`],
/// [`MULTI_GROUP_PAGE_GROUPS`] and the unchanged 1 MiB page byte budget. The
/// client ACKs the last boundary. Without this bit a page carries at most one
/// group, exactly as before.
///
/// Extension bits are never advertised in a signed descriptor (descriptors
/// stay at [`PRODUCT_DATA_V1`], so clients that predate the bit keep accepting
/// them) and are never mandatory. A host that serves the extension allows it
/// in its handshake policy beside its descriptor bits
/// ([`handshake_policy`]); a client requests it optionally and allows it
/// independently of the descriptor. Negotiation intersects both sides, so an
/// old peer on either side yields exactly the descriptor product contract.
pub const MULTI_GROUP_REPLAY_V1: u64 = 32;
/// Every transcript-only extension bit that every replay host serves.
pub const TRANSCRIPT_EXTENSIONS: u64 = MULTI_GROUP_REPLAY_V1;
/// Optional transcript-only extension: chained held event polls
/// ([`super::poll_chain`]). Unlike [`MULTI_GROUP_REPLAY_V1`] it is not part of
/// [`handshake_policy`]: only a host that implements chained polls adds it to
/// its policy ([`pipelined_policy`]), so other hosts negotiate it away and
/// their clients keep one held poll. Never advertised in a descriptor and never
/// mandatory.
pub const PIPELINED_POLL_V1: u64 = 64;
/// Every known data capability bit.
pub const KNOWN_DATA: u64 = PRODUCT_DATA_V1 | TRANSCRIPT_EXTENSIONS | PIPELINED_POLL_V1;
/// Frame bound of one multi-group page (each group keeps its own 64 frames).
pub const MULTI_GROUP_PAGE_FRAMES: usize = 512;
/// Complete groups in one multi-group page.
pub const MULTI_GROUP_PAGE_GROUPS: usize = 128;
/// Frame bound of one single-group page (and of every group).
pub const GROUP_PAGE_FRAMES: usize = 64;
/// Byte bound of every event poll page.
pub const PAGE_BYTES: usize = 1024 * 1024;
/// Reject unknown data capability bits without asserting readiness.
pub fn validate(mask: u64) -> bool {
    mask & !KNOWN_DATA == 0
}
/// Server handshake policy for a host whose descriptor advertises `descriptor`
/// and which serves every transcript extension: extensions ride only on the
/// durable replay lane and are never advertised in the descriptor itself.
pub fn handshake_policy(descriptor: u64) -> u64 {
    if descriptor & STATE_REPLAY_V1 != 0 {
        descriptor | TRANSCRIPT_EXTENSIONS
    } else {
        descriptor
    }
}
/// Server or client policy that additionally offers [`PIPELINED_POLL_V1`]
/// whenever the durable replay lane is present.
pub fn pipelined_policy(descriptor: u64) -> u64 {
    let policy = handshake_policy(descriptor);
    if descriptor & STATE_REPLAY_V1 != 0 {
        policy | PIPELINED_POLL_V1
    } else {
        policy
    }
}
/// Client policy: what it may accept for a descriptor, in addition to the
/// signed descriptor bits (transcript extensions are descriptor-independent).
pub fn client_allowed(descriptor: u64) -> u64 {
    pipelined_policy(descriptor)
}
/// Page bounds for a session that negotiated `negotiated`.
pub fn page_bounds(negotiated: u64) -> (usize, usize) {
    if negotiated & MULTI_GROUP_REPLAY_V1 != 0 {
        (MULTI_GROUP_PAGE_GROUPS, MULTI_GROUP_PAGE_FRAMES)
    } else {
        (1, GROUP_PAGE_FRAMES)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_assignments_preserve_downward_negotiation() {
        assert_eq!(
            [
                STATE_REPLAY_V1,
                PRIVATE_COMMANDS_V1,
                PRIVATE_READS_V1,
                SNAPSHOT_V1,
                RESUMABLE_STATE_V1,
                MULTI_GROUP_REPLAY_V1,
            ],
            [1, 2, 4, 8, 16, 32]
        );
        for mask in 0..=127 {
            assert!(validate(mask));
        }
        assert_eq!(PRODUCT_DATA_V1, 31);
        assert_eq!(PIPELINED_POLL_V1, 64);
        assert_eq!(KNOWN_DATA, 127);
        for bit in 7..64 {
            assert!(!validate(1 << bit));
        }
    }
    #[test]
    fn extensions_ride_only_on_replay_and_stay_out_of_the_product_mask() {
        assert_eq!(PRODUCT_DATA_V1 & TRANSCRIPT_EXTENSIONS, 0);
        assert_eq!(handshake_policy(PRODUCT_DATA_V1), 63);
        assert_eq!(handshake_policy(STATE_REPLAY_V1), 33);
        assert_eq!(handshake_policy(0), 0);
        assert_eq!(handshake_policy(PRIVATE_COMMANDS_V1), PRIVATE_COMMANDS_V1);
        assert_eq!(page_bounds(PRODUCT_DATA_V1), (1, 64));
        assert_eq!(page_bounds(KNOWN_DATA), (128, 512));
    }
    #[test]
    fn pipelined_polls_are_offered_only_by_hosts_that_opt_in() {
        // Shared policy (every replay host, including ones without chained
        // polls) never offers the bit; the opt-in policy does.
        assert_eq!(handshake_policy(PRODUCT_DATA_V1) & PIPELINED_POLL_V1, 0);
        assert_eq!(pipelined_policy(PRODUCT_DATA_V1), KNOWN_DATA);
        assert_eq!(pipelined_policy(PRIVATE_COMMANDS_V1), PRIVATE_COMMANDS_V1);
        assert_eq!(client_allowed(PRODUCT_DATA_V1), KNOWN_DATA);
        // Negotiation intersects: a client requesting the bit against a host
        // policy without it yields the previous contract.
        let requested = pipelined_policy(PRODUCT_DATA_V1);
        assert_eq!(
            requested & handshake_policy(PRODUCT_DATA_V1),
            PRODUCT_DATA_V1 | MULTI_GROUP_REPLAY_V1
        );
    }
}
