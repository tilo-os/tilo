//! Guest half of the ABI: the functions [`export_provider!`](crate::export_provider)
//! wires to the module's exports.
//!
//! Every instance serves exactly one call and is then discarded by the host,
//! so allocations handed across the boundary are leaked on purpose.

use crate::{Provider, encode_detected, encode_result, pack_region};

/// Reserve `len` bytes for the host to write its input into.
pub fn alloc(len: u32) -> u32 {
    let buf = vec![0_u8; len as usize].leak();
    buf.as_mut_ptr() as u32
}

/// Borrow the host input region.
///
/// # Safety
///
/// `ptr..ptr + len` must be a region returned by [`alloc`] that the host has
/// finished writing.
unsafe fn input<'a>(ptr: u32, len: u32) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller guarantees the region came from `alloc`, which
    // leaked a live allocation of exactly this length.
    unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

fn respond(bytes: Vec<u8>) -> u64 {
    let bytes = bytes.leak();
    pack_region(bytes.as_ptr() as u32, bytes.len() as u32)
}

/// # Safety
///
/// See [`input`].
pub unsafe fn detect<P: Provider>(ptr: u32, len: u32) -> u64 {
    // SAFETY: forwarded from the caller.
    let head = unsafe { input(ptr, len) };
    respond(encode_detected(&P::detect(head)))
}

/// # Safety
///
/// See [`input`].
pub unsafe fn import_doc<P: Provider>(ptr: u32, len: u32) -> u64 {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { input(ptr, len) };
    respond(encode_result(&P::import_doc(bytes)))
}

/// # Safety
///
/// See [`input`].
pub unsafe fn export_doc<P: Provider>(ptr: u32, len: u32) -> u64 {
    // SAFETY: forwarded from the caller.
    let canonical = unsafe { input(ptr, len) };
    respond(encode_result(&P::export_doc(canonical)))
}

#[cfg(feature = "entropy")]
mod entropy {
    #[link(wasm_import_module = "tilo_fp")]
    unsafe extern "C" {
        fn fill_random(ptr: u32, len: u32);
    }

    /// `getrandom` 0.3 custom backend: ask the host for entropy.
    ///
    /// # Safety
    ///
    /// `dest..dest + len` must be writable guest memory, as `getrandom`
    /// guarantees for its custom backend.
    #[unsafe(no_mangle)]
    unsafe extern "Rust" fn __getrandom_v03_custom(
        dest: *mut u8,
        len: usize,
    ) -> Result<(), getrandom03::Error> {
        // SAFETY: the host writes exactly `len` bytes at `dest`, which the
        // caller guarantees is writable.
        unsafe { fill_random(dest as u32, len as u32) };
        Ok(())
    }
}

/// Emit the ABI exports for a [`Provider`](crate::Provider) implementation.
///
/// ```ignore
/// struct Markdown;
/// impl format_provider_abi::Provider for Markdown { /* … */ }
/// format_provider_abi::export_provider!(Markdown);
/// ```
#[macro_export]
macro_rules! export_provider {
    ($provider:ty) => {
        const _: () = {
            #[unsafe(no_mangle)]
            extern "C" fn tilo_fp_abi_version() -> i32 {
                $crate::ABI_VERSION
            }

            #[unsafe(no_mangle)]
            extern "C" fn tilo_fp_alloc(len: u32) -> u32 {
                $crate::guest::alloc(len)
            }

            #[unsafe(no_mangle)]
            unsafe extern "C" fn tilo_fp_detect(ptr: u32, len: u32) -> u64 {
                // SAFETY: the host passes a region it obtained from
                // `tilo_fp_alloc` and filled.
                unsafe { $crate::guest::detect::<$provider>(ptr, len) }
            }

            #[unsafe(no_mangle)]
            unsafe extern "C" fn tilo_fp_import(ptr: u32, len: u32) -> u64 {
                // SAFETY: as for `tilo_fp_detect`.
                unsafe { $crate::guest::import_doc::<$provider>(ptr, len) }
            }

            #[unsafe(no_mangle)]
            unsafe extern "C" fn tilo_fp_export(ptr: u32, len: u32) -> u64 {
                // SAFETY: as for `tilo_fp_detect`.
                unsafe { $crate::guest::export_doc::<$provider>(ptr, len) }
            }
        };
    };
}
