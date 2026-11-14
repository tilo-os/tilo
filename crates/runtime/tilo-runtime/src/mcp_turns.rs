//! The turns a session's requests take: how many work at once, how many
//! may work or wait, and what the one past that is told.

/// A session's requests that work at once; more wait for a turn. One agent
/// cannot occupy the host with every call it can send.
pub(crate) const MAX_WORKING: usize = 16;
/// A session's requests that work or wait for a turn; more are refused at
/// once, so a flood of calls neither queues without end nor makes the one
/// that cancels wait behind it.
pub(crate) const MAX_ADMITTED: usize = 64;
/// What a request past [`MAX_ADMITTED`] is refused with: that it did not
/// run, and when sending it again will be taken.
pub(super) const TOO_MANY_IN_FLIGHT: &str = "this connection already has 64 requests working or \
     waiting for a turn, so this one was not run and changed nothing; send it again once one of \
     them has answered";
const _: () = assert!(MAX_ADMITTED == 64, "TOO_MANY_IN_FLIGHT names the bound");
