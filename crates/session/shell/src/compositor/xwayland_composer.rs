//! The composing input method on the per-workspace X11 displays.
//!
//! X11 applications reach an input method through XIM, which lives on the X
//! display itself: the input method has to be a client of every display whose
//! applications should compose text. The session asks its composer (Fcitx5)
//! to open a connection to a workspace's display once that display's server
//! is running for a real client, and never before, so an idle workspace
//! still runs no X server. The composer's connection is not a reason to keep
//! the server alive: when counting clients for the idle stop it is excluded
//! by the kernel's own record of who owns the connecting socket. When the
//! server stops, the connection dies with it; the next server gets a fresh
//! request.

use std::collections::HashSet;
use std::fs;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

/// Process group of the running composer, published by the input-method
/// service; 0 when none runs.
static COMPOSER_GROUP: AtomicI32 = AtomicI32::new(0);

/// Publish the composer's process group (`None` when it is not running).
pub(crate) fn set_composer_process_group(group: Option<libc::pid_t>) {
    COMPOSER_GROUP.store(group.unwrap_or(0).max(0), Ordering::Release);
}

fn composer_process_group() -> Option<libc::pid_t> {
    let group = COMPOSER_GROUP.load(Ordering::Acquire);
    (group > 0).then_some(group)
}

/// Where the composer finds the cookies of every workspace display. It is
/// passed to the composer as `XAUTHORITY` and rewritten before each
/// connection request, so a display created after the composer started is
/// still reachable.
pub(crate) fn composer_authority_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir
        .join(tilo_paths::login::SESSION_DIR)
        .join("xwayland")
        .join("composer-Xauthority")
}

/// One poll's view of the composer: which process group it is, which
/// sockets it owns (scanned at most once, and only when an idle check
/// needs it), and which displays it should now connect to.
pub(crate) struct ComposerClients {
    process_group: Option<libc::pid_t>,
    inodes: Option<HashSet<u64>>,
    pub(crate) requests: Vec<String>,
}

impl ComposerClients {
    pub(crate) fn current() -> Self {
        Self::for_group(composer_process_group())
    }

    pub(crate) fn for_group(process_group: Option<libc::pid_t>) -> Self {
        Self {
            process_group,
            inodes: None,
            requests: Vec::new(),
        }
    }

    pub(crate) fn process_group(&self) -> Option<libc::pid_t> {
        self.process_group
    }

    /// Clients of the display bound at `socket`, not counting the
    /// composer's own connection. `None` when the kernel's socket tables
    /// cannot be read; the caller then keeps the server running.
    pub(crate) fn connected_clients(&mut self, socket: &Path) -> Option<usize> {
        let Some(group) = self.process_group else {
            return connected_clients(socket);
        };
        let inodes = self
            .inodes
            .get_or_insert_with(|| process_group_socket_inodes(group));
        // Without the peer table, the composer's connection cannot be told
        // apart; counting it keeps the server up, which is the safe side.
        connected_peers(socket)
            .map(|peers| clients_excluding(&peers, inodes))
            .or_else(|| connected_clients(socket))
    }
}

/// Connections whose client end is not one of the composer's sockets.
fn clients_excluding(peers: &[u64], composer_sockets: &HashSet<u64>) -> usize {
    peers
        .iter()
        .filter(|peer| !composer_sockets.contains(peer))
        .count()
}

/// Connected X clients of the display bound at `socket`, from the kernel's
/// socket table: each connection the X server accepted is listed under the
/// listening path.
pub(crate) fn connected_clients(socket: &Path) -> Option<usize> {
    fs::read_to_string("/proc/net/unix")
        .ok()
        .map(|table| count_connected(&table, socket))
}

pub(crate) fn count_connected(table: &str, socket: &Path) -> usize {
    const SS_CONNECTED: &str = "03";
    let target = socket.as_os_str().as_bytes();
    table
        .lines()
        .skip(1)
        .filter(|line| {
            // Num RefCount Protocol Flags Type St Inode Path
            let mut fields = line.split_ascii_whitespace();
            fields.nth(5) == Some(SS_CONNECTED)
                && fields.nth(1).is_some_and(|path| path.as_bytes() == target)
        })
        .count()
}

