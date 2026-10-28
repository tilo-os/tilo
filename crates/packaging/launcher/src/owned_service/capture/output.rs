//! Root-helper attestation received directly from its kernel-authenticated stream.
use super::{check_credentials, writable_directory};
use runtime_portal::process_binding::{self, ExecutingPeer};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, net::UnixStream},
    },
};

/// A live root broker attested disjointness; serialized facts alone cannot construct this.
pub struct CaptureOutputAdmission {
    peer: ExecutingPeer,
    source: (u64, u64),
    output: (u64, u64),
    original_uid: u32,
}
impl CaptureOutputAdmission {
    /// Reverse-authenticate the exact root helper and read its bounded operation-bound proof.
    /// The broker issues this only after checking output against both original home and snapshot.
    /// # Errors
    /// Refuses foreign/dead peers, swapped descriptors, operations or original credentials.
    pub fn receive_from_broker(
        stream: &mut UnixStream,
        source: &File,
        output: &File,
        original_uid: u32,
        operation: &str,
    ) -> io::Result<Self> {
        check_credentials(original_uid)?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let operation = uuid::Uuid::parse_str(operation).map_err(io::Error::other)?;
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length =
            u32::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::from_mut(&mut credentials).cast(),
                &raw mut length,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if credentials.uid != 0
            || usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>())
            || credentials.pid <= 0
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        process_binding::arm(stream)?;
        let mut challenge = [0; 32];
        challenge
            .get_mut(..16)
            .ok_or(io::ErrorKind::InvalidData)?
            .copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge
            .get_mut(16..)
            .ok_or(io::ErrorKind::InvalidData)?
            .copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        stream.write_all(&process_binding::MAGIC)?;
        stream.write_all(&challenge)?;
        let peer = process_binding::receive(stream, &challenge, 0)?;
        if peer.pid() != credentials.pid.cast_unsigned() || !peer.current() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        stream.write_all(&[1])?;
        let mut frame = [0; 64];
        stream.read_exact(&mut frame)?;
        let source_metadata = source.metadata()?;
        let output_metadata = output.metadata()?;
        let source_identity = (source_metadata.dev(), source_metadata.ino());
        let output_identity = (output_metadata.dev(), output_metadata.ino());
        if frame.get(..8) != Some(b"TILOCAP1".as_slice())
            || integer::<4>(&frame, 8)? != 1
            || frame.get(12..28) != Some(operation.as_bytes().as_slice())
            || (integer::<8>(&frame, 28)?, integer::<8>(&frame, 36)?) != source_identity
            || (integer::<8>(&frame, 44)?, integer::<8>(&frame, 52)?) != output_identity
            || integer::<4>(&frame, 60)? != u64::from(original_uid)
            || !peer.current()
            || !source_metadata.is_dir()
            || !output_metadata.is_dir()
            || source_identity == output_identity
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        writable_directory(output)?;
        Ok(Self {
            peer,
            source: source_identity,
            output: output_identity,
            original_uid,
        })
    }
    pub(super) fn check(&self, source: &File, output: &File, original_uid: u32) -> io::Result<()> {
        let left = source.metadata()?;
        let right = output.metadata()?;
        if !self.peer.current()
            || self.original_uid != original_uid
            || self.source != (left.dev(), left.ino())
            || self.output != (right.dev(), right.ino())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        writable_directory(output)
    }
}
fn integer<const N: usize>(frame: &[u8], start: usize) -> io::Result<u64> {
    let bytes: [u8; N] = frame
        .get(start..start.checked_add(N).ok_or(io::ErrorKind::InvalidData)?)
        .ok_or(io::ErrorKind::InvalidData)?
        .try_into()
        .map_err(io::Error::other)?;
    Ok(bytes
        .iter()
        .fold(0, |value, byte| (value << 8) | u64::from(*byte)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receive(
        operation: uuid::Uuid,
        claimed: uuid::Uuid,
        swapped: bool,
    ) -> io::Result<CaptureOutputAdmission> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let output = root.path().join("output");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&output)?;
        let source = File::open(source)?;
        let output = File::open(output)?;
        let uid = rustix::process::geteuid().as_raw();
        let (mut receiver, mut broker) = UnixStream::pair()?;
        receiver.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
        receiver.set_write_timeout(Some(tilo_test_env::hang_bound()))?;
        let left = source.metadata()?;
        let right = output.metadata()?;
        let sender = std::thread::spawn(move || -> io::Result<()> {
            process_binding::complete(&mut broker)?;
            let mut frame = Vec::from(b"TILOCAP1".as_slice());
            frame.extend_from_slice(&1_u32.to_be_bytes());
            frame.extend_from_slice(claimed.as_bytes());
            for fact in [
                left.dev(),
                left.ino(),
                right.dev(),
                right
                    .ino()
                    .checked_add(u64::from(swapped))
                    .ok_or_else(|| io::Error::other("fixture inode overflow"))?,
            ] {
                frame.extend_from_slice(&fact.to_be_bytes());
            }
            frame.extend_from_slice(&uid.to_be_bytes());
            broker.write_all(&frame)
        });
        let result = CaptureOutputAdmission::receive_from_broker(
            &mut receiver,
            &source,
            &output,
            uid,
            &operation.to_string(),
        );
        drop(receiver);
        let sent = sender
            .join()
            .map_err(|_| io::Error::other("broker fixture panicked"))?;
        if uid == 0 {
            sent?;
        }
        result
    }
    #[test]
    fn authenticated_output_proof_binds_real_descriptors_and_operation() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::switch_identity(65534),
            "the broker proof binds descriptors a root broker hands over"
        );
        let operation = uuid::Uuid::new_v4();
        assert!(receive(operation, operation, false).is_ok());
        assert!(receive(operation, uuid::Uuid::new_v4(), false).is_err());
        assert!(receive(operation, operation, true).is_err());
    }
}
