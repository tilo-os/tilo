//! Fixed placement framing only; these descriptions cannot mint a source grant.
//! Callers must already own the actual mutually authenticated managed helper.
use crate::ordinary_namespace::Handoff;
use std::os::fd::AsFd;
use std::{fs::File, io, os::fd::OwnedFd};
const LIMIT: usize = 4096;
const MAGIC: &[u8; 4] = b"TEP1";
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlacementRecord {
    pub(crate) schema: u32,
    pub(crate) nonce: [u8; 32],
    pub(crate) context_sha256: [u8; 32],
    pub(crate) source_view_sha256: [u8; 32],
    pub(crate) target_uid: u32,
    pub(crate) target_gid: u32,
    pub(crate) clone_mount_id: u64,
    pub(crate) readonly: bool,
}
impl PlacementRecord {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || self.nonce == [0; 32]
            || self.context_sha256 == [0; 32]
            || self.source_view_sha256 == [0; 32]
            || self.clone_mount_id == 0
            || self.target_uid == 0
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Place,
    Retire,
    Witness,
}
impl Operation {
    fn byte(self) -> u8 {
        match self {
            Self::Place => 1,
            Self::Retire => 2,
            Self::Witness => 3,
        }
    }
    fn decode(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Place),
            2 => Ok(Self::Retire),
            3 => Ok(Self::Witness),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }
}
pub(crate) fn send_request(
    io: &mut Handoff<'_>,
    operation: Operation,
    record: &PlacementRecord,
) -> io::Result<()> {
    record.validate()?;
    let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    if bytes.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    io.write_all(MAGIC)?;
    io.write_all(&[operation.byte()])?;
    io.write_all(
        &u32::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_be_bytes(),
    )?;
    io.write_all(&bytes)
}
pub(crate) fn receive_request(io: &mut Handoff<'_>) -> io::Result<(Operation, PlacementRecord)> {
    let mut magic = [0; 4];
    let mut operation = [0];
    let mut length = [0; 4];
    io.read_exact(&mut magic)?;
    io.read_exact(&mut operation)?;
    io.read_exact(&mut length)?;
    if &magic != MAGIC {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let operation = Operation::decode(operation[0])?;
    let len = u32::from_be_bytes(length) as usize;
    if len == 0 || len > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes)?;
    let record: PlacementRecord = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    record.validate()?;
    Ok((operation, record))
}
pub(crate) fn send_file(io: &mut Handoff<'_>, tag: u8, file: &File) -> io::Result<()> {
    io.retry(libc::POLLOUT, |s| {
        runtime_portal::fd_transport::send_with_fd(s.as_fd(), &[tag], file.as_fd())
    })?;
    let mut ack = [0];
    io.read_exact(&mut ack)?;
    if ack != [tag] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
pub(crate) fn receive_file(io: &mut Handoff<'_>, tag: u8) -> io::Result<File> {
    let (bytes, fd) = io.retry(libc::POLLIN, |s| {
        runtime_portal::fd_transport::recv_with_fd(s.as_fd())
    })?;
    if bytes != [tag] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let file = File::from(fd.ok_or(io::ErrorKind::InvalidData)?);
    io.write_all(&[tag])?;
    Ok(file)
}
/// Every response echoes the exact closed request before a status. An errno
/// reports failure only; it never certifies clean placement or retirement.
pub(crate) fn send_result(
    io: &mut Handoff<'_>,
    operation: Operation,
    record: &PlacementRecord,
    result: io::Result<()>,
) -> io::Result<()> {
    send_request(io, operation, record)?;
    let code = match result {
        Ok(()) => 0,
        Err(error) => error
            .raw_os_error()
            .filter(|c| (1..=4095).contains(c))
            .unwrap_or(libc::EIO),
    };
    io.write_all(&code.to_be_bytes())
}
pub(crate) fn receive_result(
    io: &mut Handoff<'_>,
    operation: Operation,
    record: &PlacementRecord,
) -> io::Result<()> {
    let (observed, echo) = receive_request(io)?;
    if observed != operation || &echo != record {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut code = [0; 4];
    io.read_exact(&mut code)?;
    match i32::from_be_bytes(code) {
        0 => Ok(()),
        c if (1..=4095).contains(&c) => Err(io::Error::from_raw_os_error(c)),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned_service::ServiceCancellation;
    use std::{
        os::unix::{fs::MetadataExt, net::UnixStream},
        time::Duration,
    };
    // This is a framing description only, never authenticated admission.
    fn description() -> PlacementRecord {
        PlacementRecord {
            schema: 1,
            nonce: [1; 32],
            context_sha256: [2; 32],
            source_view_sha256: [3; 32],
            target_uid: 1000,
            target_gid: 1000,
            clone_mount_id: 99,
            readonly: true,
        }
    }
    #[test]
    fn actual_scm_preserves_exact_held_object_and_closed_echo() {
        let held = tempfile::tempfile().unwrap();
        let identity = (
            held.metadata().unwrap().dev(),
            held.metadata().unwrap().ino(),
        );
        let (mut root, mut child) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let cancel = ServiceCancellation::default();
            let mut io = Handoff::new(&mut child, &cancel, Duration::from_secs(1)).unwrap();
            let (operation, record) = receive_request(&mut io).unwrap();
            assert_eq!(operation, Operation::Place);
            let received = receive_file(&mut io, 1).unwrap();
            assert_eq!(
                (
                    received.metadata().unwrap().dev(),
                    received.metadata().unwrap().ino()
                ),
                identity
            );
            send_result(
                &mut io,
                operation,
                &record,
                Err(io::Error::from_raw_os_error(libc::EACCES)),
            )
            .unwrap();
        });
        let cancel = ServiceCancellation::default();
        let mut io = Handoff::new(&mut root, &cancel, Duration::from_secs(1)).unwrap();
        let record = description();
        send_request(&mut io, Operation::Place, &record).unwrap();
        send_file(&mut io, 1, &held).unwrap();
        assert_eq!(
            receive_result(&mut io, Operation::Place, &record)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        worker.join().unwrap();
    }
    #[test]
    fn response_nonce_substitution_is_refused() {
        let (mut root, mut child) = UnixStream::pair().unwrap();
        let mut changed = description();
        changed.nonce = [9; 32];
        let worker = std::thread::spawn(move || {
            let cancel = ServiceCancellation::default();
            let mut io = Handoff::new(&mut child, &cancel, Duration::from_secs(1)).unwrap();
            send_result(&mut io, Operation::Retire, &changed, Ok(())).unwrap();
        });
        let cancel = ServiceCancellation::default();
        let mut io = Handoff::new(&mut root, &cancel, Duration::from_secs(1)).unwrap();
        assert_eq!(
            receive_result(&mut io, Operation::Retire, &description())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        worker.join().unwrap();
    }
    #[test]
    fn cancellation_precedes_initial_framing_io() {
        use std::io::Read;

        let (mut root, mut child) = UnixStream::pair().unwrap();
        let cancel = ServiceCancellation::default();
        cancel.cancel();
        let mut io = Handoff::new(&mut root, &cancel, Duration::from_secs(1)).unwrap();
        assert_eq!(
            send_request(&mut io, Operation::Place, &description())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        child.set_nonblocking(true).unwrap();
        let mut bytes = [0; 1];
        assert_eq!(
            child.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}

impl super::ManagedConsumingBootstrap {
    pub(crate) fn place_exposure(
        &mut self,
        record: &PlacementRecord,
        clone: &File,
    ) -> io::Result<(File, File)> {
        self.check_current()?;
        if self.cancel.is_cancelled() || self.stop_requested {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut io = Handoff::new(
            &mut self.control,
            &self.cancel,
            std::time::Duration::from_secs(10),
        )?;
        io.write_all(b"E")?;
        send_request(&mut io, Operation::Place, record)?;
        send_file(&mut io, 1, clone)?;
        receive_result(&mut io, Operation::Place, record)?;
        let named = receive_file(&mut io, 2)?;
        let namespace = receive_file(&mut io, 3)?;
        drop(io);
        self.check_current()?;
        if !super::same(&namespace, &self.namespace)? {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok((named, namespace))
    }
    pub(crate) fn adopt_exposure_consumer(
        &mut self,
        record: &PlacementRecord,
        witness: &crate::ordinary_namespace::NamespaceWitness,
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<()> {
        self.verify_launcher(peer)?;
        witness.validate_for_launcher(peer)?;
        let init = File::from(witness.init_pidfd()?);
        let launcher = File::from(peer.pidfd().try_clone_to_owned()?);
        let pidns = witness.pid_namespace()?;
        let mntns = witness.mount_namespace()?;
        let mut io = Handoff::new(
            &mut self.control,
            &self.cancel,
            std::time::Duration::from_secs(10),
        )?;
        io.write_all(b"W")?;
        send_request(&mut io, Operation::Witness, record)?;
        for (tag, file) in [(4, &init), (5, &launcher), (6, &pidns), (7, &mntns)] {
            send_file(&mut io, tag, file)?;
        }
        receive_result(&mut io, Operation::Witness, record)?;
        drop(io);
        self.check_current()
    }
    /// Kernel/controller custody only: source cancellation cannot prevent
    /// cleanup. Parent has observed actual consuming init stop and closes all
    /// local aliases; child R stops and reaps its owned outer before detachment.
    pub(crate) fn retire_exposure(&mut self, record: &PlacementRecord) -> io::Result<()> {
        self.check_current()?;
        let cleanup = crate::owned_service::ServiceCancellation::default();
        let mut io = Handoff::new(
            &mut self.control,
            &cleanup,
            std::time::Duration::from_secs(10),
        )?;
        io.write_all(b"R")?;
        send_request(&mut io, Operation::Retire, record)?;
        receive_result(&mut io, Operation::Retire, record)?;
        drop(io);
        self.check_current()
    }
}

/// Minted only by the authenticated internal consuming bootstrap's sole reader.
/// Public callers cannot supply a descriptor, pathname or placement description.
pub struct ExposureRequest {
    operation: Operation,
    record: PlacementRecord,
    clone: Option<File>,
    witness: Option<(OwnedFd, OwnedFd, File, File)>,
}
/// Actual child stage lifetime, never an app/source grant. Retain this across
/// every command error. Unknown parent EOF cannot discharge a mounted stage.
#[derive(Default)]
pub struct ChildExposureOwner {
    scope: Option<PlacementRecord>,
    init: Option<OwnedFd>,
    placement: Option<crate::owned_service::target_projection::exposure::MountPlacement>,
}
impl super::ConsumingBootstrapEntry {
    pub(crate) fn read_exposure(&mut self, opcode: u8) -> io::Result<ExposureRequest> {
        self.check()?;
        let cancel = crate::owned_service::ServiceCancellation::default();
        let mut io = Handoff::new(
            &mut self.control,
            &cancel,
            std::time::Duration::from_secs(10),
        )?;
        let (operation, record) = receive_request(&mut io)?;
        if !matches!(
            (opcode, operation),
            (b'E', Operation::Place) | (b'R', Operation::Retire) | (b'W', Operation::Witness)
        ) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let clone = if operation == Operation::Place {
            Some(receive_file(&mut io, 1)?)
        } else {
            None
        };
        let witness = if operation == Operation::Witness {
            Some((
                receive_file(&mut io, 4)?.into(),
                receive_file(&mut io, 5)?.into(),
                receive_file(&mut io, 6)?,
                receive_file(&mut io, 7)?,
            ))
        } else {
            None
        };
        drop(io);
        self.check()?;
        Ok(ExposureRequest {
            operation,
            record,
            clone,
            witness,
        })
    }
}
impl ExposureRequest {
    /// Retire checks the held actual init and stops/reaps the typed owned outer
    /// internally before detaching. Wire descriptions never grant source access.
    /// Apply an authenticated exposure request to the held child custody.
    pub fn apply(
        self,
        owner: &mut ChildExposureOwner,
        entry: &mut super::ConsumingBootstrapEntry,
        mut consumer: Option<&mut super::OrdinaryChildSupervisor>,
    ) -> io::Result<()> {
        entry.check()?;
        let operation = self.operation;
        let record = self.record;
        let result: io::Result<()> = (|| match operation {
            Operation::Place => {
                if owner.scope.is_some() {
                    return Err(io::ErrorKind::AlreadyExists.into());
                }
                owner.scope = Some(record.clone());
                let clone = self.clone.ok_or(io::ErrorKind::InvalidData)?;
                owner.placement = Some(
                    crate::owned_service::target_projection::exposure::MountPlacement::allocate(
                        clone,
                        record.clone_mount_id,
                        record.readonly,
                        record.nonce,
                    )?,
                );
                owner
                    .placement
                    .as_mut()
                    .ok_or(io::ErrorKind::NotConnected)?
                    .place()
            }
            Operation::Retire => {
                if owner.scope.as_ref().is_some_and(|scope| scope != &record) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if let Some(init) = &owner.init
                    && !super::stopped(init)?
                {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                // Actual owned outer process wait/reap is mandatory even when
                // it is still waiting at the unexported view gate.
                if let Some(consumer) = consumer.as_mut() {
                    consumer.retire_before_exit();
                    if !consumer.observed_stopped() {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                } else {
                    return Err(io::ErrorKind::NotConnected.into());
                }
                if let Some(placement) = &mut owner.placement {
                    placement.retire()?;
                }
                owner.placement.take();
                owner.init.take();
                Ok(())
            }
            Operation::Witness => {
                if owner.scope.as_ref() != Some(&record) || owner.init.is_some() {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                let (init, launcher, pidns, mntns) =
                    self.witness.ok_or(io::ErrorKind::InvalidData)?;
                let consumer = consumer.as_ref().ok_or(io::ErrorKind::NotConnected)?;
                consumer.verify_launcher_pidfd(&launcher)?;
                verify_init(&init, &launcher, &pidns, &mntns)?;
                owner.init = Some(init);
                Ok(())
            }
        })();
        let success = result.is_ok();
        let cancel = crate::owned_service::ServiceCancellation::default();
        let mut io = Handoff::new(
            &mut entry.control,
            &cancel,
            std::time::Duration::from_secs(10),
        )?;
        send_result(&mut io, operation, &record, result)?;
        if success && operation == Operation::Place {
            let placement = owner
                .placement
                .as_ref()
                .ok_or(io::ErrorKind::NotConnected)?;
            send_file(&mut io, 2, placement.file()?)?;
            let ns = File::open("/proc/self/ns/mnt")?;
            send_file(&mut io, 3, &ns)?;
        }
        drop(io);
        entry.check()
    }
}
impl ChildExposureOwner {
    pub fn cleanup_observed(&self) -> bool {
        self.placement.is_none()
    }
}
impl Drop for ChildExposureOwner {
    fn drop(&mut self) {
        if self.placement.is_some() {
            // The dispatcher lost its observation/control path. Keep the actual
            // child, stage and kernel references alive; do not fake retirement.
            loop {
                std::thread::park_timeout(std::time::Duration::from_secs(1));
            }
        }
    }
}

// Kernel process observations only. These descriptors never grant file access.
fn verify_init(init: &OwnedFd, launcher: &OwnedFd, pidns: &File, mntns: &File) -> io::Result<()> {
    fn status(pid: u32) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        File::open(format!("/proc/{pid}/status"))?
            .take(16385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
    fn parent(pid: u32) -> io::Result<u32> {
        let bytes = status(pid)?;
        let value = bytes
            .split(|b| *b == b'\n')
            .find_map(|row| row.strip_prefix(b"PPid:"))
            .ok_or(io::ErrorKind::InvalidData)?
            .trim_ascii();
        std::str::from_utf8(value)
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)
    }
    let pid = super::kernel_pid(init)?;
    let launch = super::kernel_pid(launcher)?;
    if super::stopped(init)? || super::stopped(launcher)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let outer = parent(pid)?;
    if parent(outer)? != launch {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let bytes = status(pid)?;
    let ids = bytes
        .split(|b| *b == b'\n')
        .find_map(|row| row.strip_prefix(b"NSpid:"))
        .ok_or(io::ErrorKind::InvalidData)?;
    if ids.split(u8::is_ascii_whitespace).rfind(|v| !v.is_empty()) != Some(b"1".as_slice()) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    for (held, name) in [(pidns, "pid"), (mntns, "mnt")] {
        if !super::same(held, &File::open(format!("/proc/{pid}/ns/{name}"))?)?
            || super::same(held, &File::open(format!("/proc/{launch}/ns/{name}"))?)?
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    if parent(pid)? != outer
        || parent(outer)? != launch
        || super::stopped(init)?
        || super::stopped(launcher)?
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
