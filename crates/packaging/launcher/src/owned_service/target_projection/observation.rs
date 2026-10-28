//! Fixed held-object metadata and bounded selected lookup operations.
//! No content access or source permission constructor exists.
#[cfg(test)]
use super::Write;
use super::{AsRawFd, File, MetadataExt, Read, Record, UnixStream, io, namespaces, refused};
use crate::{ordinary_namespace::Handoff, owned_service::ServiceCancellation};
use getrandom::rand_core::TryRng as _;
use std::os::fd::AsFd;
use std::time::Duration;
pub(super) const MAGIC: [u8; 4] = *b"TOM1";
#[derive(Clone, Copy, Debug)]
pub(super) struct Facts {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mount_id: u64,
    pub fs_type: i64,
    pub fs_flags: u64,
}
pub(super) const FACT_BYTES: usize = 60;
impl Facts {
    pub(super) fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(FACT_BYTES);
        bytes.extend(self.dev.to_be_bytes());
        bytes.extend(self.ino.to_be_bytes());
        bytes.extend(self.mode.to_be_bytes());
        bytes.extend(self.uid.to_be_bytes());
        bytes.extend(self.gid.to_be_bytes());
        bytes.extend(self.size.to_be_bytes());
        bytes.extend(self.mount_id.to_be_bytes());
        bytes.extend(self.fs_type.to_be_bytes());
        bytes.extend(self.fs_flags.to_be_bytes());
        bytes
    }
    pub(super) fn decode(bytes: &[u8]) -> io::Result<Self> {
        fn u64_value(cursor: &mut std::io::Cursor<&[u8]>) -> io::Result<u64> {
            let mut bytes = [0; 8];
            cursor.read_exact(&mut bytes)?;
            Ok(u64::from_be_bytes(bytes))
        }
        fn u32_value(cursor: &mut std::io::Cursor<&[u8]>) -> io::Result<u32> {
            let mut bytes = [0; 4];
            cursor.read_exact(&mut bytes)?;
            Ok(u32::from_be_bytes(bytes))
        }
        let mut cursor = std::io::Cursor::new(bytes);
        if bytes.len() != FACT_BYTES {
            return Err(refused("Target observation facts length differs"));
        }
        let facts = Self {
            dev: u64_value(&mut cursor)?,
            ino: u64_value(&mut cursor)?,
            mode: u32_value(&mut cursor)?,
            uid: u32_value(&mut cursor)?,
            gid: u32_value(&mut cursor)?,
            size: u64_value(&mut cursor)?,
            mount_id: u64_value(&mut cursor)?,
            fs_type: i64::from_be_bytes({
                let mut value = [0; 8];
                cursor.read_exact(&mut value)?;
                value
            }),
            fs_flags: u64_value(&mut cursor)?,
        };
        Ok(facts)
    }
}
pub(super) fn measure(file: &File) -> io::Result<Facts> {
    let metadata = file.metadata()?;
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_DONT_SYNC,
            libc::STATX_MNT_ID,
            &raw mut stat,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(refused("Target observation mount identity unavailable"));
    }
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &raw mut fs) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut mount: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(file.as_raw_fd(), &raw mut mount) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Facts {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        size: metadata.len(),
        mount_id: stat.stx_mnt_id,
        fs_type: fs.f_type,
        fs_flags: mount.f_flag as u64,
    })
}
pub(super) fn serve(mut stream: UnixStream, record: &Record) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    let mut admitted_view = None;
    loop {
        // Idle has no expiry. Once a frame begins, one absolute deadline bounds
        // its complete exchange. Root shuts the exact channel on cancellation.
        let mut first = [0];
        match stream.read_exact(&mut first) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }
        let cancellation = ServiceCancellation::default();
        let mut io = Handoff::new(&mut stream, &cancellation, Duration::from_secs(10))?;
        let mut rest = [0; 3];
        io.read_exact(&mut rest)?;
        let lookup = rest == *b"OL1";
        let read = rest == *b"OR1";
        let exposure = rest == *b"ME1";
        if first != *b"T" || (!lookup && !read && !exposure && rest != *b"OM1") {
            return Err(refused("Target observation request differs"));
        }
        let mut nonce = [0; 32];
        let mut context = [0; 32];
        let mut source = [0; 32];
        io.read_exact(&mut nonce)?;
        io.read_exact(&mut context)?;
        io.read_exact(&mut source)?;
        if nonce == [0; 32]
            || context != record.context_sha256
            || source != record.source_view_sha256
        {
            return Err(refused("Target observation context differs"));
        }
        let (bytes, fd) = io.retry(libc::POLLIN, |socket| {
            runtime_portal::fd_transport::recv_with_fd(socket.as_fd())
        })?;
        if bytes != [0] {
            return Err(refused("Target observation descriptor framing differs"));
        }
        let file = File::from(fd.ok_or_else(|| refused("Target observation descriptor missing"))?);
        io.write_all(&[1])?;
        super::shim::verify_dropped(record.uid, record.gid)?;
        if namespaces()? != record.namespaces {
            return Err(refused("Target observation worker namespace changed"));
        }
        if read {
            super::selected_read::serve_request(
                &mut io,
                &file,
                record,
                &nonce,
                admitted_view.as_ref(),
            )?;
            continue;
        }
        if lookup {
            super::selected_lookup::serve_request(
                &mut io,
                &file,
                record,
                &nonce,
                admitted_view.as_ref(),
            )?;
            continue;
        }
        serve_measurement(&mut io, &file, record, &nonce, exposure, &mut admitted_view)?;
    }
}
fn serve_measurement(
    io: &mut Handoff<'_>,
    file: &File,
    record: &Record,
    nonce: &[u8; 32],
    exposure: bool,
    admitted_view: &mut Option<Facts>,
) -> io::Result<()> {
    let facts = match measure(file) {
        Ok(facts) => facts,
        Err(error)
            if exposure
                && error
                    .raw_os_error()
                    .is_some_and(|code| (1..=4095).contains(&code)) =>
        {
            // Actual native operation refusal keeps FUSE/session custody;
            // it is neither an object measurement nor a source grant.
            super::shim::verify_dropped(record.uid, record.gid)?;
            if namespaces()? != record.namespaces {
                return Err(refused("Exposure worker namespace changed"));
            }
            io.write_all(nonce)?;
            io.write_all(&record.context_sha256)?;
            io.write_all(&record.source_view_sha256)?;
            io.write_all(&[1])?;
            io.write_all(
                &error
                    .raw_os_error()
                    .ok_or(io::ErrorKind::InvalidData)?
                    .to_be_bytes(),
            )?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if facts.fs_type != 0x6573_5546
        || if exposure {
            admitted_view.as_ref().is_none_or(|root: &Facts| {
                (
                    facts.dev,
                    facts.ino,
                    facts.mode & libc::S_IFMT,
                    facts.uid,
                    facts.gid,
                ) != (root.dev, root.ino, libc::S_IFDIR, record.uid, record.gid)
                    || facts.mount_id == root.mount_id
                    || Some(facts.mount_id) != record.clone_mount_id
            })
        } else {
            facts.mount_id != record.mount_id.ok_or(io::ErrorKind::InvalidData)?
        }
    {
        return Err(refused(
            "Target observation is not exact admitted FUSE mount",
        ));
    }
    if admitted_view.is_none() {
        if facts.mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(refused("Initial observation is not admitted view"));
        }
        *admitted_view = Some(facts);
    }
    super::shim::verify_dropped(record.uid, record.gid)?;
    if namespaces()? != record.namespaces {
        return Err(refused("Target observation namespace retired"));
    }
    io.write_all(nonce)?;
    io.write_all(&record.context_sha256)?;
    io.write_all(&record.source_view_sha256)?;
    if exposure {
        io.write_all(&[0])?;
    }
    io.write_all(&facts.encode())?;
    Ok(())
}
pub(super) fn request(
    stream: &mut UnixStream,
    file: &File,
    record: &Record,
    cancellation: &ServiceCancellation,
) -> io::Result<Facts> {
    request_with_magic(stream, file, record, cancellation, MAGIC)
}
pub(super) enum ExposureResponse {
    Measured(Facts),
    Refused(io::Error),
}
pub(super) fn request_exposure(
    stream: &mut UnixStream,
    file: &File,
    record: &Record,
    cancellation: &ServiceCancellation,
) -> io::Result<ExposureResponse> {
    let mut nonce = [0; 32];
    getrandom::SysRng
        .try_fill_bytes(&mut nonce)
        .map_err(io::Error::other)?;
    if nonce == [0; 32] {
        return Err(refused("Exposure nonce unavailable"));
    }
    let mut io = Handoff::new(stream, cancellation, Duration::from_secs(10))?;
    io.write_all(b"TME1")?;
    io.write_all(&nonce)?;
    io.write_all(&record.context_sha256)?;
    io.write_all(&record.source_view_sha256)?;
    io.retry(libc::POLLOUT, |socket| {
        runtime_portal::fd_transport::send_with_fd(socket.as_fd(), &[0], file.as_fd())
    })?;
    let mut ack = [0];
    io.read_exact(&mut ack)?;
    if ack != [1] {
        return Err(refused("Exposure descriptor refused"));
    }
    let mut actual = [0; 32];
    let mut context = [0; 32];
    let mut source = [0; 32];
    let mut status = [0];
    io.read_exact(&mut actual)?;
    io.read_exact(&mut context)?;
    io.read_exact(&mut source)?;
    io.read_exact(&mut status)?;
    if actual != nonce || context != record.context_sha256 || source != record.source_view_sha256 {
        return Err(refused("Exposure measurement response binding differs"));
    }
    match status {
        [0] => {
            let mut facts = [0; FACT_BYTES];
            io.read_exact(&mut facts)?;
            Ok(ExposureResponse::Measured(Facts::decode(&facts)?))
        }
        [1] => {
            let mut bytes = [0; 4];
            io.read_exact(&mut bytes)?;
            let errno = i32::from_be_bytes(bytes);
            if !(1..=4095).contains(&errno) {
                return Err(refused("Exposure errno outside closed bounds"));
            }
            Ok(ExposureResponse::Refused(io::Error::from_raw_os_error(
                errno,
            )))
        }
        _ => Err(refused("Exposure status differs")),
    }
}
fn request_with_magic(
    stream: &mut UnixStream,
    file: &File,
    record: &Record,
    cancellation: &ServiceCancellation,
    magic: [u8; 4],
) -> io::Result<Facts> {
    let mut nonce = [0; 32];
    getrandom::SysRng
        .try_fill_bytes(&mut nonce)
        .map_err(io::Error::other)?;
    if nonce == [0; 32] {
        return Err(refused("Target observation nonce unavailable"));
    }
    let mut io = Handoff::new(stream, cancellation, Duration::from_secs(10))?;
    io.write_all(&magic)?;
    io.write_all(&nonce)?;
    io.write_all(&record.context_sha256)?;
    io.write_all(&record.source_view_sha256)?;
    io.retry(libc::POLLOUT, |socket| {
        runtime_portal::fd_transport::send_with_fd(socket.as_fd(), &[0], file.as_fd())
    })?;
    let mut ack = [0];
    io.read_exact(&mut ack)?;
    if ack != [1] {
        return Err(refused("Target observation descriptor refused"));
    }
    receive_response(
        &mut io,
        &nonce,
        &record.context_sha256,
        &record.source_view_sha256,
    )
}
fn receive_response(
    io: &mut Handoff<'_>,
    nonce: &[u8; 32],
    expected_context: &[u8; 32],
    expected_source: &[u8; 32],
) -> io::Result<Facts> {
    let mut actual_nonce = [0; 32];
    let mut context = [0; 32];
    let mut source = [0; 32];
    let mut facts = [0; FACT_BYTES];
    io.read_exact(&mut actual_nonce)?;
    io.read_exact(&mut context)?;
    io.read_exact(&mut source)?;
    io.read_exact(&mut facts)?;
    if &actual_nonce != nonce || &context != expected_context || &source != expected_source {
        return Err(refused("Target observation response differs"));
    }
    Facts::decode(&facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_socket_refuses_replayed_nonce_before_facts() {
        let (mut root, mut peer) = UnixStream::pair().unwrap();
        let nonce = [7; 32];
        let context = [8; 32];
        let source = [9; 32];
        peer.write_all(&[6; 32]).unwrap();
        peer.write_all(&context).unwrap();
        peer.write_all(&source).unwrap();
        peer.write_all(&[0; FACT_BYTES]).unwrap();
        let cancellation = ServiceCancellation::default();
        let mut io = Handoff::new(&mut root, &cancellation, Duration::from_secs(1)).unwrap();
        assert_eq!(
            receive_response(&mut io, &nonce, &context, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn actual_socket_refuses_other_source_or_launch_context_with_same_nonce() {
        for (reply_context, reply_source) in [([6; 32], [9; 32]), ([8; 32], [6; 32])] {
            let (mut root, mut peer) = UnixStream::pair().unwrap();
            peer.write_all(&[7; 32]).unwrap();
            peer.write_all(&reply_context).unwrap();
            peer.write_all(&reply_source).unwrap();
            peer.write_all(&[0; FACT_BYTES]).unwrap();
            let cancellation = ServiceCancellation::default();
            let mut io = Handoff::new(&mut root, &cancellation, Duration::from_secs(1)).unwrap();
            assert_eq!(
                receive_response(&mut io, &[7; 32], &[8; 32], &[9; 32])
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }
    #[test]
    fn actual_socket_observation_wait_is_absolute_and_cancelled() {
        let (mut root, _stalled) = UnixStream::pair().unwrap();
        let cancellation = ServiceCancellation::default();
        {
            let mut io = Handoff::new(&mut root, &cancellation, Duration::from_millis(50)).unwrap();
            assert_eq!(
                receive_response(&mut io, &[7; 32], &[8; 32], &[9; 32])
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
        }
        cancellation.cancel();
        let mut io = Handoff::new(&mut root, &cancellation, Duration::from_secs(1)).unwrap();
        assert_eq!(
            receive_response(&mut io, &[7; 32], &[8; 32], &[9; 32])
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
}

#[cfg(test)]
mod exposure_frame_tests {
    use super::*;
    #[test]
    fn actual_held_file_flags_roundtrip_and_old_frame_refusal() {
        let file = tempfile::tempfile().unwrap();
        let facts = measure(&file).unwrap();
        let encoded = facts.encode();
        let decoded = Facts::decode(&encoded).unwrap();
        assert_eq!(
            (decoded.dev, decoded.ino, decoded.mount_id),
            (facts.dev, facts.ino, facts.mount_id)
        );
        let mut independent: libc::statvfs = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::fstatvfs(file.as_raw_fd(), &raw mut independent) },
            0
        );
        assert_eq!(
            decoded.fs_flags & libc::ST_RDONLY,
            independent.f_flag & libc::ST_RDONLY
        );
        assert_eq!(
            Facts::decode(&encoded[..52]).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(Facts::decode(&encoded[..59]).is_err());
        assert!(Facts::decode(&[encoded, vec![0]].concat()).is_err());
    }
}
