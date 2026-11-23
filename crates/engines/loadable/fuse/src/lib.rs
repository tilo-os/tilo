//! The workspace filesystem's FUSE presentation, as its own library in the runtime
//! bundle (`libtilo_fuse_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(all(feature = "thin", target_os = "linux"))]
extern crate runtime_dylib;

#[cfg(all(feature = "thin", target_os = "linux"))]
pub use fuser;
#[cfg(all(feature = "thin", target_os = "linux"))]
pub use runtime_fs_fuse;

/// Run the engine's own code without a FUSE device: map capability failures
/// to the errno the kernel would see, through `runtime-fs-fuse` and
/// `fuser`'s own error type.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with; release smoke checks that
/// Imager's start loads it itself.
#[cfg(all(feature = "thin", target_os = "linux"))]
pub fn self_check() -> Result<String, String> {
    use runtime_fs_fuse::directory_fuse::kernel_errno;
    use std::io;

    let cases = [
        (io::Error::from_raw_os_error(2), 2, "an OS error"),
        (io::Error::from_raw_os_error(13), 13, "a permission error"),
        (
            io::Error::other("the transport closed"),
            fuser::Errno::EIO.code(),
            "a transport failure",
        ),
    ];
    for (failure, expected, what) in cases {
        let errno = kernel_errno(failure);
        if errno != expected {
            return Err(format!(
                "{what} reached the kernel as {errno}, not {expected}"
            ));
        }
    }
    Ok("capability failures map to the kernel's errno".to_string())
}
