//! Official bundled LevelDB C API with strict recovery/checksums, never repair.
use super::{MAX_VALUE_BYTES, SourceError};
use leveldb_sys as ffi;
use std::{ffi::CString, os::unix::ffi::OsStrExt, path::Path, ptr::NonNull};
use zeroize::{Zeroize, Zeroizing};

pub(super) struct Database(NonNull<ffi::leveldb_t>);
impl Drop for Database {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the successful leveldb_open handle.
        unsafe { ffi::leveldb_close(self.0.as_ptr()) }
    }
}
struct Options(NonNull<ffi::leveldb_options_t>);
impl Drop for Options {
    fn drop(&mut self) {
        unsafe { ffi::leveldb_options_destroy(self.0.as_ptr()) }
    }
}
struct ReadOptions(NonNull<ffi::leveldb_readoptions_t>);
impl Drop for ReadOptions {
    fn drop(&mut self) {
        unsafe { ffi::leveldb_readoptions_destroy(self.0.as_ptr()) }
    }
}
struct Allocation {
    pointer: NonNull<libc::c_char>,
    length: usize,
}
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: leveldb_get returns exactly this many malloc-owned bytes; wipe
        // before returning them to the library allocator, including error paths.
        unsafe {
            std::slice::from_raw_parts_mut(self.pointer.as_ptr().cast::<u8>(), self.length)
                .zeroize();
            ffi::leveldb_free(self.pointer.as_ptr().cast());
        }
    }
}
fn check_error(error: *mut libc::c_char) -> Result<(), SourceError> {
    if error.is_null() {
        return Ok(());
    }
    // Native messages may contain private source metadata. Never format/log them.
    unsafe { ffi::leveldb_free(error.cast()) }
    Err(SourceError::UnsupportedSchema)
}
impl Database {
    pub(super) fn open(path: &Path, create: bool) -> Result<Self, SourceError> {
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|_| SourceError::Unavailable)?;
        let options = Options(
            NonNull::new(unsafe { ffi::leveldb_options_create() })
                .ok_or(SourceError::Unavailable)?,
        );
        unsafe {
            ffi::leveldb_options_set_create_if_missing(options.0.as_ptr(), u8::from(create));
            ffi::leveldb_options_set_paranoid_checks(options.0.as_ptr(), 1);
            ffi::leveldb_options_set_max_open_files(options.0.as_ptr(), 32);
            ffi::leveldb_options_set_compression(options.0.as_ptr(), ffi::Compression::Snappy);
        }
        let mut error = std::ptr::null_mut();
        let result =
            unsafe { ffi::leveldb_open(options.0.as_ptr(), path.as_ptr(), &raw mut error) };
        let database = NonNull::new(result).map(Self);
        check_error(error)?;
        database.ok_or(SourceError::UnsupportedSchema)
    }
    pub(super) fn get(&self, key: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, SourceError> {
        let options = ReadOptions(
            NonNull::new(unsafe { ffi::leveldb_readoptions_create() })
                .ok_or(SourceError::Unavailable)?,
        );
        unsafe {
            ffi::leveldb_readoptions_set_verify_checksums(options.0.as_ptr(), 1);
            ffi::leveldb_readoptions_set_fill_cache(options.0.as_ptr(), 0);
        }
        let mut length = 0;
        let mut error = std::ptr::null_mut();
        let pointer = unsafe {
            ffi::leveldb_get(
                self.0.as_ptr(),
                options.0.as_ptr(),
                key.as_ptr().cast(),
                key.len(),
                &raw mut length,
                &raw mut error,
            )
        };
        let allocation = NonNull::new(pointer).map(|pointer| Allocation { pointer, length });
        check_error(error)?;
        let Some(allocation) = allocation else {
            return Ok(None);
        };
        if length > MAX_VALUE_BYTES {
            return Err(SourceError::LimitExceeded);
        }
        // SAFETY: ownership and length are retained by Allocation through copying.
        let bytes =
            unsafe { std::slice::from_raw_parts(allocation.pointer.as_ptr().cast::<u8>(), length) };
        Ok(Some(Zeroizing::new(bytes.to_vec())))
    }
    #[cfg(test)]
    pub(super) fn compact(&self) {
        // The C API accepts null bounds to compact the complete fixture range.
        unsafe {
            ffi::leveldb_compact_range(self.0.as_ptr(), std::ptr::null(), 0, std::ptr::null(), 0)
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn put(&self, key: &[u8], value: Option<&[u8]>) {
        let options = unsafe { ffi::leveldb_writeoptions_create() };
        assert!(!options.is_null());
        let mut error = std::ptr::null_mut();
        unsafe {
            ffi::leveldb_writeoptions_set_sync(options, 1);
            if let Some(value) = value {
                ffi::leveldb_put(
                    self.0.as_ptr(),
                    options,
                    key.as_ptr().cast(),
                    key.len(),
                    value.as_ptr().cast(),
                    value.len(),
                    &raw mut error,
                );
            } else {
                ffi::leveldb_delete(
                    self.0.as_ptr(),
                    options,
                    key.as_ptr().cast(),
                    key.len(),
                    &raw mut error,
                );
            }
            ffi::leveldb_writeoptions_destroy(options);
        }
        check_error(error).expect("actual native fixture mutation");
    }
}
