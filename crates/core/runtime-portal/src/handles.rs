//! Handing handles between processes on Windows, the way a broker does: only the privileged side
//! duplicates.
//!
//! A pipe carries bytes alone. A handle crosses it as its value in one of the two processes, and
//! the side that holds the other process (the daemon, which started it or checked whose it is)
//! duplicates it across: into the peer before it sends the value ([`Peer::push`]), or out of the
//! peer, closing it there, once it reads the value the peer sent ([`Peer::pull`]). The peer only
//! names its own handles and takes the ones named to it; it never holds any right over the
//! daemon's process, which would let a confined app reach everything the daemon holds.
//!
//! A peer is a process this one holds: one it started ([`crate::process_link::spawn`], which keeps
//! the handle the start returned, so no process that reuses its id is taken for it), or one it
//! opened and found running as this account, in this sign-in session and outside any
//! `AppContainer`. Any other process is refused.

use std::io;
use std::os::windows::io::{
    AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle, RawHandle,
};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

use tilo_socket::account::Account;

/// The processes this one started and still holds, by id.
static STARTED: Mutex<Vec<(u32, Weak<OwnedHandle>)>> = Mutex::new(Vec::new());

/// Keep `process`, which this process just started as `pid`, as a peer handles go to and come
/// from for as long as its starter holds it.
pub(crate) fn started(pid: u32, process: &Arc<OwnedHandle>) {
    let mut started = STARTED.lock().unwrap_or_else(PoisonError::into_inner);
    started.retain(|(_, held)| held.strong_count() > 0);
    started.push((pid, Arc::downgrade(process)));
}

/// Whether `process` still runs.
fn runs(process: &OwnedHandle) -> bool {
    // SAFETY: a live process handle, waited on for no time.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_TIMEOUT }
}

/// What a process that is no peer of this one is told.
fn refused(pid: u32, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("process {pid} gets and gives no handles here: {why}"),
    )
}

