//! Named pipes: Windows' local socket. Its `AF_UNIX` passes no handles, and a
//! pipe is kept to one account by its DACL, which a socket file's mode is not.
//!
//! A path names its pipe: `\\.\pipe\tilo.<account SID>.<hash of the path>`,
//! so the same profile path reaches the same service for one account and no
//! other account's pipe shares its name. The first instance is created with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a pipe another process already holds
//! is refused rather than joined; every instance rejects remote clients and
//! carries a protected DACL granting only this account and SYSTEM. Both ends
//! then check the other's token: a server answers only its own account, and
//! a client talks only to a server of its own account, where Unix compares a
//! peer's uid.
//!
//! Every instance is opened for overlapped I/O, so a read or a write waits
//! at most its stream's timeout, as a socket's does, and the runtime's
//! listener takes over the instance this one created.
pub mod link;

use crate::account::{Account, LocalAllocation, attributes};
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, ERROR_SEM_TIMEOUT,
        GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
            GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
        },
        Threading::{
            CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};

/// No length of a path fits a socket's buffer here: a pipe is named by a hash of it.
pub const MAX_PATH: usize = 0;

/// The protected DACL every instance carries: this account and SYSTEM, nobody else.
pub(crate) const PIPE_SDDL: &str = "D:P(A;;GA;;;{sid})(A;;GA;;;SY)";
const BUFFER: u32 = 64 * 1024;
/// How long a client waits for a busy pipe's next free instance.
const BUSY_WAIT_MS: u32 = 5_000;

/// The pipe `path` names for `account`.
pub(crate) fn pipe_name(account: &Account, path: &Path) -> io::Result<String> {
    // FNV-1a over the path as Windows compares it (case-insensitive, either
    // separator): a fixed function, so every build of every program reaches the
    // same pipe. Its secrecy guards nothing; the DACL and the token checks do.
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    Ok(format!(
        r"\\.\pipe\tilo.{}.{hash:016x}",
        account.sid_string()?
    ))
}

pub(crate) fn wide(name: &str) -> Vec<u16> {
    OsStr::new(name).encode_wide().chain(Some(0)).collect()
}

/// Whether the process at the other end of `pipe` runs as `account`.
pub(crate) fn peer_is(pipe: RawHandle, server_end: bool, account: &Account) -> io::Result<bool> {
    let mut pid = 0;
    // SAFETY: a live pipe handle and a valid output pointer.
    let found = unsafe {
        if server_end {
            GetNamedPipeClientProcessId(pipe, &raw mut pid)
        } else {
            GetNamedPipeServerProcessId(pipe, &raw mut pid)
        }
    };
    if found == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Account::of_process(pid)?.is(account))
}

/// A listening pipe: the instance the next client connects to.
pub struct Listener {
    name: Vec<u16>,
    account: Account,
    descriptor: LocalAllocation,
    next: Mutex<Option<OwnedHandle>>,
}

/// Where an accepted connection came from: a pipe's client has no address.
#[derive(Clone, Copy, Debug)]
pub struct SocketAddr;

/// A new overlapped instance of the pipe `name`; the first one takes the name.
pub(crate) fn instance(
    name: &[u16],
    descriptor: &LocalAllocation,
    first: bool,
) -> io::Result<OwnedHandle> {
    let attributes = attributes(descriptor);
    let first = if first {
        FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        0
    };
    // SAFETY: a NUL-terminated name and security attributes that outlive the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            &raw const attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: reads this thread's last error.
        return Err(
            if first != 0 && unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
                io::Error::new(io::ErrorKind::AddrInUse, "another process holds this pipe")
            } else {
                io::Error::last_os_error()
            },
        );
    }
    // SAFETY: a handle CreateNamedPipeW just returned, owned from here.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

impl Listener {
    /// Accept the next connection from this account; a client of another
    /// account is disconnected and the wait goes on.
    ///
    /// # Errors
    /// The pipe could not take a connection or make its next instance.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "one caller accepts at a time: the next instance is held while a client connects"
    )]
    pub fn accept(&self) -> io::Result<(Stream, SocketAddr)> {
        let mut next = self
            .next
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let pipe = match next.take() {
                Some(pipe) => pipe,
                None => instance(&self.name, &self.descriptor, false)?,
            };
            // SAFETY: a live pipe handle and the OVERLAPPED the call is waited on with.
            match overlapped(&pipe, INFINITE, None, |at| unsafe {
                ConnectNamedPipe(pipe.as_raw_handle(), at)
            }) {
                Ok(_) => {}
                // A client that connected before the call is connected all the same.
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {}
                Err(error) => return Err(error),
            }
            // The next client finds an instance waiting while this one is served.
            *next = Some(instance(&self.name, &self.descriptor, false)?);
            if peer_is(pipe.as_raw_handle(), true, &self.account)? {
                return Ok((Stream::new(pipe, true)?, SocketAddr));
            }
            // SAFETY: a connected pipe handle.
            unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
        }
    }

    /// Each connection [`Listener::accept`] takes, in turn.
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        std::iter::from_fn(|| Some(self.accept().map(|(stream, _)| stream)))
    }

    /// The pipe's name, its account's descriptor and the instance waiting, for
    /// the asynchronous listener taking it over.
    #[cfg(feature = "tokio")]
    pub(crate) fn into_parts(self) -> (Vec<u16>, Account, LocalAllocation, Option<OwnedHandle>) {
        let next = self
            .next
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (self.name, self.account, self.descriptor, next)
    }
}

