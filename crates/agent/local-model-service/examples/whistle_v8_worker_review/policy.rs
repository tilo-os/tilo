//! Actual production policy compatibility, independent of a decoder or model.
use local_model_service::speech::SpeechError;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use local_model_service::speech::confine_worker;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct Mapping(*mut libc::c_void, usize);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this owner retains the exact live mapping and its size.
        unsafe {
            libc::munmap(self.0, self.1);
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn probe() -> Result<(), SpeechError> {
    confine_worker()?;
    let size = 8192;
    // SAFETY: private anonymous mapping, owned only by this fresh single-threaded
    // qualification process; no executable or untrusted input is mapped writable.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(SpeechError::WorkerStopped);
    }
    let owned = Mapping(mapping, size);
    // Known x86-64 ENDBR64; xor eax,eax; ret. No model, loaded code or caller data.
    let code = [0xf3_u8, 0x0f, 0x1e, 0xfa, 0x31, 0xc0, 0xc3];
    // SAFETY: writable mapping is larger than this fixed, reviewed instruction span.
    unsafe {
        std::ptr::copy_nonoverlapping(code.as_ptr(), owned.0.cast(), code.len());
    }
    // SAFETY: removes write access before allowing execution. Neither transition
    // asks for writable executable pages, including during instruction copying.
    if unsafe { libc::mprotect(owned.0, owned.1, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
        return Err(SpeechError::WorkerStopped);
    }
    // SAFETY: exact reviewed x86-64 instruction sequence returns integer zero;
    // the mapping is executable, non-writable, exclusively owned and live.
    let execute: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(owned.0) };
    // SAFETY: valid zero-argument function above; no untrusted code is executed.
    if unsafe { execute() } != 0 {
        return Err(SpeechError::WorkerStopped);
    }
    let wx = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
    // SAFETY: exact live page; the production filter must reject W+X.
    if unsafe { libc::mprotect(owned.0, owned.1, wx) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
    {
        return Err(SpeechError::WorkerStopped);
    }
    // SAFETY: repeat the exact failed V8 mapping shape and key argument. The
    // filter rejects it before the kernel examines key validity or CPU support.
    if unsafe { libc::syscall(libc::SYS_pkey_mprotect, owned.0, owned.1, wx, 1) } != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
    {
        return Err(SpeechError::WorkerStopped);
    }
    // Keep the finished owned subject observable to the resource guard.
    // This fixed hold is not an inference or latency measurement.
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!(
        "SPEECH_POLICY_PROBE {{\"rw_to_rx_executes\":true,\"mprotect_wx_eperm\":true,\"exact_v8_pkey_wx_eperm\":true,\"production_confinement\":true}}"
    );
    Ok(())
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub fn probe() -> Result<(), SpeechError> {
    // This reviewed instruction fixture is x86-64 only; unsupported execution
    // must fail instead of claiming a skipped architecture passed qualification.
    Err(SpeechError::WorkerStopped)
}
