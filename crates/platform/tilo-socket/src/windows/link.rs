//! A pipe this process makes both ends of, for a link it hands to a process it starts: Windows'
//! `socketpair(SOCK_SEQPACKET)`.
//!
//! The pipe is message-mode, so each write is read as one message, as a packet socket keeps its
//! packet boundaries. It has one instance, made with `FILE_FLAG_FIRST_PIPE_INSTANCE` under a name
//! no other pipe holds, rejects remote clients and carries the protected DACL every pipe here
//! does; this process opens the client end at once and checks that the client the server sees is
//! itself, so no other process can have joined it in between. A process given the client end
//! reaches the pipe through that handle alone, which no DACL check stands between.

use super::{
    BUFFER, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, PIPE_SDDL, Stream,
    open, overlapped, wide,
};
use crate::account::{Account, attributes};
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::{
    Foundation::{GetLastError, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_TYPE_PIPE, GetFileType,
        PIPE_ACCESS_DUPLEX,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeInfo,
            PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_SERVER_END, PIPE_TYPE_MESSAGE,
            PIPE_WAIT, PeekNamedPipe, SetNamedPipeHandleState,
        },
        Threading::{GetCurrentProcessId, INFINITE},
    },
};

/// A connected pair of message pipes, both ends in this process: the server end first.
///
/// # Errors
/// The system's error making or connecting the pipe, or `PermissionDenied` when a process other
/// than this one joined it.
pub fn pair() -> io::Result<(Stream, Stream)> {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let account = Account::current()?;
    let descriptor = account.descriptor(PIPE_SDDL)?;
    let attributes = attributes(&descriptor);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    let name = wide(&format!(
        r"\\.\pipe\tilo.link.{pid}.{}.{nanos:x}",
        MADE.fetch_add(1, Ordering::Relaxed)
    ));
    // SAFETY: a NUL-terminated name and security attributes that outlive the call.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER,
            BUFFER,
            0,
            &raw const attributes,
        )
    };
    if server == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateNamedPipeW just returned, owned from here.
    let server = unsafe { OwnedHandle::from_raw_handle(server) };
    let client = open(&name)?;
    let mode = PIPE_READMODE_MESSAGE;
    // SAFETY: a live client handle and the mode it reads in.
    if unsafe {
        SetNamedPipeHandleState(
            client.as_raw_handle(),
            &raw const mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a live pipe handle and the OVERLAPPED the call is waited on with.
    match overlapped(&server, INFINITE, None, |at| unsafe {
        ConnectNamedPipe(server.as_raw_handle(), at)
    }) {
        Ok(_) => {}
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {}
        Err(error) => return Err(error),
    }
    let mut client_pid = 0;
    // SAFETY: a connected server handle and a valid output pointer.
    if unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &raw mut client_pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if client_pid != pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "another process joined a link's pipe",
        ));
    }
    Ok((Stream::new(server, true)?, Stream::new(client, false)?))
}

impl Stream {
    /// The pipe `handle` names, handed to this process: inherited, or duplicated into it.
    ///
    /// # Errors
    /// `InvalidInput` for a handle that is no pipe, or the system's error asking which end it is.
    pub fn from_handle(handle: OwnedHandle) -> io::Result<Self> {
        // SAFETY: a live handle.
        if unsafe { GetFileType(handle.as_raw_handle()) } != FILE_TYPE_PIPE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a handle handed on as a connection is no pipe",
            ));
        }
        let mut flags = 0;
        // SAFETY: a live pipe handle and a valid output pointer; the rest are not asked for.
        if unsafe {
            GetNamedPipeInfo(
                handle.as_raw_handle(),
                &raw mut flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Self::new(handle, flags & PIPE_SERVER_END != 0)
    }

    /// The handle to this end of the pipe, to hand to another process.
    #[must_use]
    pub fn into_handle(self) -> OwnedHandle {
        self.pipe
    }

    /// What a peek at the pipe sees: the bytes waiting, or `None` once the other end is gone.
    fn peek(&self) -> io::Result<Option<u32>> {
        let mut waiting = 0;
        // SAFETY: a live pipe handle; only the count of bytes waiting is asked for.
        if unsafe {
            PeekNamedPipe(
                self.pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &raw mut waiting,
                std::ptr::null_mut(),
            )
        } != 0
        {
            return Ok(Some(waiting));
        }
        // SAFETY: reads this thread's last error, right after the call.
        match unsafe { GetLastError() } {
            ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Whether a read would answer at once: a message waits, or the end of the stream does.
    ///
    /// # Errors
    /// The system's error peeking at the pipe.
    pub fn ready(&self) -> io::Result<bool> {
        Ok(self.is_shut() || self.peek()?.is_none_or(|waiting| waiting > 0))
    }

    /// Whether this connection is over: shut down here, or its other end closed or was
    /// disconnected. Never waits.
    #[must_use]
    pub fn hung_up(&self) -> bool {
        self.is_shut() || matches!(self.peek(), Ok(None))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    #[test]
    fn a_pair_keeps_each_message_whole_and_reads_the_end() {
        let (mut server, mut client) = pair().unwrap();
        assert!(!client.ready().unwrap(), "nothing waits yet");
        server.write_all(b"one").unwrap();
        server.write_all(b"second").unwrap();
        assert!(client.ready().unwrap());
        let mut buffer = [0_u8; 64];
        let read = client.read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"one", "a read takes one message");
        let read = client.read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"second");
        client.write_all(b"back").unwrap();
        let read = server.read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"back");
        assert!(!server.hung_up());
        drop(client);
        assert!(server.hung_up(), "a closed client hangs up");
        assert!(server.ready().unwrap(), "the end of the stream waits");
        assert_eq!(server.read(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn an_end_handed_on_as_a_handle_is_the_same_pipe() {
        let (server, client) = pair().unwrap();
        let mut client = Stream::from_handle(client.into_handle()).unwrap();
        let mut server = Stream::from_handle(server.into_handle()).unwrap();
        client.write_all(b"hello").unwrap();
        let mut buffer = [0_u8; 8];
        let read = server.read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"hello");
        server.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(
            client.hung_up(),
            "the server end's shutdown disconnects its client"
        );
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            Stream::from_handle(file.into()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
