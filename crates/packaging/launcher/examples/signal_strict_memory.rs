//! Source-runtime probe under the production strict syscall policy.
//! This grants no authority and is never packaged as the Signal service.
#[cfg(target_os = "linux")]
use launcher::seccomp::{SeccompProfile, compile_baseline, compile_clone3_compat};
use std::error::Error;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or("probe command required")?;
    // SAFETY: this scalar prctl call only restricts the current process.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    for code in [
        compile_baseline(SeccompProfile::TiloDefault)?,
        compile_clone3_compat()?,
    ] {
        install(&code)?;
    }
    // Replace this process, so the supervisor owns the actual JVM PID.
    Err(std::process::Command::new(command)
        .args(arguments)
        .exec()
        .into())
}

#[cfg(target_os = "linux")]
fn install(code: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut filters = Vec::new();
    let mut chunks = code.chunks_exact(8);
    for chunk in &mut chunks {
        let [
            opcode_low,
            opcode_high,
            jt,
            jf,
            value_a,
            value_b,
            value_c,
            value_d,
        ]: [u8; 8] = chunk
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        filters.push(libc::sock_filter {
            code: u16::from_ne_bytes([opcode_low, opcode_high]),
            jt,
            jf,
            k: u32::from_ne_bytes([value_a, value_b, value_c, value_d]),
        });
    }
    if !chunks.remainder().is_empty() {
        return Err(io::Error::from(io::ErrorKind::InvalidData).into());
    }
    let program = libc::sock_fprog {
        len: u16::try_from(filters.len())?,
        filter: filters.as_mut_ptr(),
    };
    // SAFETY: the kernel copies the program while the backing vector stays
    // live. The installed restriction is irreversible and grants no privilege.
    if unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), Box<dyn Error>> {
    Err("strict Signal runtime qualification requires Linux".into())
}
