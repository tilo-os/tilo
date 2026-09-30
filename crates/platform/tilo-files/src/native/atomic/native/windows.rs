//! Local-NTFS publication using write-through staging and held-file commits.

mod acl;
#[cfg(feature = "test-support")]
pub(crate) mod directory_probe;
#[cfg(test)]
mod tests;

use crate::native::atomic::identity::VolumeIdentity as PublicVolumeIdentity;
use crate::native::atomic::publication::{
    CommitGuard as PublicCommitGuard, Privacy, PublicationError, PublicationState, PublishMode,
};
#[cfg(feature = "test-support")]
use crate::native::atomic::test_support::{self, Checkpoint, CheckpointEvent, StageObservation};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Write},
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FILE_WRITE_THROUGH, FileFsDeviceInformation,
            NtCreateFile, NtQueryVolumeInformationFile,
        },
        System::SystemServices::{
            FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE, FILE_REMOTE_DEVICE_VSMB,
        },
    },
    Win32::{
        Foundation::{
            ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, HANDLE, RtlNtStatusToDosError, UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_DEVICE_DISK, FILE_DISPOSITION_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_TYPE_DISK, FileDispositionInfo, FileIdInfo, FileRenameInfoEx,
            FlushFileBuffers, GetFileInformationByHandle, GetFileInformationByHandleEx,
            GetFileType, GetVolumeInformationByHandleW, READ_CONTROL, SYNCHRONIZE,
            SetFileInformationByHandle,
        },
        System::{
            IO::IO_STATUS_BLOCK,
            SystemServices::{FILE_PERSISTENT_ACLS, FILE_SUPPORTS_POSIX_UNLINK_RENAME},
        },
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VolumeIdentity {
    serial: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) public_volume: PublicVolumeIdentity,
    id: [u8; 16],
}
impl Identity {
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        let mut info = FILE_ID_INFO::default();
        // SAFETY: retained handle, exact SDK structure and writable output.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&raw mut info).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            public_volume: PublicVolumeIdentity {
                inner: VolumeIdentity {
                    serial: info.VolumeSerialNumber,
                },
            },
            id: info.FileId.Identifier,
        })
    }
}

fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: File retains the native handle throughout both calls.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(refused("storage object is not a disk file"));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: exact SDK output and retained file handle.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(refused("storage object is a reparse point"));
    }
    Ok(info)
}