/// What every handle to one connection shares, as a socket's options and
/// state are shared: its read and write bounds in milliseconds (`u64::MAX`
/// for none), and the event [`Stream::shutdown`] sets.
#[derive(Debug)]
struct Connection {
    read: AtomicU64,
    write: AtomicU64,
    shut: OwnedHandle,
    server_end: bool,
}

const NO_TIMEOUT: u64 = u64::MAX;

/// One end of a connected pipe.
#[derive(Debug)]
pub struct Stream {
    pipe: OwnedHandle,
    connection: Arc<Connection>,
}

/// An unnamed, manual-reset, unsignalled event.
fn event() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes and no name.
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: an event CreateEventW just returned, owned from here.
    Ok(unsafe { OwnedHandle::from_raw_handle(event) })
}

/// Run one overlapped call on `pipe` and wait at most `wait_ms` for it, or
/// until `shut` is set; a call still pending then is cancelled and answers
/// `TimedOut`, or `NotConnected` once shut, unless it moved bytes before the
/// cancel took.
fn overlapped(
    pipe: &OwnedHandle,
    wait_ms: u32,
    shut: Option<&OwnedHandle>,
    start: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
) -> io::Result<usize> {
    let event = event()?;
    // SAFETY: OVERLAPPED is plain data, valid zeroed.
    let mut call: OVERLAPPED = unsafe { std::mem::zeroed() };
    call.hEvent = event.as_raw_handle();
    // SAFETY: reads this thread's last error, right after the call.
    if start(&raw mut call) == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the event this call signals; then the call's own handle and OVERLAPPED,
    // which outlive it: GetOverlappedResult waits for a cancelled call to end.
    unsafe {
        let waited = match shut {
            Some(shut) => {
                let events = [event.as_raw_handle(), shut.as_raw_handle()];
                WaitForMultipleObjects(2, events.as_ptr(), 0, wait_ms)
            }
            None => WaitForSingleObject(event.as_raw_handle(), wait_ms),
        };
        let timed_out = waited == WAIT_TIMEOUT;
        let shut_down = waited == WAIT_OBJECT_0 + 1;
        if timed_out || shut_down {
            CancelIoEx(pipe.as_raw_handle(), &raw const call);
        }
        let mut moved = 0;
        if GetOverlappedResult(pipe.as_raw_handle(), &raw const call, &raw mut moved, 1) != 0
            || moved > 0
        {
            return Ok(moved as usize);
        }
        if shut_down && GetLastError() == ERROR_OPERATION_ABORTED {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        }
        if timed_out && GetLastError() == ERROR_OPERATION_ABORTED {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the pipe's peer did not answer in time",
            ));
        }
    }
    Err(io::Error::last_os_error())
}

fn milliseconds(timeout: Option<Duration>) -> io::Result<u64> {
    match timeout {
        None => Ok(NO_TIMEOUT),
        Some(timeout) if timeout.is_zero() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        )),
        // Rounded up, and kept short of INFINITE.
        Some(timeout) => Ok(u64::try_from(timeout.as_nanos().div_ceil(1_000_000))
            .unwrap_or(u64::MAX)
            .min(u64::from(INFINITE - 1))),
    }
}

impl Stream {
    fn new(pipe: OwnedHandle, server_end: bool) -> io::Result<Self> {
        Ok(Self {
            pipe,
            connection: Arc::new(Connection {
                read: AtomicU64::new(NO_TIMEOUT),
                write: AtomicU64::new(NO_TIMEOUT),
                shut: event()?,
                server_end,
            }),
        })
    }

