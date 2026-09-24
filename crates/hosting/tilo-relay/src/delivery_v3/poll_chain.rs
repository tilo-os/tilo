//! PIPELINED_POLL_V1 event poll targets: a consumer may keep several held
//! event polls outstanding as one ordered chain, so a long round trip no
//! longer leaves the host without a poll to answer after every page.
//!
//! Wire form (the cursor is mandatory; it authenticates the consumer and is a
//! lower bound):
//!
//! `/peer/v3/events?cursor=<hex>&mode=chain&chain=<16 hex>&index=<decimal>`
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
    pub chain: u64,
    pub index: u32,
}

/// Poll target for `mode=chain`.
pub fn chain_target(cursor: &[u8], poll: ChainPoll) -> Result<String, state::StateError> {
    Ok(format!(
        "/peer/v3/events?cursor={}&mode=chain&chain={:016x}&index={}",
        state::encode_cursor_url(cursor)?,
        poll.chain,
        poll.index
    ))
}

/// Parse the query suffix after `cursor=<hex>&` for chain mode:
/// `mode=chain&chain=<16 hex>&index=<decimal>`. Canonical form only.
pub fn parse_chain_mode(mode: &str) -> Option<ChainPoll> {
    let rest = mode.strip_prefix("mode=chain&chain=")?;
    let (chain, index) = rest.split_once("&index=")?;
    if chain.len() != 16
        || !chain
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    if index.is_empty()
        || index.len() > 10
        || !index.bytes().all(|b| b.is_ascii_digit())
        || (index.len() > 1 && index.starts_with('0'))
    {
        return None;
    }
    Some(ChainPoll {
        chain: u64::from_str_radix(chain, 16).ok()?,
        index: index.parse().ok()?,
    })
}

/// Parse an exact event poll target: `mode=poll` (optional cursor) or
/// `mode=chain` (cursor required). Returns the cursor and the chain position.
pub fn parse_poll_target(target: &str) -> Option<(Option<CursorToken>, Option<ChainPoll>)> {
    if target == "/peer/v3/events?mode=poll" {
        return Some((None, None));
    }
    let rest = target.strip_prefix("/peer/v3/events?cursor=")?;
    let (cursor, mode) = rest.split_once('&')?;
    let chain = if mode == "mode=poll" {
        None
    } else {
        Some(parse_chain_mode(mode)?)
    };
    let cursor = state::decode_cursor_url(cursor).ok()?;
    Some((Some(cursor), chain))
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
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=7"
        );
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
            "/peer/v3/events?mode=chain&chain=0123456789abcdef&index=0",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789ABCDEF&index=0",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcde&index=0",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=01",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=99999999999",
            "/peer/v3/events?cursor=abcd&mode=chain&chain=0123456789abcdef&index=1&x=1",
            "/peer/v3/events?cursor=abcd&index=1&mode=chain&chain=0123456789abcdef",
            "/peer/v3/events?cursor=abc&mode=poll",
        ] {
            assert!(parse_poll_target(bad).is_none(), "{bad}");
        }
    }
}
