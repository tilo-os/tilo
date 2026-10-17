//! Separate selected-app data consumer admission. Home and account proofs grant nothing here.
use serde::{Deserialize, Serialize};
use std::io;
pub const SUBSET_PROTOCOL: u32 = 2;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCapability {
    pub schema: u32,
    pub subset_protocol: u32,
    pub prepared_helper_sha256: String,
    pub qualification_script_sha256: String,
    pub native_qualification_sha256: String,
}
impl ConsumerCapability {
    /// # Errors
    /// Refuses unsupported protocols and malformed exact artifact digests.
    pub fn validate(&self) -> io::Result<()> {
        let digest = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.schema != 1
            || self.subset_protocol != SUBSET_PROTOCOL
            || !digest(&self.prepared_helper_sha256)
            || !digest(&self.qualification_script_sha256)
            || !digest(&self.native_qualification_sha256)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Application consumer proof is invalid",
            ));
        }
        Ok(())
    }
}
/// Schema1 system archives preserve their established admission. Selected app data
/// requires its own selected authenticated image proof, never a Home proof.
/// # Errors
/// Refuses missing or incompatible selected consumers.
pub fn require(capability: Option<&ConsumerCapability>, required: Option<u32>) -> io::Result<()> {
    if let Some(required) = required {
        let capability = capability.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import selected application settings",
            )
        })?;
        capability.validate()?;
        if required != capability.subset_protocol {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import selected application settings",
            ));
        }
    }
    Ok(())
}
/// Authenticate the actual installed consumer proof before target custody writes.
/// # Errors
/// Refuses old/missing consumers, an untrusted verifier, or failed native checks.
#[cfg(target_os = "linux")]
pub fn installed() -> io::Result<ConsumerCapability> {
    use std::{
        fs::OpenOptions,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        process::Command,
    };
    const VERIFIER: &str = "/usr/libexec/tilo-verify-applications-consumer-capability";
    // Every namespace edge is root-controlled; a target user cannot replace
    // the verifier with their importer or another executable between checks.
    for path in ["/", "/usr", "/usr/libexec", VERIFIER] {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(unavailable());
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(VERIFIER)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.len() > 1024 * 1024 {
        return Err(unavailable());
    }
    let mut command = Command::new(VERIFIER);
    command
        .arg("--check")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    let bytes = bounded_verifier(command, std::time::Duration::from_secs(150))?;
    // Retain the original verifier handle until all target checks finish.
    let observed = std::fs::symlink_metadata(VERIFIER)?;
    if observed.dev() != metadata.dev()
        || observed.ino() != metadata.ino()
        || file.metadata()?.ino() != metadata.ino()
    {
        return Err(unavailable());
    }
    let shape: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if !shape.is_object() {
        return Err(unavailable());
    }
    let proof: ConsumerCapability = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    proof.validate()?;
    Ok(proof)
}
#[cfg(target_os = "linux")]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "The installed system cannot use the selected applications",
    )
}

#[cfg(target_os = "linux")]
fn bounded_verifier(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> io::Result<Vec<u8>> {
    use std::{
        io::Read,
        os::fd::AsRawFd,
        process::{Child, Stdio},
        time::Instant,
    };
    struct Owned {
        child: Child,
        reaped: bool,
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.reaped {
                bounded_command::kill_tree(&mut self.child);
                let _ = self.child.wait();
            }
        }
    }
    bounded_command::own_process_tree(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut owned = Owned {
        child: command.spawn()?,
        reaped: false,
    };
    let mut stdout = owned.child.stdout.take().ok_or_else(unavailable)?;
    let fd = stdout.as_raw_fd();
    // SAFETY: fcntl only sets nonblocking mode on this privately held pipe.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(unavailable());
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(unavailable)?;
    let pid = i32::try_from(owned.child.id()).map_err(|_| unavailable())?;
    let mut bytes = Vec::new();
    let mut eof = false;
    loop {
        let mut buffer = [0u8; 4097];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(count) => {
                if bytes.len().checked_add(count).is_none_or(|n| n > 4096) {
                    return Err(unavailable());
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(unavailable()),
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: observe only our child and keep its PID reserved until exact group cleanup.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid.cast_unsigned(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(unavailable());
        }
        // SAFETY: successful waitid initializes the zeroed siginfo, including no-status case.
        if eof && unsafe { info.assume_init().si_pid() == pid } {
            bounded_command::kill_tree(&mut owned.child);
            let status = owned.child.wait()?;
            // PID is now reaped; never signal its old group a second time.
            owned.reaped = true;
            return if status.success() {
                Ok(bytes)
            } else {
                Err(unavailable())
            };
        }
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn verifier_deadline_retires_child_holding_stdout_after_leader_exit() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & echo $! > \"$1\"; exit 0", "fixture"])
            .arg(&pid_file);
        assert!(bounded_verifier(command, std::time::Duration::from_millis(250)).is_err());
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: pidfd_open observes the captured synthetic child; it grants no signal.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        } else {
            use std::os::fd::{AsRawFd, FromRawFd};
            // SAFETY: successful pidfd_open returns this uniquely owned descriptor.
            let file = unsafe { std::fs::File::from_raw_fd(fd as i32) };
            let mut poll = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: writable single pollfd retained throughout the bounded wait.
            assert_eq!(unsafe { libc::poll(&raw mut poll, 1, 1000) }, 1);
            assert_ne!(
                poll.revents & libc::POLLIN,
                0,
                "Synthetic descendant remained live"
            );
        }
    }
    #[test]
    fn verifier_rejects_oversized_output_and_nonzero_exit() {
        let mut oversized = std::process::Command::new("/bin/sh");
        oversized.args(["-c", "head -c 4097 /dev/zero"]);
        assert!(bounded_verifier(oversized, std::time::Duration::from_secs(2)).is_err());
        let mut failed = std::process::Command::new("/bin/sh");
        failed.args(["-c", "printf '{}'; exit 7"]);
        assert!(bounded_verifier(failed, std::time::Duration::from_secs(2)).is_err());
    }
}
