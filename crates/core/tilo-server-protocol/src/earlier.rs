//! Names that state kept by an earlier build still carries: the ones this
//! crate's folders had before the server's crates took their present name.

/// The server's own folder below the state root the daemon keeps (`state_root`
/// in its server configuration).
pub(crate) const SERVER_ROOT: &str = "relay";
