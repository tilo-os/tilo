//! Native host-worker descriptor handoff. Existing runtime transport owns every
//! received descriptor and process binding pins the executing task before data.
use super::failure;
use runtime_portal::{fd_transport, process_binding};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt as _;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Duration,
};

/// Opaque proof that this directory descriptor was transferred by the exact
/// live authenticated original caller, rather than opened ambiently by root.
pub struct RetainedHomeInput {
    file: File,
    peer: std::sync::Arc<process_binding::ExecutingPeer>,
}
impl RetainedHomeInput {
    /// # Errors
    /// Rejects a stale or different authenticated peer, a non-directory or foreign-owned
    /// source, or mismatched inode or mount identity; propagates metadata lookup errors.
    pub fn validate(&self, channel: &HostChannel, named: &File) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let held = self.file.metadata()?;
        let actual = named.metadata()?;
        if !std::sync::Arc::ptr_eq(&self.peer, &channel.peer)
            || !self.peer.current()
            || !held.is_dir()
            || !actual.is_dir()
            || held.uid() != self.peer.uid()
            || (held.dev(), held.ino(), held.uid()) != (actual.dev(), actual.ino(), actual.uid())
            || fs_native::snapshot_lease::mount_id(&self.file)?
                != fs_native::snapshot_lease::mount_id(named)?
        {
            return Err(failure(
                "Retained Home descriptor differs from actual peer-selected source",
            ));
        }
        Ok(())
    }
}
pub const CHANNEL_FD: i32 = 70;

/// Authenticated transport does not authorize caller-authored output proofs.
/// Only the backend's descriptor-bound host session may send those proofs.
///
/// ```compile_fail,E0624
/// use imager_backend::capture::transport::HostChannel;
/// fn forge_output(channel: &mut HostChannel) {
///     channel.output_admission(&[0; 64]).unwrap();
/// }
/// ```
pub struct HostChannel {
    stream: UnixStream,
    peer: std::sync::Arc<process_binding::ExecutingPeer>,
}

impl HostChannel {
    pub fn original_peer(&self) -> super::process::OriginalCaller {
        super::process::OriginalCaller::authenticated(self.peer.clone())
    }
    pub fn validate_peer(&self) -> io::Result<()> {
        if self.peer.current() {
            Ok(())
        } else {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    /// # Errors
    /// Fails if the live peer cannot certify non-root original-caller credentials.
    pub fn original_caller_facts(&self) -> io::Result<super::original_caller::OriginalCallerFacts> {
        self.original_peer().original_caller_facts()
    }

    /// # Errors
    /// Propagates socket and authentication errors; rejects a peer whose PID, UID or
    /// live process identity does not match the expected child.
    pub fn authenticate(mut stream: UnixStream, child_pid: u32, uid: u32) -> io::Result<Self> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        process_binding::arm(&stream)?;
        let mut challenge = [0u8; 32];
        challenge
            .get_mut(..16)
            .ok_or_else(|| failure("missing challenge slot"))?
            .copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge
            .get_mut(16..)
            .ok_or_else(|| failure("missing challenge slot"))?
            .copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        stream.write_all(&process_binding::MAGIC)?;
        stream.write_all(&challenge)?;
        let peer = process_binding::receive(&stream, &challenge, uid)?;
        if peer.pid() != child_pid || !peer.current() {
            return Err(failure("capture host is not the exact original-user child"));
        }
        stream.write_all(&[1])?;
        Ok(Self {
            stream,
            peer: std::sync::Arc::new(peer),
        })
    }

    pub fn send(&mut self, tag: u8, file: &File) -> io::Result<()> {
        if !self.peer.current() {
            return Err(failure("capture host exited"));
        }
        fd_transport::send_with_fd(self.stream.as_fd(), &[tag], file.as_fd())?;
        // One acknowledged descriptor per frame; adjacent Unix-stream rights
        // cannot coalesce into an ambiguous source/output/input declaration.
        let mut ack = [0];
        self.stream.read_exact(&mut ack)?;
        if ack != [tag] || !self.peer.current() {
            return Err(failure("capture descriptor acknowledgment changed"));
        }
        Ok(())
    }

    /// # Errors
    /// Rejects a zero quota, an expired peer or an unexpected acknowledgment;
    /// propagates descriptor-transfer and socket errors.
    pub fn send_inventory_scratch(&mut self, file: &File, quota: u64) -> io::Result<()> {
        self.validate_peer()?;
        if quota == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut frame = [0; 9];
        frame[0] = 4;
        frame[1..].copy_from_slice(&quota.to_be_bytes());
        fd_transport::send_with_fd(self.stream.as_fd(), &frame, file.as_fd())?;
        let mut ack = [0];
        self.stream.read_exact(&mut ack)?;
        if ack != [4] {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.validate_peer()
    }
    pub fn stream(&mut self) -> &mut UnixStream {
        &mut self.stream
    }

    /// Reverse authentication is implemented by the launcher's native proof
    /// constructor. No claims are sent before the exact child was admitted.
    pub(in crate::capture) fn output_admission(&mut self, frame: &[u8; 64]) -> io::Result<()> {
        if !self.peer.current() {
            return Err(failure("capture host exited before output admission"));
        }
        process_binding::complete(&mut self.stream)?;
        self.stream
            .set_read_timeout(Some(Duration::from_secs(10)))?;
        self.stream
            .set_write_timeout(Some(Duration::from_secs(10)))?;
        if !self.peer.current() {
            return Err(failure("capture host exited during output admission"));
        }
        self.stream.write_all(frame)
    }

    pub fn authenticate_as_root(&mut self) -> io::Result<()> {
        process_binding::complete(&mut self.stream)?;
        if !self.peer.current() {
            return Err(failure("capture caller exited during authentication"));
        }
        Ok(())
    }

    /// Distinct source-input receiver; output's directory-only gate stays intact.
    /// # Errors
    /// Rejects a stale peer, missing descriptor, wrong tag or kind, or writable input;
    /// propagates descriptor-transfer, metadata, descriptor-flag and acknowledgment errors.
    pub fn receive_mounted_input(&mut self, tag: u8, directory: bool) -> io::Result<File> {
        self.validate_peer()?;
        let (bytes, descriptor) = fd_transport::recv_with_fd(self.stream.as_fd())?;
        let file =
            File::from(descriptor.ok_or_else(|| failure("mounted input descriptor missing"))?);
        let metadata = file.metadata()?;
        // A transferred source input never supplies writer authority.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE != libc::O_RDONLY {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if bytes != [tag] || metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
            return Err(failure("mounted input kind or ordering changed"));
        }
        self.validate_peer()?;
        self.stream.write_all(&[tag])?;
        Ok(file)
    }

    pub fn receive_retained_home(&mut self) -> io::Result<RetainedHomeInput> {
        let file = self.receive_mounted_input(4, true)?;
        if file.metadata()?.uid() != self.peer.uid() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(RetainedHomeInput {
            file,
            peer: self.peer.clone(),
        })
    }
    /// # Errors
    /// Rejects an expired peer, missing descriptor, wrong tag or non-directory output;
    /// propagates transport, metadata and acknowledgment errors.
    pub fn receive(&mut self, tag: u8) -> io::Result<File> {
        if !self.peer.current() {
            return Err(failure("capture caller exited"));
        }
        let (bytes, descriptor) = fd_transport::recv_with_fd(self.stream.as_fd())?;
        let file =
            File::from(descriptor.ok_or_else(|| failure("capture output descriptor missing"))?);
        if bytes != [tag] || !file.metadata()?.is_dir() || !self.peer.current() {
            return Err(failure("capture output descriptor authority changed"));
        }
        self.stream.write_all(&[tag])?;
        Ok(file)
    }
}

pub struct WorkerChannel {
    stream: UnixStream,
    root: Option<process_binding::ExecutingPeer>,
}
impl WorkerChannel {
    pub fn authenticate(mut stream: UnixStream) -> io::Result<Self> {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
            .map_err(io::Error::other)?;
        // SAFETY: retained connected stream and exact writable credential buffer.
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &raw mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if credentials.uid != 0
            || usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        process_binding::complete(&mut stream)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self { stream, root: None })
    }

