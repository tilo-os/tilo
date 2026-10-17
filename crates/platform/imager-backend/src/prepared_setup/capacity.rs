//! Ciphertext copy capacity for the authenticated completed setup generation.
//! Physical bytes are not inverse encrypted Home plaintext capacity.
use std::{fs::File, io, os::fd::AsRawFd};

// The completed marker is bounded to 16 KiB, the canonical receipt to 1 KiB;
// the remaining allowance covers its durable directory/checkpoint publication.
// This is part of the producer's existing 128 KiB media metadata reservation.
const COMPLETION_METADATA: u64 = 32 * 1024;
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Completed setup capacity differs",
    )
}
pub fn completed_setup_write_reservation(maximum: u64) -> io::Result<u64> {
    maximum.checked_add(COMPLETION_METADATA).ok_or_else(invalid)
}
pub fn completed_setup_ciphertext_reservation(sealed_bytes: u64, maximum: u64) -> io::Result<u64> {
    if sealed_bytes < 48 || sealed_bytes > maximum {
        return Err(invalid());
    }
    sealed_bytes
        .checked_add(COMPLETION_METADATA)
        .ok_or_else(invalid)
}
pub fn completed_setup_free_bytes(directory: &File) -> io::Result<u64> {
    if !directory.metadata()?.is_dir() {
        return Err(invalid());
    }
    // SAFETY: statvfs is a native integer output structure; all-zero initialization is valid.
    let mut filesystem: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: held directory descriptor and correctly sized native output.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), &raw mut filesystem) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if filesystem.f_flag & libc::ST_RDONLY != 0 || filesystem.f_frsize == 0 {
        return Err(invalid());
    }
    filesystem
        .f_bavail
        .checked_mul(filesystem.f_frsize)
        .ok_or_else(invalid)
}