pub(crate) struct Directory {
    file: File,
}
impl Directory {
    pub(crate) fn from_file(file: File) -> io::Result<Self> {
        if information(&file)?.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "storage authority is not a directory",
            ));
        }
        admit_volume(&file)?;
        Ok(Self { file })
    }
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Self::from_file(
            File::options()
                .access_mode(
                    FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
                )
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?,
        )
    }
    pub(crate) fn validate_privacy(&self, privacy: Privacy) -> io::Result<()> {
        if privacy == Privacy::OwnerOnly {
            let principal = acl::Principal::current()?;
            acl::validate_private_security(&self.file, &principal)?;
        }
        Ok(())
    }
    pub(crate) fn create_private(&self, name: &OsStr) -> io::Result<File> {
        let principal = acl::Principal::current()?;
        let descriptor = principal.private_descriptor()?;
        let file = self.relative(name, true, Some(&descriptor))?;
        if let Err(error) = acl::validate_private(&file, &principal) {
            // Never delete by a reconstructed ambient path. If identity/name
            // cannot be proved, leave recovery evidence rather than guessing.
            let _ = self.remove_owned(name, &file);
            return Err(error);
        }
        Ok(file)
    }
    pub(crate) fn open_private(&self, name: &OsStr) -> io::Result<File> {
        let principal = acl::Principal::current()?;
        let file = self.relative(name, false, None)?;
        acl::validate_private(&file, &principal)?;
        Ok(file)
    }
    pub(crate) fn identity(&self) -> io::Result<Identity> {
        Identity::of(&self.file)
    }
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }
    pub(crate) fn open_read(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        if privacy == Privacy::OwnerOnly {
            return self.open_private(name);
        }
        let file = self.relative(name, false, None)?;
        if information(&file)?.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(refused("storage child is not a regular file"));
        }
        Ok(file)
    }
    fn create(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        if privacy == Privacy::OwnerOnly {
            return self.create_private(name);
        }
        let file = self.relative(name, true, None)?;
        if let Err(error) = information(&file) {
            let _ = self.remove_owned(name, &file);
            return Err(error);
        }
        Ok(file)
    }
    fn remove_owned(&self, name: &OsStr, file: &File) -> io::Result<()> {
        let held = Identity::of(file)?;
        let named = self.relative(name, false, None)?;
        if Identity::of(&named)? != held {
            return Err(refused("stage name no longer belongs to held file"));
        }
        // The trusted/excluded namespace contract is required: this is not a
        // hostile-child race defense. Delete the proven object through its
        // retained DELETE handle, never unlink a name after checking it.
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: retained owned handle and exact live SDK input.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const disposition).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn relative(
        &self,
        name: &OsStr,
        create: bool,
        descriptor: Option<&acl::Descriptor>,
    ) -> io::Result<File> {
        // NtCreateFile consumes counted UTF-16 without DOS normalization. Keep
        // unpaired surrogates intact; prohibit separators, ADS and traversal.
        let mut units = component(name)?;
        let length = (units.len() * 2) as u16;
        let mut name = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: units.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.file.as_raw_handle(),
            ObjectName: &raw mut name,
            Attributes: 0, // Deliberately no OBJ_INHERIT.
            SecurityDescriptor: descriptor.map_or(ptr::null_mut(), |d| d.as_ptr().cast()),
            SecurityQualityOfService: ptr::null_mut(),
        };
        let mut handle: HANDLE = ptr::null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: retained parent, bounded counted UTF-16, live descriptor,
        // non-inherited synchronous handle and exact SDK structures.
        let result = unsafe {
            NtCreateFile(
                &raw mut handle,
                FILE_GENERIC_READ
                    | if create {
                        FILE_GENERIC_WRITE | DELETE
                    } else {
                        0
                    },
                &raw const attributes,
                &raw mut status,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ | FILE_SHARE_DELETE | if create { 0 } else { FILE_SHARE_WRITE },
                if create { FILE_CREATE } else { FILE_OPEN },
                FILE_NON_DIRECTORY_FILE
                    | FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | if create { FILE_WRITE_THROUGH } else { 0 },
                ptr::null(),
                0,
            )
        };
        if result < 0 {
            // SAFETY: maps the returned native status; no ownership transfer.
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ));
        }
        if handle.is_null() {
            return Err(io::Error::other("NtCreateFile returned no owned handle"));
        }
        // SAFETY: successful synchronous NtCreateFile transfers one owned handle.
        let file = unsafe { File::from_raw_handle(handle) };
        // SAFETY: synchronous call completed and initialized the status union.
        check_status(unsafe { status.Anonymous.Status })?;
        Ok(file)
    }
}

fn check_status(status: i32) -> io::Result<()> {
    if status < 0 {
        // SAFETY: pure SDK conversion of a returned native status.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(())
}

fn component(name: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.is_empty()
        || units.len() > 255
        || units == [46]
        || units == [46, 46]
        || units.iter().any(|u| matches!(u, 0 | 47 | 92 | 58))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage child must be one native component",
        ));
    }
    Ok(units)
}

fn admit_volume(file: &File) -> io::Result<()> {
    let mut device = FILE_FS_DEVICE_INFORMATION::default();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: held directory and exact bounded SDK output buffers; this
    // query returns its final completion status (no async callback).
    let result = unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            (&raw mut device).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    check_status(result)?;
    // SAFETY: synchronous native query initialized the completion union.
    check_status(unsafe { status.Anonymous.Status })?;
    if status.Information < size_of::<FILE_FS_DEVICE_INFORMATION>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete native volume device evidence",
        ));
    }
    // Positive device evidence, not an NTFS string or a failed remote query.
    if device.DeviceType != FILE_DEVICE_DISK
        || device.Characteristics & (FILE_REMOTE_DEVICE | FILE_REMOTE_DEVICE_VSMB) != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "publication requires a local disk device",
        ));
    }
    let mut fs_name = [0u16; 261];
    let mut flags = 0;
    // SAFETY: retained directory; optional outputs null, name buffer fully sized.
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let required = FILE_PERSISTENT_ACLS | FILE_SUPPORTS_POSIX_UNLINK_RENAME;
    if fs_name.get(..5) != Some(&[78, 84, 70, 83, 0]) || flags & required != required {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "publication requires NTFS with persistent ACLs and extended POSIX rename",
        ));
    }
    Ok(())
}

