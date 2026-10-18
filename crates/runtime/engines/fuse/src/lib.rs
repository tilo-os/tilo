//! The workspace filesystem's FUSE presentation, as an engine library beside
//! the shared runtime (`libtilo_fuse_engine.so`).
//!
//! `runtime-fs-fuse` serves a directory or a selected file through FUSE over
//! a channel the launcher already admitted, on `fuser`'s session loop. Only
//! Imager's retained-source projection worker does that. Inside
//! `libruntime_dylib.so` (as runtime-fs's old `selected-fuse` feature) every
//! app process mapped and relocated it; linked into Imager's tapp it would
//! carry fuser's request parser in a tapp that is near its size budget. Here
//! it is a library of its own in the runtime bundle that only Imager names
//! `DT_NEEDED`.
//!
//! Like `runtime-dylib`, this crate re-exports the crates it provides, which
//! records them as provided by this library for the app's `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
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
///
/// # Errors
/// Names the failure that mapped to the wrong errno.
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