/// For every connection the X server accepted on `socket`, the inode of the
/// client's end, from the kernel's `sock_diag` interface.
fn connected_peers(socket: &Path) -> Option<Vec<u64>> {
    Some(peers_bound_to(unix_diag_established()?, socket))
}

fn peers_bound_to(entries: Vec<UnixDiagEntry>, socket: &Path) -> Vec<u64> {
    let target = socket.as_os_str().as_bytes();
    entries
        .into_iter()
        .filter(|entry| entry.name.as_deref() == Some(target))
        .filter_map(|entry| entry.peer)
        .map(u64::from)
        .collect()
}

struct UnixDiagEntry {
    name: Option<Vec<u8>>,
    peer: Option<u32>,
}

const NETLINK_SOCK_DIAG: libc::c_int = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const UDIAG_SHOW_NAME: u32 = 0x1;
const UDIAG_SHOW_PEER: u32 = 0x4;
const UNIX_DIAG_NAME: u16 = 0;
const UNIX_DIAG_PEER: u16 = 2;
const TCP_ESTABLISHED: u32 = 1;
const NLMSG_HEADER: usize = 16;
const UNIX_DIAG_MSG: usize = 16;

/// Dump every established Unix socket visible to this process with its bound
/// name and peer inode.
fn unix_diag_established() -> Option<Vec<UnixDiagEntry>> {
    // SAFETY: plain socket(2); the result is checked before use.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_SOCK_DIAG,
        )
    };
    if raw < 0 {
        return None;
    }
    // SAFETY: `raw` is a descriptor socket(2) just returned to us.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };

    // nlmsghdr + unix_diag_req, native endian as netlink requires.
    let mut request = Vec::with_capacity(NLMSG_HEADER + 24);
    let length = u32::try_from(NLMSG_HEADER + 24).ok()?;
    request.extend_from_slice(&length.to_ne_bytes());
    request.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    let flags = u16::try_from(libc::NLM_F_REQUEST | libc::NLM_F_DUMP).ok()?;
    request.extend_from_slice(&flags.to_ne_bytes());
    request.extend_from_slice(&1_u32.to_ne_bytes()); // sequence
    request.extend_from_slice(&0_u32.to_ne_bytes()); // port id: the kernel
    request.push(u8::try_from(libc::AF_UNIX).ok()?);
    request.push(0); // protocol
    request.extend_from_slice(&0_u16.to_ne_bytes()); // padding
    request.extend_from_slice(&(1_u32 << TCP_ESTABLISHED).to_ne_bytes());
    request.extend_from_slice(&0_u32.to_ne_bytes()); // any inode
    request.extend_from_slice(&(UDIAG_SHOW_NAME | UDIAG_SHOW_PEER).to_ne_bytes());
    request.extend_from_slice(&[0_u8; 8]); // cookie
    // SAFETY: `request` is initialised for its full length and the descriptor
    // is open.
    let sent = unsafe {
        libc::send(
            socket.as_raw_fd(),
            request.as_ptr().cast(),
            request.len(),
            0,
        )
    };
    if usize::try_from(sent).ok()? != request.len() {
        return None;
    }

    let mut entries = Vec::new();
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        // SAFETY: `buffer` is writable for its full length.
        let received = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        let received = usize::try_from(received).ok()?;
        if received == 0 {
            return None;
        }
        match parse_diag_messages(buffer.get(..received)?, &mut entries)? {
            DiagProgress::More => {}
            DiagProgress::Done => return Some(entries),
        }
    }
}