    /// # Errors
    /// Propagates transport and authentication errors; rejects a root peer whose
    /// PID or live process identity differs from the expected helper.
    pub fn authenticate_root(&mut self, expected_pid: u32) -> io::Result<()> {
        process_binding::arm(&self.stream)?;
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        self.stream.write_all(&process_binding::MAGIC)?;
        self.stream.write_all(&challenge)?;
        let peer = process_binding::receive(&self.stream, &challenge, 0)?;
        if peer.pid() != expected_pid || !peer.current() {
            return Err(failure("capture broker is not the exact elevated helper"));
        }
        self.stream.write_all(&[1])?;
        self.root = Some(peer);
        Ok(())
    }

    /// # Errors
    /// Rejects an unauthenticated or expired root peer or an unexpected acknowledgment;
    /// propagates descriptor-transfer and socket errors.
    pub fn send(&mut self, tag: u8, file: &File) -> io::Result<()> {
        if !self
            .root
            .as_ref()
            .is_some_and(process_binding::ExecutingPeer::current)
        {
            return Err(failure("capture broker is not authenticated"));
        }
        fd_transport::send_with_fd(self.stream.as_fd(), &[tag], file.as_fd())?;
        let mut ack = [0];
        self.stream.read_exact(&mut ack)?;
        if ack != [tag] {
            return Err(failure("capture output acknowledgment changed"));
        }
        // A socket clone can outlive the admitted root process. A delayed ACK
        // from that clone must never acknowledge retired authority.
        self.validate_root()?;
        Ok(())
    }

    /// # Errors
    /// Rejects a missing descriptor, wrong tag or unexpected file kind; propagates
    /// descriptor-transfer, metadata and acknowledgment errors.
    pub fn receive(&mut self, tag: u8, directory: bool) -> io::Result<File> {
        let (bytes, descriptor) = fd_transport::recv_with_fd(self.stream.as_fd())?;
        let file = File::from(descriptor.ok_or_else(|| failure("capture descriptor missing"))?);
        if bytes != [tag]
            || file.metadata()?.is_dir() != directory
            || (!directory && !file.metadata()?.is_file())
        {
            return Err(failure("capture descriptor kind or ordering changed"));
        }
        self.stream.write_all(&[tag])?;
        Ok(file)
    }

