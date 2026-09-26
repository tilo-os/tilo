//! `PIPELINED_POLL_V1` event poll targets: a consumer may keep several held
//! event polls outstanding as one ordered chain, so a long round trip no
//! longer leaves the host without a poll to answer after every page.
//!
//! Wire form (query names in the canonical sorted order the request proof
//! requires; the cursor is mandatory, it authenticates the consumer and is a
//! lower bound):
//!
//! `/peer/v3/events?chain=<16 hex>&cursor=<hex>&index=<decimal>&mode=chain`
//!
//! Semantics (host side, per authenticated session):
//! - `index=0` starts chain `chain`: it supersedes every other held poll of the
//!   consumer and is served from exactly `cursor`, like `mode=poll`.
//! - `index=k>0` of the current chain is answered only after index `k-1` was
//!   answered (page or empty), and is served after the end of the last page
//!   the host answered in this chain (never before `cursor`).
//! - A poll of another chain, or an index already answered, is answered empty.
//!
//! Client side, a page of index `k` is admitted only when every earlier index
//! of the chain was received and processed in order; any failure, stale epoch
//! or out-of-order answer abandons the chain and the next poll starts a new
//! one at the client's own cursor. The host therefore never skips a group the
//! client did not receive: a page is only served after the page before it in
//! the same chain, and a lost page invalidates everything after it.
//!
//! The capability is optional and negotiated beside the descriptor bits like
//! [`super::capabilities::MULTI_GROUP_REPLAY_V1`]; `mode=poll` is unchanged.
use super::{cursor::CursorToken, state};

/// Held polls one consumer may keep in one chain (host bound).
pub const MAX_CHAIN_HELD: usize = 4;

/// One chained poll's position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPoll {
    /// Client-chosen chain identity (random per chain).
    pub chain: u64,
    /// Position in the chain; 0 starts it.
    pub index: u32,
}

/// Poll target for `mode=chain`.
///
/// # Errors
///
/// Returns `state::StateError` if the inputs violate this operation's bounds or invariants.
pub fn chain_target(cursor: &[u8], poll: ChainPoll) -> Result<String, state::StateError> {
    Ok(format!(
        "/peer/v3/events?chain={:016x}&cursor={}&index={}&mode=chain",
        poll.chain,
        state::encode_cursor_url(cursor)?,
        poll.index
    ))
}

fn parse_index(index: &str) -> Option<u32> {
    if index.is_empty()
        || index.len() > 10
        || !index.bytes().all(|b| b.is_ascii_digit())
        || (index.len() > 1 && index.starts_with('0'))
    {
        return None;
    }
    index.parse().ok()
}

/// Parse an exact event poll target: `mode=poll` (optional cursor) or
/// `mode=chain` (cursor required). Returns the cursor and the chain position.
/// Canonical form only.
pub fn parse_poll_target(target: &str) -> Option<(Option<CursorToken>, Option<ChainPoll>)> {
    let query = target.strip_prefix("/peer/v3/events?")?;
    if query == "mode=poll" {
        return Some((None, None));
    }
    if let Some(rest) = query.strip_prefix("chain=") {
        let (chain, rest) = rest.split_once("&cursor=")?;
        let (cursor, rest) = rest.split_once("&index=")?;
        let index = rest.strip_suffix("&mode=chain")?;
        if chain.len() != 16
            || !chain
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let poll = ChainPoll {
            chain: u64::from_str_radix(chain, 16).ok()?,
            index: parse_index(index)?,
        };
        return Some((Some(state::decode_cursor_url(cursor).ok()?), Some(poll)));
    }
    let cursor = query.strip_prefix("cursor=")?.strip_suffix("&mode=poll")?;
    Some((Some(state::decode_cursor_url(cursor).ok()?), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chain_targets_round_trip_and_reject_non_canonical_forms() {
        let poll = ChainPoll {
            chain: 0x0123_4567_89ab_cdef,
            index: 7,
        };
        let target = chain_target(&[0xab, 0xcd], poll).unwrap();
        assert_eq!(
            target,
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=7&mode=chain"
        );
        // The proof layer accepts it (canonical sorted query).
        assert!(super::super::http_proof::Request::new("GET", &target, b"").is_ok());
        let (cursor, parsed) = parse_poll_target(&target).unwrap();
        assert_eq!(cursor.unwrap().as_bytes(), &[0xab, 0xcd]);
        assert_eq!(parsed, Some(poll));
        assert_eq!(
            parse_poll_target("/peer/v3/events?mode=poll"),
            Some((None, None))
        );
        let (cursor, chain) = parse_poll_target("/peer/v3/events?cursor=abcd&mode=poll").unwrap();
        assert!(cursor.is_some() && chain.is_none());
        for bad in [
            // Chain mode needs a cursor.
            "/peer/v3/events?chain=0123456789abcdef&index=0&mode=chain",
            "/peer/v3/events?chain=0123456789ABCDEF&cursor=abcd&index=0&mode=chain",
            "/peer/v3/events?chain=0123456789abcde&cursor=abcd&index=0&mode=chain",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=01&mode=chain",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=&mode=chain",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=99999999999&mode=chain",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=4294967296&mode=chain",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=1&mode=poll",
            "/peer/v3/events?chain=0123456789abcdef&cursor=abcd&index=1&mode=chain&x=1",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=1",
            "/peer/v3/events?cursor=abc&mode=poll",
            "/peer/v3/events?cursor=abcd&mode=poll&x=1",
        ] {
            assert!(parse_poll_target(bad).is_none(), "{bad}");
        }
    }
}