enum DiagProgress {
    More,
    Done,
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn align4(value: usize) -> Option<usize> {
    Some(value.checked_add(3)? & !3)
}

/// Parse one datagram of netlink messages. `None` on a malformed message or
/// a kernel error, which the caller treats as "table unavailable".
fn parse_diag_messages(mut bytes: &[u8], entries: &mut Vec<UnixDiagEntry>) -> Option<DiagProgress> {
    const NLMSG_ERROR: u16 = 2;
    const NLMSG_DONE: u16 = 3;
    while bytes.len() >= NLMSG_HEADER {
        let length = usize::try_from(read_u32(bytes, 0)?).ok()?;
        let kind = read_u16(bytes, 4)?;
        if length < NLMSG_HEADER || length > bytes.len() {
            return None;
        }
        match kind {
            NLMSG_DONE => return Some(DiagProgress::Done),
            NLMSG_ERROR => return None,
            SOCK_DIAG_BY_FAMILY => {
                let message = bytes.get(NLMSG_HEADER..length)?;
                entries.push(parse_unix_diag(message)?);
            }
            _ => {}
        }
        bytes = bytes.get(align4(length)?.min(bytes.len())..)?;
    }
    Some(DiagProgress::More)
}

fn parse_unix_diag(message: &[u8]) -> Option<UnixDiagEntry> {
    let mut attributes = message.get(UNIX_DIAG_MSG..)?;
    let mut entry = UnixDiagEntry {
        name: None,
        peer: None,
    };
    while attributes.len() >= 4 {
        let length = usize::from(read_u16(attributes, 0)?);
        let kind = read_u16(attributes, 2)?;
        if length < 4 || length > attributes.len() {
            return None;
        }
        let payload = attributes.get(4..length)?;
        match kind {
            // A filesystem name is reported with its terminating NUL.
            UNIX_DIAG_NAME => {
                let end = if payload.first() == Some(&0) {
                    payload.len()
                } else {
                    payload
                        .iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(payload.len())
                };
                entry.name = payload.get(..end).map(<[u8]>::to_vec);
            }
            UNIX_DIAG_PEER => entry.peer = read_u32(payload, 0),
            _ => {}
        }
        attributes = attributes.get(align4(length)?.min(attributes.len())..)?;
    }
    Some(entry)
}

fn process_group_socket_inodes(group: libc::pid_t) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    let Ok(processes) = fs::read_dir("/proc") else {
        return inodes;
    };
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        // SAFETY: getpgid has no memory-safety preconditions; a vanished pid
        // simply fails.
        if unsafe { libc::getpgid(pid) } != group {
            continue;
        }
        inodes.extend(socket_inodes_of(pid));
    }
    inodes
}

fn socket_inodes_of(pid: libc::pid_t) -> Vec<u64> {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    fds.flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter_map(|target| {
            target
                .to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse()
                .ok()
        })
        .collect()
}

/// Write every display's cookie into the composer's authority file,
/// atomically, owner-only.
///
/// The compositor loop calls this, so it does not sync: the file lives in the
/// login's runtime directory and is rewritten from the live displays whenever
/// the composer is asked to connect, so a crash loses nothing a sync would
/// have kept, while a sync under I/O pressure stalls the desktop.
pub(crate) fn write_composer_authority(path: &Path, authorities: &[PathBuf]) -> Result<(), String> {
    let mut contents = Vec::new();
    for authority in authorities {
        contents.extend(
            fs::read(authority)
                .map_err(|error| format!("reading {}: {error}", authority.display()))?,
        );
    }
    tilo_files::native::atomic::write_regenerable_with_mode(path, &contents, 0o600)
        .map_err(|error| format!("installing {}: {error}", path.display()))
}

