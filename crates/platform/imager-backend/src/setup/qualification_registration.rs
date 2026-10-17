//! Native qualification's admitted production main. This never mints a grant:
//! original-user registration still goes through pkexec and the real helper RPC.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
};
const MARKER: &str = "/run/tilo-retained-session-source-qualification";
thread_local! {
    static CURRENT: RefCell<Option<Arc<AdmittedMain>>> = const { RefCell::new(None) };
    static FRONTEND_PARENT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    schema: u32,
    receiver_sha256: String,
    bridge_sha256: String,
    registration_main_sha256: Option<String>,
}
pub struct AdmittedMain {
    program: File,
    namespace: File,
    receiver: File,
    receiver_pidfd: File,
    digest: String,
    receiver_digest: String,
}
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn digest(file: &File) -> io::Result<String> {
    use std::os::unix::fs::FileExt;
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != 0
        || before.mode() & 0o022 != 0
        || before.len() == 0
        || before.len() > 512 * 1024 * 1024
    {
        return Err(denied());
    }
    let mut h = Sha256::new();
    let mut offset = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = file.read_at(&mut buffer, offset)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        if offset > before.len() {
            return Err(denied());
        }
        h.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    if offset != before.len()
        || (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err(denied());
    }
    Ok(hex::encode(h.finalize()))
}
fn duplicate(slot: i32) -> io::Result<File> {
    let fd = unsafe { libc::fcntl(slot, libc::F_DUPFD_CLOEXEC, 256) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
impl AdmittedMain {
    /// Only the existing receiver bootstrap can provide this root-owned role.
    /// Neither a caller path nor an environment digest selects the executable.
    pub fn from_receiver() -> io::Result<Arc<Self>> {
        use std::io::Read;
        let marker = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(MARKER)?;
        let m = marker.metadata()?;
        if !m.is_file()
            || m.uid() != 0
            || m.mode() & 0o7777 != 0o444
            || m.nlink() != 1
            || m.len() > 512
        {
            return Err(denied());
        }
        let mut bytes = Vec::new();
        marker.take(513).read_to_end(&mut bytes)?;
        let b: Bootstrap = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let program_digest = b.registration_main_sha256.ok_or_else(denied)?;
        if b.schema != 1
            || program_digest.len() != 64
            || !program_digest
                .bytes()
                .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
            || b.bridge_sha256.len() != 64
        {
            return Err(denied());
        }
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                87,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
            || (peer.pid, peer.uid, peer.gid) != (1, 0, 0)
        {
            return Err(denied());
        }
        if digest(&File::open("/proc/self/exe")?)? != b.bridge_sha256 {
            return Err(denied());
        }
        let result = Arc::new(Self {
            program: duplicate(90)?,
            namespace: duplicate(89)?,
            receiver: duplicate(88)?,
            receiver_pidfd: duplicate(91)?,
            digest: program_digest,
            receiver_digest: b.receiver_sha256,
        });
        result.check()?;
        Ok(result)
    }
    pub fn check(&self) -> io::Result<()> {
        if unsafe { libc::ioctl(self.namespace.as_raw_fd(), 0xb703) } != libc::CLONE_NEWPID {
            return Err(denied());
        }
        let a = File::open("/proc/self/ns/pid")?.metadata()?;
        let b = self.namespace.metadata()?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino())
            || digest(&self.program)? != self.digest
            || digest(&self.receiver)? != self.receiver_digest
        {
            return Err(denied());
        }
        let info = std::fs::read_to_string(format!(
            "/proc/self/fdinfo/{}",
            self.receiver_pidfd.as_raw_fd()
        ))?;
        if info
            .lines()
            .filter_map(|l| l.strip_prefix("Pid:\t"))
            .collect::<Vec<_>>()
            != ["1"]
        {
            return Err(denied());
        }
        let mut poll = libc::pollfd {
            fd: self.receiver_pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 0) } != 0 {
            return Err(denied());
        }
        Ok(())
    }
    pub fn with_original<T>(
        self: &Arc<Self>,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.check()?;
        if unsafe { libc::getuid() } != 1000 || unsafe { libc::geteuid() } != 1000 {
            return Err(denied());
        }
        CURRENT.with(|slot| {
            if slot.borrow().is_some() {
                return Err(denied());
            }
            *slot.borrow_mut() = Some(self.clone());
            Ok(())
        })?;
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                CURRENT.with(|slot| *slot.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let result = operation();
        self.check()?;
        result
    }
}
/// Run frontend assertions using the live native fixture parent's admitted
/// production main. This conveys no filesystem or root grant: registration
/// still launches the genuine main and authenticates its privileged RPC.
pub fn with_frontend_parent<T>(
    program: PathBuf,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    validate_frontend_parent(&program)?;
    FRONTEND_PARENT.with(|slot| {
        if slot.borrow().is_some() {
            return Err(denied());
        }
        *slot.borrow_mut() = Some(program.clone());
        Ok(())
    })?;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FRONTEND_PARENT.with(|slot| *slot.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    let result = operation();
    validate_frontend_parent(&program)?;
    result
}
#[cfg(test)]
mod frontend_tests {
    use super::*;

    #[test]
    fn frontend_cannot_select_an_unrelated_program_or_run_before_admission() {
        for path in [
            "/proc/self/exe",
            "/usr/bin/true",
            "/proc/1/fd/not-a-descriptor",
        ] {
            let ran = std::cell::Cell::new(false);
            let result = with_frontend_parent(PathBuf::from(path), || {
                ran.set(true);
                Ok(())
            });
            assert!(result.is_err(), "unrelated program admitted: {path}");
            assert!(!ran.get(), "frontend operation ran before admission");
            assert!(current().unwrap().is_none());
        }
    }
}

fn validate_frontend_parent(program: &std::path::Path) -> io::Result<()> {
    use std::io::Read;
    // SAFETY: observes the live parent process only.
    let parent = unsafe { libc::getppid() };
    let prefix = format!("/proc/{parent}/fd/");
    let slot = program
        .to_str()
        .and_then(|s| s.strip_prefix(&prefix))
        .ok_or_else(denied)?;
    if slot.is_empty() || !slot.bytes().all(|b| b.is_ascii_digit()) {
        return Err(denied());
    }
    let marker = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(MARKER)?;
    let m = marker.metadata()?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o7777 != 0o444 || m.nlink() != 1 || m.len() > 512
    {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    marker.take(513).read_to_end(&mut bytes)?;
    let b: Bootstrap = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if b.schema != 1
        || digest(&File::open(format!("/proc/{parent}/exe"))?)? != b.bridge_sha256
        || Some(digest(&File::open(program)?)?) != b.registration_main_sha256
    {
        return Err(denied());
    }
    let ours = File::open("/proc/self/ns/pid")?.metadata()?;
    let theirs = File::open(format!("/proc/{parent}/ns/pid"))?.metadata()?;
    if (ours.dev(), ours.ino()) != (theirs.dev(), theirs.ino()) {
        return Err(denied());
    }
    Ok(())
}
pub fn current() -> io::Result<Option<PathBuf>> {
    if let Some(program) = FRONTEND_PARENT.with(|slot| slot.borrow().clone()) {
        validate_frontend_parent(&program)?;
        return Ok(Some(program));
    }
    CURRENT.with(|slot| {
        let value = slot.borrow();
        let Some(main) = value.as_ref() else {
            return Ok(None);
        };
        main.check()?;
        Ok(Some(PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            main.program.as_raw_fd()
        ))))
    })
}
