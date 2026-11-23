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
use crate::account::{Account, LocalAllocation, attributes};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::Mutex;
use windows_sys::Win32::{
    Foundation::{
        ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT,
        GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
        SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    },
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
        GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
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

fn instance(name: &[u16], descriptor: &LocalAllocation, first: bool) -> io::Result<OwnedHandle> {
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
            PIPE_ACCESS_DUPLEX | first,
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
            // SAFETY: a live pipe handle, waited on synchronously.
            if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } == 0
                // SAFETY: reads this thread's last error.
                && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
            {
                return Err(io::Error::last_os_error());
            }
            // The next client finds an instance waiting while this one is served.
            *next = Some(instance(&self.name, &self.descriptor, false)?);
            if peer_is(pipe.as_raw_handle(), true, &self.account)? {
                return Ok((Stream(File::from(pipe)), SocketAddr));
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

/// One end of a connected pipe.
#[derive(Debug)]
pub struct Stream(File);

impl Stream {
    /// Another handle to the same connection.
    ///
    /// # Errors
    /// The handle could not be duplicated.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    /// A pipe opened for synchronous I/O takes no timeout: its peer is this
    /// account's own process, and a call waits for it as a blocking socket
    /// without a timeout does. Accepted so a caller bounding a Unix peer needs
    /// no other path.
    ///
    /// # Errors
    /// Never.
    pub fn set_read_timeout(&self, _timeout: Option<std::time::Duration>) -> io::Result<()> {
        Ok(())
    }

    /// As [`Stream::set_read_timeout`].
    ///
    /// # Errors
    /// Never.
    pub fn set_write_timeout(&self, _timeout: Option<std::time::Duration>) -> io::Result<()> {
        Ok(())
    }
}

impl AsRawHandle for Stream {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.0).flush()
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

/// Open the client end of the pipe `name`, waiting out a busy one.
pub(crate) fn open(name: &[u16], overlapped: u32) -> io::Result<OwnedHandle> {
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
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | overlapped,
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
    let pipe = open(&wide(&pipe_name(&account, path)?), 0)?;
    if !peer_is(pipe.as_raw_handle(), false, &account)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: the pipe is served by another account", path.display()),
        ));
    }
    Ok(Stream(File::from(pipe)))
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
