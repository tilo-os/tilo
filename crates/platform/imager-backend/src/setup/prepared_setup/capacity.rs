//! Frontend archive-bound bridge to native completed-setup capacity checks.
use crate::prepared_setup::capacity;
pub use capacity::completed_setup_free_bytes;
use std::io;

pub fn completed_setup_write_reservation() -> io::Result<u64> {
    capacity::completed_setup_write_reservation(maximum()?)
}
pub fn completed_setup_ciphertext_reservation(sealed_bytes: u64) -> io::Result<u64> {
    capacity::completed_setup_ciphertext_reservation(sealed_bytes, maximum()?)
}
fn maximum() -> io::Result<u64> {
    super::LIMITS.sealed_bound().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Completed setup capacity differs",
        )
    })
}
