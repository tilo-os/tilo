//! Whether a raw descriptor number names a descriptor this process still has open.

/// `raw`, when the descriptor it names is open and usable: a spoofed number pointing at nothing
/// is no descriptor.
#[must_use]
pub fn open(raw: i32) -> Option<i32> {
    if raw < 0 {
        return None;
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags and never mutates or closes it.
    let rc = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    (rc != -1).then_some(raw)
}
