//! Confinement for Tilo apps on Windows.
//!
//! Each app runs in an `AppContainer` of its own, named for its package id: a token that carries
//! the container's SID, no capability (no network, no library folder, no device), and reaches
//! only what was granted to that SID. What the app's manifest grants it (its payload or runtime
//! to read, its own storage to keep) becomes an access entry for the SID on those paths
//! ([`Container::grant`]); nothing is granted to every container at once (`ALL APPLICATION
//! PACKAGES`), so one app's grants never reach another. The daemon's pipes admit only its own
//! account's non-container tokens (`tilo_socket`), so a container reaches none of them unless a
//! connection is handed to it.
//!
//! The process starts suspended through the one Windows process start
//! (`runtime_portal::process_link::spawn_confined`), inheriting only its link, the null device
//! and its error pipe, and joins a job before it runs. The job is held by [`Launched`]: when its
//! holder (the daemon) goes, the system ends the process and everything it started.
//!
//! An `AppContainer` needs Windows 8 or later; Wine has none, and a start there fails rather than
//! running the app unconfined.

#![cfg(windows)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use runtime_portal::process_link::{Child, Confinement, Descriptor};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, FreeSid, GetLengthSid, PSECURITY_DESCRIPTOR, PSID,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`: the container was made before, by an earlier
/// launch of the same app.
const ALREADY_EXISTS: i32 = 0x8007_00B7_u32.cast_signed();
/// `E_NOTIMPL`: a Windows with no `AppContainer` (Wine).
const NOT_IMPLEMENTED: i32 = 0x8000_4001_u32.cast_signed();

/// What a container may do with a path granted to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read and run what is there: an app's payload, the runtime it runs on.
    Read,
    /// Read, write, make and remove: the app's own storage.
    ReadWrite,
}

/// The `AppContainer` an app runs in, by its SID.
#[derive(Debug)]
pub struct Container {
    name: String,
    sid: Vec<u8>,
}

/// `text` as a NUL-terminated wide string.
fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = text.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a name or path holds a NUL",
        ));
    }
    Ok(wide.into_iter().chain([0]).collect())
}

impl Container {
    /// The container's name for `package_id`: one per app, the same on every launch, and within
    /// the 64 characters a container's name may have.
    #[must_use]
    pub fn name_for(package_id: &str) -> String {
        let hash = blake3::hash(package_id.as_bytes()).to_hex();
        format!("tilo.app.{}", &hash[..32])
    }

    /// The container of `package_id`, made the first time it is asked for.
    ///
    /// # Errors
    /// The system could not make or find it, or this Windows has no `AppContainer`.
    pub fn for_app(package_id: &str) -> io::Result<Self> {
        let name = Self::name_for(package_id);
        let wide_name = wide(OsStr::new(&name))?;
        let display = wide(OsStr::new(&format!("Tilo app {package_id}")))?;
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: NUL-terminated strings, no capabilities, and an output pointer.
        let made = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                std::ptr::null(),
                0,
                &raw mut sid,
            )
        };
        let made = if made == ALREADY_EXISTS {
            // SAFETY: a NUL-terminated name and an output pointer.
            unsafe { DeriveAppContainerSidFromAppContainerName(wide_name.as_ptr(), &raw mut sid) }
        } else {
            made
        };
        if made == NOT_IMPLEMENTED {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this Windows cannot confine an app in an AppContainer",
            ));
        }
        if made < 0 || sid.is_null() {
            return Err(io::Error::from_raw_os_error(made));
        }
        // SAFETY: a valid SID the call above returned, freed once copied.
        let sid = unsafe {
            let len = usize::try_from(GetLengthSid(sid)).unwrap_or(0);
            let bytes = std::slice::from_raw_parts(sid.cast::<u8>(), len).to_vec();
            FreeSid(sid);
            bytes
        };
        Ok(Self { name, sid })
    }

    /// The container's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The container's SID, as the system wrote it.
    #[must_use]
    pub fn sid(&self) -> &[u8] {
        &self.sid
    }

    /// Let this container reach `path`, and everything under it, as `access` says. The entry
    /// names this container's SID alone and is kept beside the path's other entries.
    ///
    /// # Errors
    /// The path's access list could not be read or written.
    pub fn grant(&self, path: &Path, access: Access) -> io::Result<()> {
        let name = wide(path.as_os_str())?;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path and output pointers; only the DACL is asked for.
        let read = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut dacl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if read != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(read.cast_signed()));
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: match access {
                Access::Read => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                Access::ReadWrite => FILE_ALL_ACCESS,
            },
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: self.sid.as_ptr().cast_mut().cast(),
            },
        };
        let mut merged: *mut ACL = std::ptr::null_mut();
        // SAFETY: one entry naming a SID this container holds, the path's DACL, which its
        // descriptor holds until freed below, and an output pointer.
        let set = unsafe { SetEntriesInAclW(1, &raw const entry, dacl, &raw mut merged) };
        let written = if set == ERROR_SUCCESS {
            // SAFETY: a NUL-terminated path and the merged DACL, which lives until freed below.
            unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    merged,
                    std::ptr::null(),
                )
            }
        } else {
            set
        };
        // SAFETY: allocations the calls above made (or null), each freed once.
        unsafe {
            LocalFree(merged.cast());
            LocalFree(descriptor);
        }
        if written != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(written.cast_signed()));
        }
        Ok(())
    }
}

/// What to start, confined: a program in the container of `package_id`, granted the paths it is
/// given and nothing else.
pub struct LaunchRequest {
    /// The app's package id: its container's.
    pub package_id: String,
    /// The program, its arguments, its environment and its directory.
    pub command: Command,
    /// What it reads and runs: its payload, its runtime.
    pub read: Vec<PathBuf>,
    /// What it keeps: its own storage.
    pub read_write: Vec<PathBuf>,
}

/// A process [`launch`] started, in its job.
#[derive(Debug)]
pub struct Launched {
    /// The process, its id and its error output.
    pub child: Child,
    /// The job its tree runs in, ended when this is dropped.
    job: OwnedHandle,
}

impl Launched {
    /// Its process id.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for it to end, and how it ended. Whatever it started ends with the job once this is
    /// dropped.
    ///
    /// # Errors
    /// The system's error waiting for it.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// The job its tree runs in.
    #[must_use]
    pub fn job(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.job.as_handle()
    }
}

/// A job that ends every process in it when its last handle closes.
fn kill_on_close_job() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes and no name: a job only this process holds.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateJobObjectW just returned, owned from here.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    // SAFETY: plain data, valid zeroed.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the job, and limits of the size the class names.
    if unsafe {
        SetInformationJobObject(
            std::os::windows::io::AsRawHandle::as_raw_handle(&job),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(u32::MAX),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Start `request` in its app's container, with its grants, in a job of its own, and the link
/// to it.
///
/// # Errors
/// The container, a grant, the job or the start failed; on a Windows without `AppContainer`, an
/// [`io::ErrorKind::Unsupported`] error.
pub fn launch(request: LaunchRequest) -> io::Result<(Descriptor, Launched)> {
    let container = Container::for_app(&request.package_id)?;
    for path in &request.read {
        container.grant(path, Access::Read)?;
    }
    for path in &request.read_write {
        container.grant(path, Access::ReadWrite)?;
    }
    let job = kill_on_close_job()?;
    let (link, child) = runtime_portal::process_link::spawn_confined(
        request.command,
        &Confinement {
            container: container.sid(),
            job: job.as_handle(),
        },
    )?;
    Ok((link, Launched { child, job }))
}

#[cfg(test)]
mod tests;
