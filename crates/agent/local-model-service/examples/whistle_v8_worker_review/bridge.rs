//! Single owned decoder ABI. No pointer or native error is logged.
use super::custody;
use local_model_service::speech::{MAX_TEXT_BYTES, SpeechError};
use std::ffi::{CString, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use zeroize::Zeroizing;

type Create =
    unsafe extern "C" fn(*const u8, usize, *const u8, usize, *const u8, usize) -> *mut c_void;
type Call = unsafe extern "C" fn(*mut c_void, i32, *const f32, usize, *mut u8, usize) -> i32;
type Free = unsafe extern "C" fn(*mut c_void);
pub struct Library {
    handle: NonNull<c_void>,
    _snapshot: File,
}
impl Library {
    pub fn load(path: &str, digest: &str) -> Result<Self, SpeechError> {
        let bytes = custody::verified(path, 1024 * 1024, digest)?;
        let snapshot = custody::sealed(&bytes)?;
        let path = CString::new(format!("/proc/self/fd/{}", snapshot.as_raw_fd()))
            .map_err(|_| SpeechError::ModelReceipt)?;
        // SAFETY: authenticated immutable review library, explicit local symbols;
        // runtime dependencies must be supplied by the outer attested sandbox.
        let handle =
            NonNull::new(unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) })
                .ok_or(SpeechError::ModelReceipt)?;
        Ok(Self {
            handle,
            _snapshot: snapshot,
        })
    }
    fn symbol(&self, name: &std::ffi::CStr) -> Result<NonNull<c_void>, SpeechError> {
        // SAFETY: live owned library and static NUL symbol; null always rejected.
        NonNull::new(unsafe { libc::dlsym(self.handle.as_ptr(), name.as_ptr()) })
            .ok_or(SpeechError::ModelReceipt)
    }
    pub fn decoder<'a>(
        &'a self,
        module: &[u8],
        model: &[u8],
        config: &[u8],
    ) -> Result<Decoder<'a>, SpeechError> {
        // SAFETY: exact versioned ABI exported by the authenticated review source.
        let create: Create =
            unsafe { std::mem::transmute(self.symbol(c"tilo_whistle_review_new")?.as_ptr()) };
        // SAFETY: exact signatures, library remains borrowed for decoder lifetime.
        let call: Call =
            unsafe { std::mem::transmute(self.symbol(c"tilo_whistle_review_call")?.as_ptr()) };
        // SAFETY: matching destroy entry point for the same owned opaque context.
        let free: Free =
            unsafe { std::mem::transmute(self.symbol(c"tilo_whistle_review_free")?.as_ptr()) };
        // SAFETY: input slices remain live for the call; bridge copies their bytes.
        let pointer = NonNull::new(unsafe {
            create(
                module.as_ptr(),
                module.len(),
                model.as_ptr(),
                model.len(),
                config.as_ptr(),
                config.len(),
            )
        })
        .ok_or(SpeechError::Recognition)?;
        Ok(Decoder {
            pointer,
            call,
            free,
            _library: self,
        })
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: all borrowing decoder owners have retired before library unload.
        unsafe {
            libc::dlclose(self.handle.as_ptr());
        }
    }
}
pub struct Decoder<'a> {
    pointer: NonNull<c_void>,
    call: Call,
    free: Free,
    _library: &'a Library,
}
impl Decoder<'_> {
    pub fn call(&mut self, operation: i32, samples: &[f32]) -> Result<String, SpeechError> {
        let mut output = Zeroizing::new(vec![0_u8; MAX_TEXT_BYTES + 1]);
        // SAFETY: exclusive live decoder, bounded input/output, no callback into Rust.
        let length = unsafe {
            (self.call)(
                self.pointer.as_ptr(),
                operation,
                samples.as_ptr(),
                samples.len(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        let length = usize::try_from(length).map_err(|_| SpeechError::Recognition)?;
        let bytes = output
            .get(..length)
            .filter(|_| length <= MAX_TEXT_BYTES)
            .ok_or(SpeechError::TextTooLong)?;
        let text = std::str::from_utf8(bytes).map_err(|_| SpeechError::Recognition)?;
        Ok(text.to_owned())
    }
}
impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: uniquely owned pointer; borrowed library outlives this retirement.
        unsafe {
            (self.free)(self.pointer.as_ptr());
        }
    }
}
