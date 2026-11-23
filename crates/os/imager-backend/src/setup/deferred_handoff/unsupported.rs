//! Hosts without the retained-source route refuse the deferred handoff.

use super::{Prepare, Prepared};
use std::io;

/// Refuse to write the private journal on a host that never installs the
/// retained original system, so the person sees the same unavailable choice
/// the guided route reports there instead of a journal nothing reads back.
pub fn prepare(_input: &Prepare<'_>) -> io::Result<Prepared> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "same-computer migration requires Linux",
    ))
}