/// The sign-in session process `pid` runs in.
fn session_of(pid: u32) -> io::Result<u32> {
    let mut session = 0;
    // SAFETY: a valid output pointer; the id is only looked up.
    if unsafe { ProcessIdToSessionId(pid, &raw mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

/// A process this one hands handles to and takes them from.
#[derive(Debug)]
pub struct Peer {
    process: Arc<OwnedHandle>,
    pid: u32,
}

impl Peer {
    /// The process this one started, with the handle its start returned.
    pub(crate) const fn of_started(process: Arc<OwnedHandle>, pid: u32) -> Self {
        Self { process, pid }
    }

    /// The process `pid`, as a peer: one this process started and holds, or one of this
    /// account's own programs in this sign-in session.
    ///
    /// # Errors
    /// `PermissionDenied` for any other process: one of another account or session, a confined
    /// app this process did not start, one that ended, or this process itself, which is no peer.
    pub fn of(pid: u32) -> io::Result<Self> {
        if pid == std::process::id() {
            return Err(refused(pid, "it is this process"));
        }
        let held = STARTED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(started, _)| *started == pid)
            .find_map(|(_, held)| held.upgrade());
        // A process this one holds keeps its id: no other process can be given it meanwhile.
        if let Some(process) = held {
            return if runs(&process) {
                Ok(Self { process, pid })
            } else {
                Err(refused(pid, "it ended"))
            };
        }
        // Another person's session first: its processes may not even be opened by this one.
        let here = session_of(std::process::id())?;
        match session_of(pid) {
            Ok(there) if there != here => {
                return Err(refused(pid, "it runs in another sign-in session"));
            }
            Ok(_) => {}
            Err(error) => return Err(refused(pid, &format!("no such process ({error})"))),
        }
        // SAFETY: opens by id; the handle is owned at once, and every check below reads the
        // process it names, so a process that took a reused id is checked itself.
        let process = unsafe {
            OpenProcess(
                PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if process.is_null() {
            return Err(refused(
                pid,
                &format!(
                    "this process did not start it and cannot open it ({})",
                    io::Error::last_os_error()
                ),
            ));
        }
        // SAFETY: a handle OpenProcess just returned.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let theirs = Account::of_process_handle(&process)
            .map_err(|error| refused(pid, &format!("its account does not read ({error})")))?;
        let ours = Account::current()
            .map_err(|error| refused(pid, &format!("this account does not read ({error})")))?;
        if !theirs.is(&ours) {
            return Err(refused(pid, "it runs as another account"));
        }
        if theirs.session() != ours.session() {
            return Err(refused(pid, "it runs in another sign-in session"));
        }
        if theirs.is_app_container() {
            return Err(refused(pid, "it is an app this process did not start"));
        }
        if !runs(&process) {
            return Err(refused(pid, "it ended"));
        }
        Ok(Self {
            process: Arc::new(process),
            pid,
        })
    }

    /// Its process id.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Duplicate `handle`, this process's, into the peer: the value it names it by there, which
    /// the peer owns once it reads it.
    ///
    /// # Errors
    /// The system's error duplicating it.
    pub fn push(&self, handle: BorrowedHandle<'_>) -> io::Result<u64> {
        let mut theirs: HANDLE = std::ptr::null_mut();
        // SAFETY: a handle borrowed through the call, and the peer, which this process holds.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle.as_raw_handle(),
                self.process.as_raw_handle(),
                &raw mut theirs,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(theirs as usize as u64)
    }

    /// Take the handle the peer names `value` out of it: duplicated into this process and
    /// closed in the peer, whether or not the duplicate could be made.
    ///
    /// # Errors
    /// `InvalidData` when the peer holds no handle by that value.
    pub fn pull(&self, value: u64) -> io::Result<OwnedHandle> {
        let value = usize::try_from(value)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "no handle value"))?;
        let mut ours: HANDLE = std::ptr::null_mut();
        // SAFETY: the peer, which this process holds; the value is only looked up in it.
        let duplicated = unsafe {
            DuplicateHandle(
                self.process.as_raw_handle(),
                value as HANDLE,
                GetCurrentProcess(),
                &raw mut ours,
                0,
                0,
                DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
            )
        };
        if duplicated == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "process {} handed no handle {value:#x}: {}",
                    self.pid,
                    io::Error::last_os_error()
                ),
            ));
        }
        // SAFETY: a handle DuplicateHandle just made in this process.
        Ok(unsafe { OwnedHandle::from_raw_handle(ours) })
    }

    /// Close the handles `values` names in the peer: ones pushed that it never read, or ones it
    /// handed that nothing here takes.
    pub fn close(&self, values: &[u64]) {
        for value in values {
            let Ok(value) = usize::try_from(*value) else {
                continue;
            };
            // SAFETY: the peer, which this process holds; closing needs no target.
            unsafe {
                DuplicateHandle(
                    self.process.as_raw_handle(),
                    value as HANDLE,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE,
                );
            }
        }
    }
}

/// Give up a copy of `handle` for the privileged side to take ([`Peer::pull`]): its value, which
/// this process no longer owns.
///
/// # Errors
/// The system's error copying it.
pub fn release(handle: BorrowedHandle<'_>) -> io::Result<u64> {
    let mut copy: HANDLE = std::ptr::null_mut();
    // SAFETY: a handle borrowed through the call, copied within this process.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle.as_raw_handle(),
            GetCurrentProcess(),
            &raw mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(copy as usize as u64)
}

/// Close the copies [`release`] gave up that never left this process.
pub fn unrelease(values: &[u64]) {
    for value in values {
        if let Ok(value) = usize::try_from(*value) {
            // SAFETY: a copy `release` made and nothing else owns.
            drop(unsafe { OwnedHandle::from_raw_handle(value as RawHandle) });
        }
    }
}

/// The handle the privileged side pushed into this process as `value`, owned from here.
///
/// # Safety
/// `value` must be a handle the peer duplicated into this process for this caller alone, read
/// once.
pub unsafe fn pushed(value: u64) -> Option<OwnedHandle> {
    let value = usize::try_from(value).ok().filter(|value| *value != 0)?;
    // SAFETY: as the caller promises.
    Some(unsafe { OwnedHandle::from_raw_handle(value as RawHandle) })
}

#[cfg(test)]
#[path = "handles/tests.rs"]
mod tests;