/// Ask the composer, over the session bus, to become the input method of
/// `display`. Runs off the compositor thread: the composer answers after
/// its X handshake.
pub(crate) fn request_connection(display_name: String) {
    let spawned = std::thread::Builder::new()
        .name("tilo-xim-connect".into())
        .spawn(move || {
            let result = zbus::blocking::Connection::session().and_then(|bus| {
                bus.call_method(
                    Some("org.fcitx.Fcitx5"),
                    "/controller",
                    Some("org.fcitx.Fcitx.Controller1"),
                    "OpenX11Connection",
                    &(display_name.as_str(),),
                )
                .map(drop)
            });
            match result {
                Ok(()) => tracing::info!(target: "tilo::input_method", x11_display = %display_name, "composer serves the X11 display"),
                Err(error) => {
                    tracing::warn!(target: "tilo::input_method", x11_display = %display_name, %error, "composer could not open the X11 display");
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "tilo::input_method", %error, "could not ask the composer to open an X11 display");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::net::UnixStream;

    fn inode(stream: &UnixStream) -> u64 {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `stat` is writable and the descriptor is open.
        assert_eq!(unsafe { libc::fstat(stream.as_raw_fd(), &raw mut stat) }, 0);
        stat.st_ino
    }

    /// One `SOCK_DIAG_BY_FAMILY` reply as the kernel frames it: header,
    /// `unix_diag_msg`, then NAME (NUL-terminated) and PEER attributes.
    fn diag_message(name: Option<&[u8]>, peer: u32) -> Vec<u8> {
        let mut attributes = Vec::new();
        if let Some(name) = name {
            let mut payload = name.to_vec();
            payload.push(0);
            let length = u16::try_from(payload.len().saturating_add(4)).unwrap();
            attributes.extend_from_slice(&length.to_ne_bytes());
            attributes.extend_from_slice(&UNIX_DIAG_NAME.to_ne_bytes());
            attributes.extend_from_slice(&payload);
            while !attributes.len().is_multiple_of(4) {
                attributes.push(0);
            }
        }
        attributes.extend_from_slice(&8_u16.to_ne_bytes());
        attributes.extend_from_slice(&UNIX_DIAG_PEER.to_ne_bytes());
        attributes.extend_from_slice(&peer.to_ne_bytes());
        let length = u32::try_from(
            attributes
                .len()
                .saturating_add(NLMSG_HEADER + UNIX_DIAG_MSG),
        )
        .unwrap();
        let mut message = Vec::new();
        message.extend_from_slice(&length.to_ne_bytes());
        message.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        message.extend_from_slice(&[0_u8; 10]);
        message.extend_from_slice(&[0_u8; UNIX_DIAG_MSG]);
        message.extend_from_slice(&attributes);
        message
    }

    #[test]
    fn diag_replies_yield_the_peers_of_one_display_socket() {
        let mut datagram = diag_message(Some(b"/tmp/.X11-unix/X7"), 101);
        datagram.extend(diag_message(Some(b"/tmp/.X11-unix/X70"), 202));
        datagram.extend(diag_message(None, 303));
        datagram.extend(diag_message(Some(b"/tmp/.X11-unix/X7"), 404));
        let mut done = Vec::new();
        done.extend_from_slice(&16_u32.to_ne_bytes());
        done.extend_from_slice(&3_u16.to_ne_bytes());
        done.extend_from_slice(&[0_u8; 10]);
        datagram.extend(done);

        let mut entries = Vec::new();
        assert!(matches!(
            parse_diag_messages(&datagram, &mut entries),
            Some(DiagProgress::Done)
        ));
        assert_eq!(
            peers_bound_to(entries, Path::new("/tmp/.X11-unix/X7")),
            [101, 404],
            "exact path match, NUL stripped, unnamed sockets ignored"
        );
    }

    #[test]
    fn a_truncated_or_error_reply_reads_as_unavailable() {
        let message = diag_message(Some(b"/tmp/.X11-unix/X7"), 1);
        let mut entries = Vec::new();
        assert!(parse_diag_messages(&message[..message.len() - 3], &mut entries).is_none());
        let mut error = Vec::new();
        error.extend_from_slice(&16_u32.to_ne_bytes());
        error.extend_from_slice(&2_u16.to_ne_bytes());
        error.extend_from_slice(&[0_u8; 10]);
        assert!(parse_diag_messages(&error, &mut entries).is_none());
    }

    #[test]
    fn the_composer_connection_is_not_counted_as_a_client() {
        let composer = HashSet::from([404]);
        assert_eq!(clients_excluding(&[101, 404], &composer), 1);
        assert_eq!(
            clients_excluding(&[404], &composer),
            0,
            "only the composer is left"
        );
        assert_eq!(clients_excluding(&[101], &HashSet::new()), 1);
    }

    #[test]
    fn own_process_group_sockets_are_found() {
        let (a, _b) = UnixStream::pair().unwrap();
        // SAFETY: getpgrp has no failure mode.
        let group = unsafe { libc::getpgrp() };
        assert!(process_group_socket_inodes(group).contains(&inode(&a)));
    }

    #[test]
    fn composer_authority_concatenates_every_display_cookie() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("Xauthority-1");
        let second = temp.path().join("Xauthority-2");
        fs::write(&first, b"one").unwrap();
        fs::write(&second, b"two").unwrap();
        let target = temp.path().join("composer-Xauthority");
        write_composer_authority(&target, &[first, second]).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"onetwo");
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
    }
}