/// Same file object throughout write-through creation, rename and final flush.
/// Parent ownership is retained for the complete in-process recovery lifetime.
/// Drop closes handles only; it never rolls back a published name.
pub(crate) struct CommitGuard {
    file: File,
    _parent: Directory,
}
impl CommitGuard {
    pub(crate) fn commit(&self) -> io::Result<()> {
        checkpoint(CheckpointKind::BeforeCommit, &self.file)?;
        flush(&self.file)?;
        checkpoint(CheckpointKind::Committed, &self.file)
    }
}

fn flush(file: &File) -> io::Result<()> {
    // SAFETY: retained writable regular-file handle, never a directory handle.
    if unsafe { FlushFileBuffers(file.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Cleanup borrows custody only until publication submission. This separates
// potentially deleting prepublication cleanup from the non-deleting commit guard.
struct StageCleanup<'a> {
    custody: &'a CommitGuard,
    name: OsString,
    armed: bool,
}
impl Drop for StageCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .custody
                ._parent
                .remove_owned(&self.name, &self.custody.file);
        }
    }
}
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn publish(
    directory: &Directory,
    target: &OsStr,
    bytes: &[u8],
    mode: PublishMode,
    privacy: Privacy,
    exact_mode: Option<u32>,
) -> Result<(), PublicationError> {
    let units = component(target).map_err(not_published)?;
    if exact_mode.is_some() {
        return Err(not_published(io::Error::new(
            io::ErrorKind::Unsupported,
            "native Windows publication requires semantic privacy",
        )));
    }
    let parent = directory.try_clone().map_err(not_published)?;
    // Allocate and validate every rename parameter before namespace submission.
    let rename = RenameBuffer::new(&parent, &units, mode).map_err(not_published)?;
    let mut staged = None;
    for _ in 0..32 {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        // Independent of the target's length/encoding. Native targets remain
        // raw UTF-16; max-length target components still have valid siblings.
        let name = OsString::from(format!(".atomic-{}.{}.tmp", std::process::id(), sequence));
        match parent.create(&name, privacy) {
            Ok(file) => {
                staged = Some((file, name));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(not_published(error)),
        }
    }
    let (file, name) = staged.ok_or_else(|| {
        not_published(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate exclusive publication stage",
        ))
    })?;
    let guard = CommitGuard {
        file,
        _parent: parent,
    };
    let mut cleanup = StageCleanup {
        custody: &guard,
        name,
        armed: true,
    };
    checkpoint(CheckpointKind::StageCreated, &guard.file).map_err(not_published)?;
    (&guard.file).write_all(bytes).map_err(not_published)?;
    checkpoint(CheckpointKind::BeforeDataFlush, &guard.file).map_err(not_published)?;
    flush(&guard.file).map_err(not_published)?;
    checkpoint(CheckpointKind::DataFlushed, &guard.file).map_err(not_published)?;
    checkpoint(CheckpointKind::BeforePublish, &guard.file).map_err(not_published)?;

    // A native write-through error may follow actual namespace effects. No
    // deleting destructor may run after submission without proof of collision.
    cleanup.armed = false;
    checkpoint(CheckpointKind::PublishSubmitted, &guard.file).map_err(unknown)?;
    if let Err(error) = rename.submit(&guard.file) {
        if mode == PublishMode::IfAbsent
            && matches!(error.raw_os_error(), Some(code) if code == ERROR_ALREADY_EXISTS as i32 || code == ERROR_FILE_EXISTS as i32)
        {
            // A proven no-replace collision has no target effects. Cleanup
            // still independently proves this stage's held identity/name.
            cleanup.armed = true;
            return Err(not_published(error));
        }
        return Err(unknown(error));
    }
    // This test failure represents a lost native acknowledgment, not a commit
    // failure. Keep semantics identical to the Unix backend's fault model.
    checkpoint(CheckpointKind::AfterPublish, &guard.file).map_err(unknown)?;
    drop(cleanup);
    // Custody moves without allocation, native reopen or fallible transfer.
    let guard = PublicCommitGuard::new(guard);
    if let Err(error) = guard.commit() {
        return Err(PublicationError::pending(error, guard));
    }
    Ok(())
}

