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
    BUFFER, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    PIPE_SDDL, Stream, event, open, overlapped, wide,
};
use crate::account::{Account, attributes};
use std::io;
use std::os::windows::io::{AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use windows_sys::Win32::{
    Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED},
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_TYPE_PIPE, GetFileType,
        PIPE_ACCESS_DUPLEX,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeInfo,
            PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_SERVER_END, PIPE_TYPE_MESSAGE,
            PIPE_WAIT, PeekNamedPipe, SetNamedPipeHandleState,
        },
        Threading::{GetCurrentProcessId, INFINITE, WaitForMultipleObjects},
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
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED.cast_signed()) => {}
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

/// The most handles one wait watches: the system's bound on `WaitForMultipleObjects`.
const MOST_WATCHED: usize = 64;

/// A zero-byte read pending on one stream: it ends once a message waits on the pipe or its other
/// end goes, and reads nothing either way.
struct Pending<'a> {
    stream: &'a Stream,
    event: OwnedHandle,
    call: Box<OVERLAPPED>,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let pipe = self.stream.pipe.as_raw_handle();
        let mut moved = 0;
        // SAFETY: the call this owns on the stream's own pipe; GetOverlappedResult waits for a
        // cancelled call to end, so the OVERLAPPED outlives it.
        unsafe {
            CancelIoEx(pipe, &raw const *self.call);
            GetOverlappedResult(pipe, &raw const *self.call, &raw mut moved, 1);
        }
    }
}

/// Wait until one of `streams` has something to read (a message, or the end of the stream), one
/// of `signals` is signalled, or `timeout` passes (`None` waits without end): what `poll` is on a
/// socket. It reads nothing: ask each stream [`Stream::ready`] after.
///
/// # Errors
/// More than one wait watches (64 handles: two for each stream, one for each signal), or the system's error starting a read
/// or waiting.
pub fn wait(
    streams: &[&Stream],
    signals: &[BorrowedHandle<'_>],
    timeout: Option<Duration>,
) -> io::Result<()> {
    // Each stream is watched for a message and for its own shutdown.
    if streams
        .len()
        .saturating_mul(2)
        .saturating_add(signals.len())
        > MOST_WATCHED
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a wait watches at most 64 handles: two for each stream, one for each signal",
        ));
    }
    let mut pending = Vec::with_capacity(streams.len());
    for stream in streams {
        if stream.ready()? {
            return Ok(());
        }
        let event = event()?;
        // SAFETY: OVERLAPPED is plain data, valid zeroed.
        let mut call: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        call.hEvent = event.as_raw_handle();
        // SAFETY: a live pipe handle, an empty buffer, and an OVERLAPPED that stays where it is
        // until `Pending` has waited for the call to end.
        let started = unsafe {
            windows_sys::Win32::Storage::FileSystem::ReadFile(
                stream.pipe.as_raw_handle(),
                std::ptr::NonNull::<u8>::dangling().as_ptr(),
                0,
                std::ptr::null_mut(),
                &raw mut *call,
            )
        };
        // SAFETY: reads this thread's last error, right after the call.
        let ended_at_once = started != 0 || unsafe { GetLastError() } != ERROR_IO_PENDING;
        pending.push(Pending {
            stream,
            event,
            call,
        });
        // A message waits (a read of none answers that more data does), or the pipe ended.
        if ended_at_once {
            return Ok(());
        }
    }
    let handles: Vec<HANDLE> = pending
        .iter()
        .map(|pending| pending.event.as_raw_handle())
        .chain(
            signals
                .iter()
                .map(std::os::windows::io::AsRawHandle::as_raw_handle),
        )
        .chain(
            streams
                .iter()
                .map(|stream| stream.connection.shut.as_raw_handle()),
        )
        .collect();
    let wait_ms = timeout.map_or(INFINITE, |timeout| {
        u32::try_from(timeout.as_millis())
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1)
    });
    let count = u32::try_from(handles.len()).unwrap_or(u32::MAX);
    // SAFETY: live event and signal handles, as many as `count` says.
    let waited = unsafe { WaitForMultipleObjects(count, handles.as_ptr(), 0, wait_ms) };
    if waited == WAIT_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

    #[test]
    fn a_wait_ends_on_a_message_a_signal_a_hang_up_or_its_timeout() {
        let (mut server, client) = pair().unwrap();
        let other = pair().unwrap();
        let started = std::time::Instant::now();
        wait(
            &[&client, &other.1],
            &[],
            Some(std::time::Duration::from_millis(50)),
        )
        .unwrap();
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(40),
            "nothing woke it"
        );
        assert!(!client.ready().unwrap());
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            server.write_all(b"wake").unwrap();
            server
        });
        wait(&[&other.1, &client], &[], None).unwrap();
        assert!(client.ready().unwrap(), "a message ends the wait");
        assert!(!other.1.ready().unwrap());
        let mut buffer = [0_u8; 8];
        let read = (&client).read(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"wake", "the wait read nothing of it");
        let server = writer.join().unwrap();
        let signal = super::super::event().unwrap();
        // SAFETY: the event just made.
        unsafe { windows_sys::Win32::System::Threading::SetEvent(signal.as_raw_handle()) };
        wait(
            &[&client],
            &[std::os::windows::io::AsHandle::as_handle(&signal)],
            None,
        )
        .unwrap();
        assert!(
            !client.ready().unwrap(),
            "a signal ends it with nothing to read"
        );
        drop(server);
        wait(&[&client], &[], None).unwrap();
        assert!(client.hung_up(), "the other end going ends it");
    }
}
