//! One administrator-created socket, one authenticated connection, one retained claim owner.
use super::{
    refusal,
    session::{ExclusiveReadSession, random},
};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    time::Instant,
};
use tilo_ntfs_protocol::{Operation, Reply, Request, Response, SCHEMA, Sequence, socket};
struct Runtime {
    parent: File,
    name: CString,
    path: String,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Ok(path) = CString::new(format!("{}/read.sock", self.path)) {
            unsafe {
                libc::unlink(path.as_ptr());
            }
        }
        unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            );
        }
    }
}
fn listener(uid: u32) -> io::Result<(Runtime, OwnedFd)> {
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/run")?;
    let m = parent.metadata()?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(refusal("unsafe administrative runtime parent"));
    }
    let name = CString::new(format!("tilo-ntfs-{}", random()?)).map_err(io::Error::other)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o711) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fchmodat(parent.as_raw_fd(), name.as_ptr(), 0o711, 0) } != 0 {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
        }
        return Err(io::Error::last_os_error());
    }
    let path = format!("/run/{}", name.to_str().map_err(io::Error::other)?);
    let runtime = Runtime { parent, name, path };
    let address = socket::address(&format!("{}/read.sock", runtime.path))?;
    let fd = socket::create()?;
    if unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const address).cast(),
            libc::socklen_t::try_from(mem::size_of_val(&address)).map_err(io::Error::other)?,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let path = CString::new(format!("{}/read.sock", runtime.path)).map_err(io::Error::other)?;
    if unsafe { libc::chown(path.as_ptr(), uid, 0) } != 0
        || unsafe { libc::chmod(path.as_ptr(), 0o600) } != 0
        || unsafe { libc::listen(fd.as_raw_fd(), 1) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((runtime, fd))
}
extern "C" fn cancelled(_: i32) {
    crate::limits::CANCELLED.store(true, std::sync::atomic::Ordering::Release);
}
fn signals() -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    action.sa_sigaction = cancelled as *const () as usize;
    for signal in [libc::SIGINT, libc::SIGTERM] {
        if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
pub(super) fn serve(mut session: ExclusiveReadSession, uid: u32) -> io::Result<()> {
    signals()?;
    let (runtime, listener) = listener(uid)?;
    println!(
        "{}",
        serde_json::json!({"schema":SCHEMA,"socket":format!("{}/read.sock",runtime.path),"nonce":session.nonce,"identity":session.identity,"backend":"native-exclusive-read","diagnostic_only":true})
    );
    io::stdout().flush()?;
    socket::ready(&listener, libc::POLLIN, session.deadline(30)?)?;
    let raw = unsafe {
        libc::accept4(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let connection = unsafe { OwnedFd::from_raw_fd(raw) };
    drop(listener);
    if socket::peer_uid(&connection)? != uid {
        return Err(refusal("unauthorized peer UID"));
    }
    let result = connected(&mut session, &connection, uid);
    session.invalidate();
    drop(connection);
    drop(runtime);
    result
}
fn reply(fd: &OwnedFd, request: &Request, response: Response, deadline: Instant) -> io::Result<()> {
    socket::send(
        fd,
        &Reply {
            schema: SCHEMA,
            nonce: request.nonce.clone(),
            id: request.id,
            result: response,
        },
        deadline,
    )
}
struct Watchdog;
impl Watchdog {
    fn start() -> Self {
        unsafe {
            libc::alarm(20);
        }
        Self
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        unsafe {
            libc::alarm(0);
        }
    }
}
fn connected(session: &mut ExclusiveReadSession, fd: &OwnedFd, uid: u32) -> io::Result<()> {
    session.cancellation_fd = Some(fd.as_raw_fd());
    let mut sequence = Sequence::default();
    let mut hello = false;
    let mut page = usize::MAX;
    let mut pages_done = false;
    loop {
        if crate::limits::CANCELLED.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refusal("cancelled session"));
        }
        let request: Request = socket::receive(fd, session.deadline(30)?)?;
        sequence.accept(&request, &session.nonce)?;
        let _watchdog = Watchdog::start();
        let end = session.deadline(20)?;
        if !hello && !matches!(request.operation, Operation::Hello) {
            return Err(refusal("hello required"));
        }
        match &request.operation {
            Operation::Hello => {
                if hello {
                    return Err(refusal("duplicate hello"));
                }
                hello = true;
                reply(
                    fd,
                    &request,
                    Response::Hello {
                        identity: session.identity.clone(),
                        client_uid: uid,
                        backend: "native-exclusive-read".into(),
                        acceptance: "native-runtime-unverified".into(),
                    },
                    end,
                )?;
            }
            Operation::BeginObservation => {
                let generation = session.begin()?;
                page = 0;
                pages_done = false;
                reply(fd, &request, Response::Begun { generation }, end)?;
            }
            Operation::NextManifestPage => {
                if page == usize::MAX || pages_done {
                    return Err(refusal("manifest paging state"));
                }
                manifest_page(session, fd, &request, end, &mut page, &mut pages_done)?;
            }
            Operation::ReadChunk {
                token,
                offset,
                length,
            } => {
                if !pages_done {
                    return Err(refusal("manifest not delivered"));
                }
                content(session, fd, &request, token, *offset, *length, end)?;
            }
            Operation::FinishObservation => {
                if !pages_done {
                    return Err(refusal("manifest not delivered"));
                }
                let (hashes, issues) = session.finish()?;
                for n in 0..hashes.len().max(issues.len()).max(1) {
                    reply(
                        fd,
                        &request,
                        Response::Finished {
                            hashes: hashes.get(n).cloned().into_iter().collect(),
                            issues: issues.get(n).cloned().into_iter().collect(),
                            done: n.saturating_add(1) >= hashes.len().max(issues.len()),
                        },
                        end,
                    )?;
                }
                page = usize::MAX;
                pages_done = false;
            }
            Operation::Cancel | Operation::Close => {
                session.invalidate();
                reply(fd, &request, Response::Closed, end)?;
                return Ok(());
            }
        }
    }
}

fn manifest_page(
    session: &ExclusiveReadSession,
    fd: &OwnedFd,
    request: &Request,
    end: Instant,
    page: &mut usize,
    pages_done: &mut bool,
) -> io::Result<()> {
    let entries = session
        .manifest
        .entries
        .get(*page)
        .cloned()
        .into_iter()
        .collect();
    let issues = session
        .manifest
        .issues
        .get(*page)
        .cloned()
        .into_iter()
        .collect();
    let start = (*page)
        .checked_mul(1024)
        .ok_or_else(|| refusal("page overflow"))?;
    let upcase = session
        .manifest
        .upcase
        .get(start..start.saturating_add(1024).min(65_536))
        .unwrap_or(&[])
        .to_vec();
    *page = (*page)
        .checked_add(1)
        .ok_or_else(|| refusal("page overflow"))?;
    *pages_done = *page
        >= session
            .manifest
            .entries
            .len()
            .max(session.manifest.issues.len())
            .max(64);
    reply(
        fd,
        request,
        Response::Page {
            entries,
            issues,
            upcase,
            done: *pages_done,
        },
        end,
    )?;
    Ok(())
}

fn content(
    session: &mut ExclusiveReadSession,
    fd: &OwnedFd,
    request: &Request,
    token: &str,
    offset: u64,
    length: u32,
    end: Instant,
) -> io::Result<()> {
    let bytes = session.read(token, offset, length)?;
    for (index, chunk) in bytes.chunks(4096).enumerate() {
        reply(
            fd,
            request,
            Response::Data {
                offset: u32::try_from(index.saturating_mul(4096)).map_err(io::Error::other)?,
                bytes: chunk.to_vec(),
            },
            end,
        )?;
    }
    reply(
        fd,
        request,
        Response::ReadDone {
            length,
            sha256: hex::encode(Sha256::digest(&bytes)),
        },
        end,
    )?;
    Ok(())
}