fn not_published(error: io::Error) -> PublicationError {
    PublicationError::new(PublicationState::NotPublished, error)
}
fn unknown(error: io::Error) -> PublicationError {
    PublicationError::new(PublicationState::PublicationUnknown, error)
}

struct RenameBuffer {
    words: Vec<usize>,
    bytes: u32,
}
impl RenameBuffer {
    fn new(parent: &Directory, units: &[u16], mode: PublishMode) -> io::Result<Self> {
        let invalid =
            || io::Error::new(io::ErrorKind::InvalidInput, "rename buffer length overflow");
        let name_bytes = units
            .len()
            .checked_mul(size_of::<u16>())
            .ok_or_else(invalid)?;
        let bytes = size_of::<FILE_RENAME_INFO>()
            .checked_add(name_bytes)
            .ok_or_else(invalid)?;
        let word_count = bytes
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(invalid)?
            / size_of::<usize>();
        let mut words = Vec::<usize>::new();
        words
            .try_reserve_exact(word_count)
            .map_err(io::Error::other)?;
        words.resize(word_count, 0);
        let flags = if mode == PublishMode::Replace {
            FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS
        } else {
            0
        };
        let info = FILE_RENAME_INFO {
            Anonymous: FILE_RENAME_INFO_0 { Flags: flags },
            RootDirectory: parent.file.as_raw_handle(),
            FileNameLength: u32::try_from(name_bytes).map_err(|_| invalid())?,
            FileName: [0],
        };
        // SAFETY: usize allocation has the struct's pointer alignment on both
        // Windows architectures; checked size covers the struct plus UTF-16.
        // Copy via allocation pointer, not beyond the one-element Rust array.
        unsafe {
            words.as_mut_ptr().cast::<FILE_RENAME_INFO>().write(info);
            ptr::copy_nonoverlapping(
                units.as_ptr(),
                words
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(offset_of!(FILE_RENAME_INFO, FileName))
                    .cast::<u16>(),
                units.len(),
            );
        }
        Ok(Self {
            words,
            bytes: u32::try_from(bytes).map_err(|_| invalid())?,
        })
    }
    fn submit(&self, file: &File) -> io::Result<()> {
        // SAFETY: aligned live input; parent and source handles remain retained.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfoEx,
                self.words.as_ptr().cast(),
                self.bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum CheckpointKind {
    StageCreated,
    BeforeDataFlush,
    DataFlushed,
    BeforePublish,
    PublishSubmitted,
    AfterPublish,
    BeforeCommit,
    Committed,
}
#[cfg(feature = "test-support")]
fn checkpoint(kind: CheckpointKind, file: &File) -> io::Result<()> {
    let checkpoint = match kind {
        CheckpointKind::StageCreated => Checkpoint::StageCreated,
        CheckpointKind::BeforeDataFlush => Checkpoint::BeforeDataFlush,
        CheckpointKind::DataFlushed => Checkpoint::DataFlushed,
        CheckpointKind::BeforePublish => Checkpoint::BeforePublish,
        CheckpointKind::PublishSubmitted => Checkpoint::PublishSubmitted,
        CheckpointKind::AfterPublish => Checkpoint::AfterPublish,
        CheckpointKind::BeforeCommit => Checkpoint::BeforeCommit,
        CheckpointKind::Committed => Checkpoint::Committed,
    };
    test_support::checkpoint(&CheckpointEvent {
        checkpoint,
        stage: Some(StageObservation {
            metadata: file.metadata()?,
            identity: crate::native::atomic::FileIdentity::of(file)?,
        }),
    })
}
#[cfg(not(feature = "test-support"))]
fn checkpoint(_kind: CheckpointKind, _file: &File) -> io::Result<()> {
    Ok(())
}
