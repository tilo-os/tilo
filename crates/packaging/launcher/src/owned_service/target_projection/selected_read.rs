//! Exact lookup-bound readonly reopen by the signed fully dropped worker.
//! Source FsSession/role remains the authority; the held LL rule only enables
//! asking this exact backend. No arbitrary callback, root path, or edit upgrade.
use super::{AsRawFd, File, FromRawFd, Record, UnixStream, io, refused, validate_name};
#[cfg(test)]
use super::{OpenOptionsExt, Read, Write};
use super::{
    observation::{Facts, measure},
    selected_lookup as lookup,
};
use crate::{ordinary_namespace::Handoff, owned_service::ServiceCancellation};
use getrandom::rand_core::TryRng as _;
use std::os::fd::AsFd;
use std::{ffi::CString, time::Duration};
const MAGIC: [u8; 4] = *b"TOR1";
#[derive(Clone, Copy)]
pub(super) struct ReadRequest<'a> {
    pub view: &'a File,
    pub parent: &'a File,
    pub selected: &'a File,
    pub selected_facts: &'a Facts,
    pub components: &'a [Vec<u8>],
}
pub(super) enum Response {
    Opened(File, Facts),
    Refused(io::Error),
}
fn receive_file(io: &mut Handoff<'_>, marker: u8) -> io::Result<File> {
    let (bytes, fd) = io.retry(libc::POLLIN, |socket| {
        runtime_portal::fd_transport::recv_with_fd(socket.as_fd())
    })?;
    if bytes != [marker] {
        return Err(refused("Selected read descriptor order differs"));
    }
    let file = File::from(fd.ok_or_else(|| refused("Selected read descriptor missing"))?);
    io.write_all(&[1])?;
    Ok(file)
}
fn send_file(io: &mut Handoff<'_>, marker: u8, file: &File) -> io::Result<()> {
    io.retry(libc::POLLOUT, |socket| {
        runtime_portal::fd_transport::send_with_fd(socket.as_fd(), &[marker], file.as_fd())
    })?;
    let mut ack = [0];
    io.read_exact(&mut ack)?;
    if ack != [1] {
        return Err(refused("Selected read custody refused"));
    }
    Ok(())
}
fn open_readonly(parent: &File, leaf: &[u8]) -> io::Result<File> {
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    validate_name(leaf)?;
    let leaf = CString::new(leaf).map_err(io::Error::other)?;
    let how = OpenHow {
        flags: u64::try_from(libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .map_err(io::Error::other)?,
        mode: 0,
        resolve: 0x01 | 0x02 | 0x04 | 0x08,
    };
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            leaf.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(i32::try_from(raw).map_err(io::Error::other)?) })
}
pub(super) fn check_read_held(file: &File, facts: &Facts) -> io::Result<()> {
    lookup::check_kernel_identity(file, facts)?;
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_PATH != 0
        || flags & libc::O_ACCMODE != libc::O_RDONLY
        || facts.mode & libc::S_IFMT != libc::S_IFREG
    {
        return Err(refused("Selected read is not actual readonly regular OFD"));
    }
    Ok(())
}
fn read_from_selected(parent: &File, selected: &File, leaf: &[u8]) -> io::Result<(File, Facts)> {
    let before = measure(selected)?;
    let file = open_readonly(parent, leaf)?;
    let facts = measure(&file)?;
    check_read_held(&file, &facts)?;
    if !lookup::same_object(&facts, &before) || !lookup::same_object(&measure(selected)?, &facts) {
        return Err(refused("Selected read readonly reopen inode differs"));
    }
    Ok((file, facts))
}
fn reopen(
    request: &ReadRequest<'_>,
    record: &Record,
    view_facts: &Facts,
) -> io::Result<(File, Facts)> {
    let parent_before = measure(request.parent)?;
    let selected_before = measure(request.selected)?;
    lookup::check_held(request.parent, &parent_before)?;
    lookup::check_held(request.selected, &selected_before)?;
    let current = lookup::lookup(request.view, request.components, record, view_facts)?;
    if !lookup::same_object(&parent_before, &current.parent_facts)
        || !lookup::same_object(&selected_before, &current.selected_facts)
    {
        return Err(refused("Selected read held parent or raw child replaced"));
    }
    let leaf = request
        .components
        .last()
        .ok_or(io::ErrorKind::InvalidInput)?;
    let (file, facts) = read_from_selected(request.parent, request.selected, leaf)?;
    let after = lookup::lookup(request.view, request.components, record, view_facts)?;
    if !lookup::same_object(&facts, &selected_before)
        || !lookup::same_object(&measure(request.selected)?, &facts)
        || !lookup::same_object(&measure(request.parent)?, &parent_before)
        || !lookup::same_object(&after.parent_facts, &parent_before)
        || !lookup::same_object(&after.selected_facts, &facts)
    {
        return Err(refused("Selected read inode or raw parent/leaf changed"));
    }
    Ok((file, facts))
}
pub(super) fn serve_request(
    io: &mut Handoff<'_>,
    view: &File,
    record: &Record,
    nonce: &[u8; 32],
    admitted_view: Option<&Facts>,
) -> io::Result<()> {
    let components = lookup::read_components(io)?;
    let digest = lookup::components_sha256(&components)?;
    let parent = receive_file(io, 1)?;
    let selected = receive_file(io, 2)?;
    let admitted_view =
        admitted_view.ok_or_else(|| refused("Selected read before view readiness"))?;
    lookup::verify_worker(record)?;
    let incoming = measure(view)?;
    lookup::check_held(view, &incoming)?;
    if !lookup::same_object(&incoming, admitted_view) {
        return Err(refused("Selected read view identity differs"));
    }
    let expected = measure(&selected)?;
    let result = reopen(
        &ReadRequest {
            view,
            parent: &parent,
            selected: &selected,
            selected_facts: &expected,
            components: &components,
        },
        record,
        admitted_view,
    );
    lookup::verify_worker(record)?;
    io.write_all(nonce)?;
    io.write_all(&record.context_sha256)?;
    io.write_all(&record.source_view_sha256)?;
    io.write_all(&digest)?;
    let (file, facts) = match result {
        Ok(value) => {
            io.write_all(&0u32.to_be_bytes())?;
            value
        }
        Err(error) => {
            let errno = error
                .raw_os_error()
                .filter(|value| (1..=4095).contains(value))
                .unwrap_or(libc::EACCES);
            io.write_all(
                &u32::try_from(errno)
                    .map_err(io::Error::other)?
                    .to_be_bytes(),
            )?;
            return Ok(());
        }
    };
    io.write_all(&facts.encode())?;
    send_file(io, 0, &file)?;
    lookup::verify_worker(record)?;
    io.write_all(&[3])
}
pub(super) fn request(
    stream: &mut UnixStream,
    request: ReadRequest<'_>,
    record: &Record,
    cancellation: &ServiceCancellation,
) -> io::Result<Response> {
    let encoded = lookup::encoding(request.components)?;
    let digest = lookup::components_sha256(request.components)?;
    let mut nonce = [0; 32];
    getrandom::SysRng
        .try_fill_bytes(&mut nonce)
        .map_err(io::Error::other)?;
    if nonce == [0; 32] {
        return Err(refused("Selected read nonce unavailable"));
    }
    let mut io = Handoff::new(stream, cancellation, Duration::from_secs(10))?;
    io.write_all(&MAGIC)?;
    io.write_all(&nonce)?;
    io.write_all(&record.context_sha256)?;
    io.write_all(&record.source_view_sha256)?;
    send_file(&mut io, 0, request.view)?;
    io.write_all(&encoded)?;
    send_file(&mut io, 1, request.parent)?;
    send_file(&mut io, 2, request.selected)?;
    let status = lookup::receive_status(
        &mut io,
        &nonce,
        &record.context_sha256,
        &record.source_view_sha256,
        &digest,
    )?;
    if status != 0 {
        return Ok(Response::Refused(io::Error::from_raw_os_error(
            i32::try_from(status).map_err(io::Error::other)?,
        )));
    }
    let mut encoded = [0; super::observation::FACT_BYTES];
    io.read_exact(&mut encoded)?;
    let facts = Facts::decode(&encoded)?;
    if !lookup::same_object(&facts, request.selected_facts) {
        return Err(refused("Selected read response inode differs"));
    }
    let file = receive_file(&mut io, 0)?;
    check_read_held(&file, &facts)?;
    let mut complete = [0];
    io.read_exact(&mut complete)?;
    if complete != [3] {
        return Err(refused("Selected read completion differs"));
    }
    Ok(Response::Opened(file, facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::SysRng.try_fill_bytes(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "tilo-selected-read-{}-{}",
                std::process::id(),
                hex::encode(nonce)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn parent(&self) -> File {
            File::options()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(&self.0)
                .unwrap()
        }
        fn selected(&self) -> File {
            File::options()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
                .open(self.0.join("selected"))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    // Local kernel OFDs prove the exact production reopen guard. These are
    // not authentic signed FUSE/worker/native-positive qualification.
    #[test]
    fn actual_reopen_reads_same_held_inode_and_cannot_write() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("selected"), b"actual bytes").unwrap();
        let parent = fixture.parent();
        let selected = fixture.selected();
        let (mut read, facts) = read_from_selected(&parent, &selected, b"selected").unwrap();
        let mut bytes = Vec::new();
        read.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"actual bytes");
        assert_eq!(
            read.write(b"changed").unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
        let write = File::options()
            .read(true)
            .write(true)
            .open(fixture.0.join("selected"))
            .unwrap();
        assert_eq!(
            check_read_held(&write, &facts).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            check_read_held(&selected, &facts).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn actual_reopen_refuses_replacement_and_symlink_alias_of_same_old_inode() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("selected"), b"original").unwrap();
        let parent = fixture.parent();
        let selected = fixture.selected();
        std::fs::rename(fixture.0.join("selected"), fixture.0.join("original")).unwrap();
        std::fs::write(fixture.0.join("selected"), b"replacement").unwrap();
        assert_eq!(
            read_from_selected(&parent, &selected, b"selected")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        std::fs::remove_file(fixture.0.join("selected")).unwrap();
        std::os::unix::fs::symlink("original", fixture.0.join("selected")).unwrap();
        assert_eq!(
            read_from_selected(&parent, &selected, b"selected")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ELOOP)
        );
    }
    #[test]
    fn actual_read_scm_is_held_readonly_and_stalled_custody_is_bounded() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("selected"), b"bytes").unwrap();
        let parent = fixture.parent();
        let selected = fixture.selected();
        let (read, facts) = read_from_selected(&parent, &selected, b"selected").unwrap();
        let (mut root, peer) = UnixStream::pair().unwrap();
        runtime_portal::fd_transport::send_with_fd(peer.as_fd(), &[0], read.as_fd()).unwrap();
        let cancellation = ServiceCancellation::default();
        let mut io = Handoff::new(&mut root, &cancellation, Duration::from_secs(1)).unwrap();
        let mut received = receive_file(&mut io, 0).unwrap();
        check_read_held(&received, &facts).unwrap();
        assert_eq!(
            received.write(b"upgrade").unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
        drop(io);
        let (mut root, _stalled) = UnixStream::pair().unwrap();
        let mut io = Handoff::new(&mut root, &cancellation, Duration::from_millis(50)).unwrap();
        assert_eq!(
            receive_file(&mut io, 0).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        drop(io);
        cancellation.cancel();
        let mut io = Handoff::new(&mut root, &cancellation, Duration::from_secs(1)).unwrap();
        assert_eq!(
            receive_file(&mut io, 0).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}
