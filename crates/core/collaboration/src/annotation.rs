//! What a collaboration transport's error text says about *why* it failed.
//!
//! A transport's one interesting distinction is whether the workspace this
//! app syncs with read a change and would not take it. That is a refusal of
//! one change: the connection is fine and the next change goes through.
//! Anything else (a dropped socket, a closed worker, an overloaded host)
//! leaves the outcome unknown, and only that may end collaboration.
//!
//! The error travels as a string, so this module is the one place that
//! reads the meaning out of it. A host that can answer typed — the native
//! broker — answers with its RPC error kind instead
//! (`crate::broker`), and does not come through here.

/// The phrasing a host uses for the one answer that is a refusal: it read
/// the change and would not commit it, and says so.
const REFUSAL: &str = "the workspace refused the change";

/// Whether `error` is the peer's definite answer about the change itself:
/// it read the change and would not take it. A lost, cancelled or
/// overloaded call is not an answer, and neither is a transport failure.
#[must_use]
pub fn refused(error: &str) -> bool {
    error.trim_start().starts_with(REFUSAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_hosts_own_refusal_reads_as_one() {
        assert!(refused("the workspace refused the change: not a member"));
        assert!(refused("  the workspace refused the change"));
        assert!(!refused("the workspace refused to connect"));
        assert!(!refused("connection closed"));
        assert!(!refused("owner commit interrupted; outcome unknown"));
        assert!(!refused("the change was too large"));
    }
}