    /// Another handle to the same connection, sharing its timeouts and its
    /// shutdown.
    ///
    /// # Errors
    /// The handle could not be duplicated.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            pipe: self.pipe.try_clone()?,
            connection: Arc::clone(&self.connection),
        })
    }

    /// Bound each read: one that waits longer is cancelled and fails with
    /// `TimedOut`. `None` waits without end.
    ///
    /// # Errors
    /// A zero duration, as a socket refuses it.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.connection
            .read
            .store(milliseconds(timeout)?, Ordering::Relaxed);
        Ok(())
    }

    /// Bound each write, as [`Stream::set_read_timeout`] bounds reads.
    ///
    /// # Errors
    /// A zero duration, as a socket refuses it.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.connection
            .write
            .store(milliseconds(timeout)?, Ordering::Relaxed);
        Ok(())
    }

    /// End the connection for every handle to it, as a socket's
    /// `shutdown(Both)` does: a read waiting on it, or made after, reads the
    /// end of the stream, and a write fails. The server end also disconnects
    /// its client, which then reads the end; a client's server learns of it
    /// when the client closes. A pipe has no half-close.
    ///
    /// # Errors
    /// `Unsupported` for `Read` or `Write` alone, or the shutdown could not
    /// be signalled.
    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        if how != std::net::Shutdown::Both {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a pipe shuts down both ways at once",
            ));
        }
        // SAFETY: the connection's own event.
        if unsafe { SetEvent(self.connection.shut.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if self.connection.server_end {
            // SAFETY: a live server-end handle; a client already gone is no error here.
            unsafe { DisconnectNamedPipe(self.pipe.as_raw_handle()) };
        }
        Ok(())
    }

    fn is_shut(&self) -> bool {
        // SAFETY: the connection's own event, polled without waiting.
        unsafe { WaitForSingleObject(self.connection.shut.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    fn wait(bound: &AtomicU64) -> u32 {
        u32::try_from(bound.load(Ordering::Relaxed)).unwrap_or(INFINITE)
    }

    fn read_into(&self, buf: &mut [u8]) -> io::Result<usize> {
        if self.is_shut() {
            return Ok(0);
        }
        let length = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let shut = Some(&self.connection.shut);
        let wait = Self::wait(&self.connection.read);
        // SAFETY: a live pipe handle, and a buffer that outlives the call:
        // `overlapped` returns only once the call has ended.
        let read = overlapped(&self.pipe, wait, shut, |at| unsafe {
            ReadFile(
                self.pipe.as_raw_handle(),
                buf.as_mut_ptr(),
                length,
                std::ptr::null_mut(),
                at,
            )
        });
        match read {
            // The peer closed or disconnected its end, or this connection was
            // shut down: the end of the stream, as a socket reads it.
            Err(error)
                if error.kind() == io::ErrorKind::NotConnected
                    || [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED]
                        .iter()
                        .any(|code| error.raw_os_error() == Some(*code as i32)) =>
            {
                Ok(0)
            }
            read => read,
        }
    }

    fn write_from(&self, buf: &[u8]) -> io::Result<usize> {
        if self.is_shut() {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let length = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let shut = Some(&self.connection.shut);
        let wait = Self::wait(&self.connection.write);
        // SAFETY: as `read_into`.
        overlapped(&self.pipe, wait, shut, |at| unsafe {
            WriteFile(
                self.pipe.as_raw_handle(),
                buf.as_ptr(),
                length,
                std::ptr::null_mut(),
                at,
            )
        })
    }
}

impl AsRawHandle for Stream {
    fn as_raw_handle(&self) -> RawHandle {
        self.pipe.as_raw_handle()
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_into(buf)
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_into(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_from(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_from(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
    let account = Account::current()?;
    let name = wide(&pipe_name(&account, path)?);
    let descriptor = account.descriptor(PIPE_SDDL)?;
    let first = instance(&name, &descriptor, true)?;
    Ok(Listener {
        name,
        account,
        descriptor,
        next: Mutex::new(Some(first)),
    })
}

/// Open the overlapped client end of the pipe `name`, waiting out a busy one.
pub(crate) fn open(name: &[u16]) -> io::Result<OwnedHandle> {
    loop {
        // SAFETY: a NUL-terminated name. Identification only: the server may
        // learn who connects, never act as them.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: a handle CreateFileW just returned, owned from here.
            return Ok(unsafe { OwnedHandle::from_raw_handle(handle) });
        }
        // SAFETY: reads this thread's last error, then waits on a NUL-terminated name.
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY
            || unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
}

pub(crate) fn connect(path: &Path) -> io::Result<Stream> {
    let account = Account::current()?;
    let pipe = open(&wide(&pipe_name(&account, path)?))?;
    if !peer_is(pipe.as_raw_handle(), false, &account)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: the pipe is served by another account", path.display()),
        ));
    }
    Stream::new(pipe, false)
}

pub(crate) fn is_bound(path: &Path) -> bool {
    let Ok(name) = Account::current().and_then(|account| pipe_name(&account, path)) else {
        return false;
    };
    let name = wide(&name);
    // A wait of a millisecond answers whether an instance exists without taking
    // one: it times out on a pipe whose instances are all busy, and fails at
    // once on a name nothing serves.
    // SAFETY: a NUL-terminated name; then this thread's last error.
    unsafe { WaitNamedPipeW(name.as_ptr(), 1) != 0 || GetLastError() == ERROR_SEM_TIMEOUT }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("collab.sock")
    }

    #[test]
    fn a_pipe_carries_bytes_both_ways_and_takes_the_next_client() {
        let dir = tempfile::tempdir().unwrap();
        let listener = crate::bind(&path(&dir)).unwrap();
        assert!(crate::is_bound(&path(&dir)));
        for round in 0..2_u8 {
            let at = path(&dir);
            let client = std::thread::spawn(move || {
                let mut stream = crate::connect(&at).unwrap();
                stream.write_all(&[round, 1, 2]).unwrap();
                let mut answer = [0; 3];
                stream.read_exact(&mut answer).unwrap();
                answer
            });
            let (mut served, _) = listener.accept().unwrap();
            let mut asked = [0; 3];
            served.read_exact(&mut asked).unwrap();
            assert_eq!(asked, [round, 1, 2]);
            served
                .try_clone()
                .unwrap()
                .write_all(&[round, 3, 4])
                .unwrap();
            assert_eq!(client.join().unwrap(), [round, 3, 4]);
        }
    }

    #[test]
    fn nothing_bound_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!crate::is_bound(&path(&dir)));
        assert_eq!(
            crate::connect(&path(&dir)).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn a_read_waits_at_most_its_timeout_and_the_stream_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let listener = crate::bind(&path(&dir)).unwrap();
        let at = path(&dir);
        let client = std::thread::spawn(move || crate::connect(&at).unwrap());
        let (mut served, _) = listener.accept().unwrap();
        let mut client = client.join().unwrap();
        assert_eq!(
            client
                .set_read_timeout(Some(Duration::ZERO))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let started = std::time::Instant::now();
        let mut byte = [0];
        assert_eq!(
            client.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() >= Duration::from_millis(40));
        served.write_all(&[7]).unwrap();
        client.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [7]);
        drop(served);
        assert_eq!(client.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn a_shutdown_ends_a_waiting_read_on_both_ends() {
        let dir = tempfile::tempdir().unwrap();
        let listener = crate::bind(&path(&dir)).unwrap();
        let at = path(&dir);
        let client = std::thread::spawn(move || crate::connect(&at).unwrap());
        let (served, _) = listener.accept().unwrap();
        let mut client = client.join().unwrap();
        let waiting = served.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let mut byte = [0];
            (&waiting).read(&mut byte).unwrap()
        });
        std::thread::sleep(Duration::from_millis(50));
        served.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(reader.join().unwrap(), 0, "the waiting read reads the end");
        assert_eq!(
            (&served).write(&[1]).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let mut byte = [0];
        assert_eq!(
            client.read(&mut byte).unwrap(),
            0,
            "the client reads the end"
        );
        assert_eq!(
            served
                .shutdown(std::net::Shutdown::Write)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    /// Wine lets a second process create a pipe's first instance again: this
    /// runs on a Windows worker (tools/ci/windows/wine-skip.txt).
    #[test]
    fn a_held_pipe_is_not_joined() {
        let dir = tempfile::tempdir().unwrap();
        let _held = crate::bind(&path(&dir)).unwrap();
        let Err(refused) = crate::bind(&path(&dir)) else {
            panic!("a second listener joined a pipe another holds");
        };
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
    }

    /// Wine keeps a pipe's security descriptor but enforces none of it, and has
    /// no second account to try it with: this runs on a Windows worker
    /// (tools/ci/windows/wine-skip.txt).
    #[test]
    fn a_pipe_grants_this_account_and_system_alone() {
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_KERNEL_OBJECT,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let dir = tempfile::tempdir().unwrap();
        let listener = crate::bind(&path(&dir)).unwrap();
        let pipe = listener.next.lock().unwrap().take().unwrap();
        let mut descriptor = std::ptr::null_mut();
        let mut text = std::ptr::null_mut();
        // SAFETY: a live pipe handle and valid output pointers; allocations are freed by guards.
        let sddl = unsafe {
            assert_eq!(
                GetSecurityInfo(
                    pipe.as_raw_handle(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut descriptor,
                ),
                0
            );
            let _descriptor = LocalAllocation(descriptor);
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &raw mut text,
                    std::ptr::null_mut(),
                ),
                0
            );
            let _text = LocalAllocation(text.cast());
            let length = (0..).take_while(|&at| *text.add(at) != 0).count();
            String::from_utf16(std::slice::from_raw_parts(text, length)).unwrap()
        };
        let sid = Account::current().unwrap().sid_string().unwrap();
        assert_eq!(sddl, format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)"));
    }
}
