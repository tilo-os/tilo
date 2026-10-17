use super::*;
use crate::capture::{failure, projection, transport::HostChannel};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixStream,
    },
    process::Child,
    sync::{Arc, Mutex},
};

pub(super) struct Session {
    channel: HostChannel,
    process: HostProcess,
    identity: Incarnation,
    directory: projection::Directory,
    handed_off: bool,
    uid: u32,
}

pub(super) struct Process {
    child: Mutex<Child>,
    revoke: UnixStream,
    handed_off: std::sync::atomic::AtomicBool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Revocation is safe at every stage. Durable frontend custody still
        // owns descendant quiescence and protected-receipt cleanup.
        let _ = self.process.revoke();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.revoke.shutdown(std::net::Shutdown::Write);
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = child.wait();
    }
}

impl HostSession {
    pub(super) fn spawn_native(executable: &Path, uid: u32, output: bool) -> io::Result<Self> {
        let directory = projection::Directory::with_inventory_scratch(output)?;
        let (mut child, mut channel) = match super::host::spawn(executable, uid) {
            Ok(value) => value,
            Err(error) => {
                directory.cleanup()?;
                return Err(error);
            }
        };
        let admission = (|| {
            let identity = Incarnation::capture(child.id(), uid)?;
            let revoke = channel.stream().try_clone()?;
            Ok((identity, revoke))
        })();
        let (identity, revoke) = match admission {
            Ok(value) => value,
            Err(error) => {
                let _ = child.kill();
                child.wait()?;
                directory.cleanup()?;
                return Err(error);
            }
        };
        Ok(Self {
            inner: Session {
                channel,
                process: HostProcess {
                    inner: Arc::new(Process {
                        child: Mutex::new(child),
                        revoke,
                        handed_off: std::sync::atomic::AtomicBool::new(false),
                    }),
                },
                identity,
                directory,
                handed_off: false,
                uid,
            },
        })
    }
    pub(super) fn process_native(&self) -> HostProcess {
        self.inner.process.clone()
    }
    pub(super) fn identity_native(&self) -> Incarnation {
        self.inner.identity
    }
    pub(super) fn receipt_native(&self) -> ProjectionReceipt {
        self.inner.directory.alias.clone()
    }
    pub(super) fn handoff_native(
        &mut self,
        source: &File,
        output: Option<&File>,
        scratch: (&File, u64),
        request: &[u8],
        operation: &str,
    ) -> io::Result<()> {
        if self.inner.handed_off || request.len() > 64 * 1024 {
            return Err(failure("capture host handoff shape or state changed"));
        }
        let admission = output
            .map(|output| output_admission(source, output, operation, self.inner.uid))
            .transpose()?;
        // A partial handoff is terminal, not permission to replay descriptors.
        self.inner.handed_off = true;
        self.inner
            .process
            .inner
            .handed_off
            .store(true, std::sync::atomic::Ordering::Release);
        let input = sealed_input(request)?;
        let projected = projection::project_with_inventory_scratch(
            self.inner.identity,
            source,
            output,
            Some(scratch.0),
            &self.inner.directory,
        )?;
        let channel = &mut self.inner.channel;
        channel.send(1, &projected.source)?;
        channel.send(3, &input)?;
        channel.authenticate_as_root()?;
        channel.send_inventory_scratch(
            projected
                .scratch
                .as_ref()
                .ok_or(io::ErrorKind::InvalidData)?,
            scratch.1,
        )?;
        if let Some(output) = &projected.output {
            channel.send(2, output)?;
        }
        if let Some(frame) = admission {
            channel.output_admission(&frame)?;
        }
        Ok(())
    }
    pub(super) fn events_native(&mut self) -> io::Result<Events> {
        let stream = self.inner.channel.stream().try_clone()?;
        stream.set_read_timeout(None)?;
        Ok(Events { inner: stream })
    }
    pub(super) fn acknowledge_native(&mut self) -> io::Result<()> {
        if !self.inner.handed_off {
            return Err(failure("capture host has no admitted handoff"));
        }
        self.inner.channel.validate_peer()?;
        self.inner.channel.stream().write_all(&[1])
    }
}
impl HostProcess {
    pub(super) fn revoke_native(&self) -> io::Result<()> {
        self.inner.revoke.shutdown(std::net::Shutdown::Write)
    }
    pub(super) fn try_wait_native(&self) -> io::Result<Option<ExitStatus>> {
        self.inner
            .child
            .lock()
            .map_err(|_| failure("capture child poisoned"))?
            .try_wait()
    }
    pub(super) fn wait_native(&self) -> io::Result<ExitStatus> {
        self.inner
            .child
            .lock()
            .map_err(|_| failure("capture child poisoned"))?
            .wait()
    }
    pub(super) fn abort_native(&self) -> io::Result<()> {
        let mut child = self
            .inner
            .child
            .lock()
            .map_err(|_| failure("capture child poisoned"))?;
        if self
            .inner
            .handed_off
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(failure(
                "capture host handoff already began; exact custody is required to stop it",
            ));
        }
        let _ = child.kill();
        child.wait()?;
        Ok(())
    }
}
impl Events {
    pub(super) fn read_native(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

/// Private proof constructor: source/output identities come only from held
/// native descriptors, and the UID comes from the authenticated child.
fn output_admission(
    source: &File,
    output: &File,
    operation: &str,
    uid: u32,
) -> io::Result<[u8; 64]> {
    use std::os::unix::fs::MetadataExt;
    let operation_id = uuid::Uuid::parse_str(operation).map_err(io::Error::other)?;
    if operation_id.to_string() != operation {
        return Err(failure("capture operation must be a canonical UUID"));
    }
    if contains(source, output)? || contains(output, source)? {
        return Err(failure("capture output overlaps immutable home"));
    }
    let source = source.metadata()?;
    let output = output.metadata()?;
    if !source.is_dir() || !output.is_dir() {
        return Err(failure("capture proof requires directory descriptors"));
    }
    let mut frame = [0; 64];
    frame[..8].copy_from_slice(b"TILOCAP1");
    frame[8..12].copy_from_slice(&1u32.to_be_bytes());
    frame[12..28].copy_from_slice(operation_id.as_bytes());
    frame[28..36].copy_from_slice(&source.dev().to_be_bytes());
    frame[36..44].copy_from_slice(&source.ino().to_be_bytes());
    frame[44..52].copy_from_slice(&output.dev().to_be_bytes());
    frame[52..60].copy_from_slice(&output.ino().to_be_bytes());
    frame[60..64].copy_from_slice(&uid.to_be_bytes());
    Ok(frame)
}

fn contains(ancestor: &File, child: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let facts = ancestor.metadata()?;
    let wanted = (facts.dev(), facts.ino());
    let mut current = child.try_clone()?;
    for _ in 0..128 {
        let facts = current.metadata()?;
        let identity = (facts.dev(), facts.ino());
        if identity == wanted {
            return Ok(true);
        }
        let parent = File::open(format!("/proc/self/fd/{}/..", current.as_raw_fd()))?;
        let facts = parent.metadata()?;
        if identity == (facts.dev(), facts.ino()) {
            return Ok(false);
        }
        current = parent;
    }
    Err(failure("capture ancestry exceeds bound"))
}

fn sealed_input(bytes: &[u8]) -> io::Result<File> {
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-home-command".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_proof_binds_exact_held_identities_and_rejects_overlap() {
        use std::os::unix::fs::MetadataExt;
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let source = File::open(source.path()).unwrap();
        let output = File::open(output.path()).unwrap();
        let operation = uuid::Uuid::new_v4();
        let proof = output_admission(&source, &output, &operation.to_string(), 1000).unwrap();
        assert_eq!(&proof[..8], b"TILOCAP1");
        assert_eq!(&proof[12..28], operation.as_bytes());
        assert_eq!(
            &proof[28..36],
            &source.metadata().unwrap().dev().to_be_bytes()
        );
        assert_eq!(
            &proof[36..44],
            &source.metadata().unwrap().ino().to_be_bytes()
        );
        assert_eq!(
            &proof[44..52],
            &output.metadata().unwrap().dev().to_be_bytes()
        );
        assert_eq!(
            &proof[52..60],
            &output.metadata().unwrap().ino().to_be_bytes()
        );
        assert_eq!(&proof[60..64], &1000u32.to_be_bytes());
        assert!(output_admission(&source, &source, &operation.to_string(), 1000).is_err());
        assert!(
            output_admission(
                &source,
                &output,
                &operation.to_string().to_uppercase(),
                1000
            )
            .is_err()
        );
    }

    #[test]
    fn host_admission_refuses_root_as_original_user() {
        let error = HostSession::spawn(Path::new("/not-an-executable"), 0, false)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn request_memfd_is_sealed_against_modification() {
        let mut file = sealed_input(b"bounded command").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"bounded command");
        assert!(file.write_all(b"changed").is_err());
        assert!(file.set_len(0).is_err());
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(
            flags,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
        );
    }
}