    pub fn stream(&mut self) -> &mut UnixStream {
        &mut self.stream
    }
}

/// # Errors
/// Propagates descriptor-flag and metadata errors; returns `PermissionDenied`
/// unless the descriptor is a read-only block device.
pub fn validate_read_only_block_device(file: &File) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    // SAFETY: fcntl F_GETFL observes the held descriptor without a pointer.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if !file.metadata()?.file_type().is_block_device() || flags & libc::O_ACCMODE != libc::O_RDONLY
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
impl WorkerChannel {
    /// # Errors
    /// Rejects an unauthenticated or expired root peer, malformed frame, missing
    /// descriptor, zero quota, or a scratch directory with the wrong owner or mode;
    /// propagates transport, metadata and acknowledgment errors.
    pub fn receive_inventory_scratch(&mut self) -> io::Result<(File, u64)> {
        use std::os::unix::fs::MetadataExt;
        self.validate_root()?;
        let (frame, descriptor) = fd_transport::recv_with_fd(self.stream.as_fd())?;
        let file = File::from(descriptor.ok_or(io::ErrorKind::InvalidData)?);
        if frame.len() != 9 || frame.first() != Some(&4) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let quota = u64::from_be_bytes(
            frame
                .get(1..)
                .ok_or(io::ErrorKind::InvalidData)?
                .try_into()
                .map_err(io::Error::other)?,
        );
        let facts = file.metadata()?;
        if quota == 0
            || !facts.is_dir()
            || facts.uid() != rustix::process::geteuid().as_raw()
            || facts.mode() & 0o7777 != 0o700
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.validate_root()?;
        self.stream.write_all(&[4])?;
        Ok((file, quota))
    }
    /// # Errors
    /// Returns `PermissionDenied` unless an authenticated root peer is still current.
    pub fn validate_root(&self) -> io::Result<()> {
        if self
            .root
            .as_ref()
            .is_some_and(process_binding::ExecutingPeer::current)
        {
            Ok(())
        } else {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
    /// # Errors
    /// Rejects a stale or unauthenticated root peer, a missing descriptor, wrong tag
    /// or a descriptor that is not a read-only block device; propagates transport,
    /// descriptor inspection and acknowledgment errors.
    pub fn receive_read_only_media(&mut self) -> io::Result<File> {
        self.validate_root()?;
        let (bytes, descriptor) = fd_transport::recv_with_fd(self.stream.as_fd())?;
        let file = File::from(descriptor.ok_or(io::ErrorKind::InvalidData)?);
        if bytes != [81] {
            return Err(io::ErrorKind::InvalidData.into());
        }
        validate_read_only_block_device(&file)?;
        self.validate_root()?;
        self.stream.write_all(&[81])?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_rights_reject_wrong_kind_and_tag_without_mutating_source() {
        let original = tempfile::NamedTempFile::new().unwrap();
        original
            .as_file()
            .write_all(b"original source bytes")
            .unwrap();
        let (sender, receiver) = UnixStream::pair().unwrap();
        let mut receiver = WorkerChannel {
            stream: receiver,
            root: None,
        };
        fd_transport::send_with_fd(sender.as_fd(), &[2], original.as_file().as_fd()).unwrap();
        assert!(receiver.receive(1, false).is_err());
        fd_transport::send_with_fd(sender.as_fd(), &[1], original.as_file().as_fd()).unwrap();
        assert!(receiver.receive(1, true).is_err());
        assert_eq!(
            std::fs::read(original.path()).unwrap(),
            b"original source bytes"
        );
    }

    #[test]
    fn received_directory_is_the_exact_held_inode_even_after_rename() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected");
        std::fs::create_dir(&path).unwrap();
        let original = File::open(&path).unwrap();
        let facts = original.metadata().unwrap();
        std::fs::rename(&path, root.path().join("renamed")).unwrap();
        std::fs::create_dir(&path).unwrap();
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let mut receiver = WorkerChannel {
            stream: receiver,
            root: None,
        };
        fd_transport::send_with_fd(sender.as_fd(), &[1], original.as_fd()).unwrap();
        let transferred = receiver.receive(1, true).unwrap();
        let mut ack = [0];
        sender.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [1]);
        let metadata = transferred.metadata().unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), (facts.dev(), facts.ino()));
        assert_ne!(metadata.ino(), std::fs::metadata(&path).unwrap().ino());
    }
}

#[cfg(test)]
mod read_media_tests {
    use super::*;
    #[test]
    fn read_only_media_transport_refuses_plain_file_directory_and_unauthenticated_peer() {
        let plain = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_read_only_block_device(plain.as_file()).is_err());
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_read_only_block_device(&File::open(directory.path()).unwrap()).is_err());
        let (_, stream) = UnixStream::pair().unwrap();
        let mut channel = WorkerChannel { stream, root: None };
        assert_eq!(
            channel.receive_read_only_media().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
