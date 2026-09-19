//! Connected-only native handles. A completed flush is an operation receipt,
//! never platform qualification or permission to publish an endpoint change.
#[cfg(windows)]
pub(super) use super::store::WindowsFileId;
use std::io;

fn component(units: &[u16]) -> io::Result<Vec<u16>> {
    if units.is_empty()
        || units.len() > 255
        || units == [46]
        || units == [46, 46]
        || units.iter().any(|u| matches!(*u, 0 | 47 | 92 | 58))
        || char::decode_utf16(units.iter().copied()).any(|u| u.is_err())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bounded native component required",
        ));
    }
    u16::try_from(
        units
            .len()
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("UTF-16 overflow"))?,
    )
    .map_err(|_| io::Error::other("UNICODE_STRING overflow"))?;
    Ok(units.to_vec())
}
fn trailing_layout(
    offset: usize,
    units: usize,
    struct_size: usize,
    alignment: usize,
) -> io::Result<(usize, usize)> {
    let bytes = units
        .checked_mul(2)
        .and_then(|n| n.checked_add(offset))
        .ok_or_else(|| io::Error::other("native buffer overflow"))?
        .max(struct_size);
    let allocation = bytes
        .checked_add(alignment - 1)
        .ok_or_else(|| io::Error::other("native buffer overflow"))?
        / alignment;
    Ok((bytes, allocation))
}
#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn nt_components_reject_streams_separators_nul_and_bounds() {
        for text in ["", ".", "..", "a/b", "a\\b", "a:b", "a\0b"] {
            assert!(
                component(&text.encode_utf16().collect::<Vec<_>>()).is_err(),
                "{text:?}"
            );
        }
        assert!(component(&vec![u16::from(b'x'); 256]).is_err());
        assert!(component(&vec![u16::from(b'x'); 255]).is_ok());
        assert!(component(&[0xd800]).is_err());
        assert!(component(&[0xd83d, 0xde00]).is_ok());
    }
    #[test]
    fn native_trailing_buffer_layout_is_checked_and_aligned() {
        assert_eq!(trailing_layout(20, 3, 24, 8).unwrap(), (26, 4));
        assert!(trailing_layout(20, usize::MAX, 24, 8).is_err());
        assert!(trailing_layout(usize::MAX, 1, 24, 8).is_err());
    }
}

#[cfg(windows)]
mod native {
    use super::super::endpoint;
    use super::{WindowsFileId, component, io, trailing_layout};
    use crate::destination_security;
    use std::{
        ffi::OsStr,
        fs::File,
        mem::{offset_of, size_of},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::{Component, Path, Prefix},
        ptr,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    };
    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
            },
        },
        Win32::{
            Foundation::{HANDLE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError, UNICODE_STRING},
            Storage::FileSystem::{
                DELETE, FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO,
                FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FileDispositionInfo, FileIdInfo,
                FileRenameInfo, FlushFileBuffers, GetFileInformationByHandleEx, READ_CONTROL,
                ReOpenFile, SYNCHRONIZE, SetFileInformationByHandle,
            },
            System::{
                IO::{DeviceIoControl, IO_STATUS_BLOCK},
                Ioctl::IOCTL_STORAGE_CHECK_VERIFY2,
            },
        },
    };

    fn refused() -> io::Error {
        io::Error::other("Connected native identity or session changed")
    }
    fn evidence<T>(result: Result<T, endpoint::EndpointError>) -> io::Result<T> {
        result.map_err(|error| {
            let kind = match error {
                endpoint::EndpointError::PermissionDenied => io::ErrorKind::PermissionDenied,
                endpoint::EndpointError::Offline => io::ErrorKind::NotConnected,
                endpoint::EndpointError::ReadOnly => io::ErrorKind::ReadOnlyFilesystem,
                endpoint::EndpointError::Unsupported | endpoint::EndpointError::Unverified => {
                    io::ErrorKind::Unsupported
                }
                _ => io::ErrorKind::Other,
            };
            io::Error::new(kind, format!("Connected native evidence: {error:?}"))
        })
    }
    pub(in super::super) fn identity(file: &File) -> io::Result<WindowsFileId> {
        let mut id = FILE_ID_INFO::default();
        // SAFETY: live handle and exact SDK structure/output length.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&raw mut id).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(WindowsFileId {
            volume_serial: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        })
    }
    #[derive(Clone, Copy)]
    pub(in super::super) enum Role {
        Directory,
        Lock,
        Writer,
        RenameWriter,
        StableRead,
        Reread,
        Named,
    }
    impl Role {
        fn access(self) -> u32 {
            match self {
                Self::Directory => {
                    FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE
                }
                Self::Lock | Self::Writer => FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                Self::RenameWriter => FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                Self::StableRead | Self::Reread => FILE_GENERIC_READ,
                Self::Named => FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            }
        }
        fn share(self) -> u32 {
            match self {
                Self::Directory | Self::Lock => FILE_SHARE_READ | FILE_SHARE_WRITE,
                Self::Reread | Self::Named => {
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
                }
                _ => FILE_SHARE_READ,
            }
        }
    }
    fn relative(
        parent: &File,
        name: &OsStr,
        directory: bool,
        role: Role,
        create: bool,
    ) -> io::Result<File> {
        let mut units = component(&name.encode_wide().collect::<Vec<_>>())?;
        let length = u16::try_from(units.len() * 2).map_err(|_| refused())?;
        let mut unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: units.as_mut_ptr(),
        };
        let mut call = |descriptor: *mut std::ffi::c_void| {
            let attributes = OBJECT_ATTRIBUTES {
                Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
                RootDirectory: parent.as_raw_handle(),
                ObjectName: &raw mut unicode,
                Attributes: 0,
                SecurityDescriptor: descriptor.cast(),
                SecurityQualityOfService: ptr::null_mut(),
            };
            let mut handle: HANDLE = ptr::null_mut();
            let mut status = IO_STATUS_BLOCK::default();
            // SAFETY: bounded counted UTF-16 name, retained parent, exact SDK
            // structures, synchronous open, descriptor scoped by the caller.
            let result = unsafe {
                NtCreateFile(
                    &raw mut handle,
                    role.access(),
                    &raw const attributes,
                    &raw mut status,
                    ptr::null(),
                    FILE_ATTRIBUTE_NORMAL,
                    role.share(),
                    if create { FILE_CREATE } else { FILE_OPEN },
                    FILE_OPEN_REPARSE_POINT
                        | FILE_SYNCHRONOUS_IO_NONALERT
                        | if directory {
                            FILE_DIRECTORY_FILE
                        } else {
                            FILE_NON_DIRECTORY_FILE
                        },
                    ptr::null(),
                    0,
                )
            };
            if result < 0 {
                return Err(io::Error::from_raw_os_error(
                    unsafe { RtlNtStatusToDosError(result) } as i32,
                ));
            }
            // SAFETY: successful NtCreateFile transfers one owned file handle.
            Ok(unsafe { File::from_raw_handle(handle) })
        };
        let file = if create {
            destination_security::with_private_creation_descriptor(call)?
        } else {
            call(ptr::null_mut())?
        };
        evidence(endpoint::connected_metadata(&file, directory))?;
        Ok(file)
    }
    pub(in super::super) struct WindowsDirectory {
        pub(in super::super) file: File,
        id: WindowsFileId,
        parent: Option<(Arc<Self>, std::ffi::OsString)>,
        private: bool,
        barrier: Option<Arc<NtfsVolumeBarrier>>,
    }
    impl WindowsDirectory {
        fn anchor(path: &Path) -> io::Result<Arc<Self>> {
            let mut parts = path.components();
            let Some(Component::Prefix(prefix)) = parts.next() else {
                return Err(refused());
            };
            if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
                || parts.next() != Some(Component::RootDir)
            {
                return Err(refused());
            }
            let mut root = prefix.as_os_str().to_os_string();
            root.push("\\");
            let file = std::fs::OpenOptions::new()
                .access_mode(Role::Directory.access())
                .share_mode(Role::Directory.share())
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(root)?;
            evidence(endpoint::connected_metadata(&file, true))?;
            destination_security::check_directory(&file, None, false)?;
            let mut result = Arc::new(Self {
                id: identity(&file)?,
                file,
                parent: None,
                private: false,
                barrier: None,
            });
            for (depth, part) in parts.enumerate() {
                if depth >= 64 {
                    return Err(refused());
                }
                let Component::Normal(name) = part else {
                    return Err(refused());
                };
                result = result.child(name, false, false)?;
            }
            result.validate_destination()?;
            Ok(result)
        }
        fn child(
            self: &Arc<Self>,
            name: &OsStr,
            create: bool,
            private: bool,
        ) -> io::Result<Arc<Self>> {
            self.validate_destination()?;
            if create {
                self.authority()?.validate()?;
            }
            let file = relative(&self.file, name, true, Role::Directory, create)?;
            let id = identity(&file)?;
            if id.volume_serial != self.id.volume_serial {
                return Err(refused());
            }
            if private {
                destination_security::require_private_protected(&file)?;
            } else {
                destination_security::check_directory(&file, None, false)?;
            }
            let child = Arc::new(Self {
                file,
                id,
                parent: Some((Arc::clone(self), name.to_owned())),
                private,
                barrier: self.barrier.clone(),
            });
            child.validate_destination()?;
            Ok(child)
        }
        pub(in super::super) fn create_child(
            self: &Arc<Self>,
            name: &OsStr,
        ) -> io::Result<Arc<Self>> {
            self.child(name, true, true)
        }
        pub(in super::super) fn open_child(
            self: &Arc<Self>,
            name: &OsStr,
            _: &cap_primitives::fs::Metadata,
        ) -> io::Result<Arc<Self>> {
            self.child(name, false, true)
        }
        pub(in super::super) fn validate_destination(&self) -> io::Result<()> {
            evidence(endpoint::connected_metadata(&self.file, true))?;
            if identity(&self.file)? != self.id {
                return Err(refused());
            }
            if self.private {
                destination_security::require_private_protected(&self.file)?;
            } else {
                destination_security::check_directory(&self.file, None, false)?;
            }
            if let Some((parent, name)) = &self.parent {
                parent.validate_destination()?;
                let named = relative(&parent.file, name, true, Role::Named, false)?;
                if identity(&named)? != self.id {
                    return Err(refused());
                }
            }
            Ok(())
        }
        fn authority(&self) -> io::Result<&Arc<NtfsVolumeBarrier>> {
            self.barrier.as_ref().ok_or_else(refused)
        }
        pub(in super::super) fn validate_authority(&self) -> io::Result<()> {
            self.authority()?.validate()
        }
        pub(in super::super) fn sync(&self) -> io::Result<()> {
            self.authority()?.flush_namespace(&[self]).map(|_| ())
        }
        pub(in super::super) fn entries_file(&self) -> io::Result<File> {
            self.validate_destination()?;
            // SAFETY: live directory; separately requested enumeration access.
            let handle = unsafe {
                ReOpenFile(
                    self.file.as_raw_handle(),
                    FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let file = unsafe { File::from_raw_handle(handle) };
            if identity(&file)? != self.id {
                return Err(refused());
            }
            Ok(file)
        }
    }
    pub(in super::super) fn open_file(
        parent: &WindowsDirectory,
        name: &str,
        role: Role,
        create: bool,
    ) -> io::Result<File> {
        parent.validate_destination()?;
        if create {
            parent.authority()?.validate()?;
        }
        let file = relative(&parent.file, OsStr::new(name), false, role, create)?;
        destination_security::require_private_protected(&file)?;
        if identity(&file)?.volume_serial != parent.id.volume_serial {
            return Err(refused());
        }
        validate_named(parent, name, &file, identity(&file)?)?;
        Ok(file)
    }
    pub(in super::super) fn validate_named(
        parent: &WindowsDirectory,
        name: &str,
        file: &File,
        expected: WindowsFileId,
    ) -> io::Result<()> {
        parent.validate_destination()?;
        evidence(endpoint::connected_metadata(file, false))?;
        destination_security::require_private_protected(file)?;
        let named = relative(&parent.file, OsStr::new(name), false, Role::Named, false)?;
        destination_security::require_private_protected(&named)?;
        if identity(file)? != expected || identity(&named)? != expected {
            return Err(refused());
        }
        Ok(())
    }
    pub(in super::super) fn flush_file(file: &File) -> io::Result<()> {
        // SAFETY: owned file opened writable, ordinary full flush.
        if unsafe { FlushFileBuffers(file.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(in super::super) struct NtfsVolumeBarrier {
        volume: File,
        root: Arc<WindowsDirectory>,
        binding: Vec<u8>,
        media_count: u32,
        live: AtomicBool,
        step: AtomicU64,
        session: Arc<()>,
    }
    #[must_use]
    pub(in super::super) struct CompletedBarrier {
        session: Arc<()>,
        step: u64,
    }
    fn media_count(volume: &File) -> io::Result<u32> {
        let mut value = 0_u32;
        let mut returned = 0;
        // SAFETY: read-only media-change query on the retained volume. A stack
        // unable to supply this continuity evidence refuses the candidate.
        if unsafe {
            DeviceIoControl(
                volume.as_raw_handle(),
                IOCTL_STORAGE_CHECK_VERIFY2,
                ptr::null(),
                0,
                (&raw mut value).cast(),
                4,
                &raw mut returned,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if returned != 4 {
            return Err(refused());
        }
        Ok(value)
    }
    impl NtfsVolumeBarrier {
        // Deliberately private. There is no product authority provider or
        // qualification constructor in this slice. Only explicit native tests
        // may reach this authenticating candidate, never public open_at.
        fn authenticate(root: Arc<WindowsDirectory>) -> io::Result<Arc<Self>> {
            root.validate_destination()?;
            let expected = evidence(endpoint::connected_root_binding(&root.file))?;
            let mut path = evidence(endpoint::connected_volume_path(&root.file))?;
            if path.pop() != Some(92) {
                return Err(refused());
            }
            let volume = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(std::ffi::OsString::from_wide(&path))?;
            let binding = evidence(endpoint::connected_volume_binding(&root.file, &volume))?;
            if binding != expected {
                return Err(refused());
            }
            let media_count = media_count(&volume)?;
            let result = Arc::new(Self {
                volume,
                root,
                binding,
                media_count,
                live: AtomicBool::new(true),
                step: AtomicU64::new(0),
                session: Arc::new(()),
            });
            result.validate()?;
            Ok(result)
        }
        pub(in super::super) fn revoke(&self) {
            self.live.store(false, Ordering::Release);
        }
        fn validate(&self) -> io::Result<()> {
            let result = (|| {
                if !self.live.load(Ordering::Acquire) {
                    return Err(refused());
                }
                self.root.validate_destination()?;
                if media_count(&self.volume)? != self.media_count
                    || evidence(endpoint::connected_volume_binding(
                        &self.root.file,
                        &self.volume,
                    ))? != self.binding
                {
                    return Err(refused());
                }
                Ok(())
            })();
            if result.is_err() {
                self.revoke();
            }
            result
        }
        pub(in super::super) fn flush_namespace(
            &self,
            directories: &[&WindowsDirectory],
        ) -> io::Result<CompletedBarrier> {
            let result = (|| {
                self.validate()?;
                if directories.is_empty() {
                    return Err(refused());
                }
                let mut seen = Vec::new();
                for directory in directories {
                    directory.validate_destination()?;
                    if !std::ptr::eq(self, directory.authority()?.as_ref()) {
                        return Err(refused());
                    }
                    if directory.id.volume_serial != self.root.id.volume_serial
                        || evidence(endpoint::connected_volume_path(&directory.file))?
                            != evidence(endpoint::connected_volume_path(&self.root.file))?
                    {
                        return Err(refused());
                    }
                    if !seen.contains(&directory.id) {
                        seen.push(directory.id);
                    }
                }
                flush_file(&self.volume)?;
                self.validate()?;
                for directory in directories {
                    directory.validate_destination()?;
                }
                let step = self
                    .step
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
                    .map_err(|_| refused())?;
                Ok(CompletedBarrier {
                    session: Arc::clone(&self.session),
                    step,
                })
            })();
            if result.is_err() {
                self.revoke();
            }
            result
        }
    }
    fn rename(file: &File, parent: &WindowsDirectory, name: &str, replace: bool) -> io::Result<()> {
        parent.validate_destination()?;
        parent.authority()?.validate()?;
        if identity(file)?.volume_serial != parent.id.volume_serial {
            return Err(refused());
        }
        let units = component(&name.encode_utf16().collect::<Vec<_>>())?;
        let (bytes, _) = trailing_layout(
            offset_of!(FILE_RENAME_INFO, FileName),
            units.len(),
            size_of::<FILE_RENAME_INFO>(),
            std::mem::align_of::<FILE_RENAME_INFO>(),
        )?;
        // Allocate SDK structs to preserve their ABI alignment (including union).
        let count = bytes.div_ceil(size_of::<FILE_RENAME_INFO>());
        let mut buffer = vec![FILE_RENAME_INFO::default(); count];
        let info = buffer.as_mut_ptr();
        // SAFETY: aligned allocation has at least bytes capacity; trailing name
        // is bounded and copied to the SDK's flexible array at its native offset.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = replace;
            (*info).RootDirectory = parent.file.as_raw_handle();
            (*info).FileNameLength = (units.len() * 2) as u32;
            ptr::copy_nonoverlapping(
                units.as_ptr(),
                ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                units.len(),
            );
            if SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                u32::try_from(bytes).map_err(|_| refused())?,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    pub(in super::super) fn rename_noreplace(
        file: &File,
        parent: &WindowsDirectory,
        name: &str,
    ) -> io::Result<()> {
        rename(file, parent, name, false)
    }
    // Only the store adapter can request the one replacement operation.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "consume the store-issued move-only replacement capability"
    )]
    pub(in super::super) fn replace_current(
        file: &File,
        permit: super::super::store::CurrentReplacement<'_>,
    ) -> io::Result<()> {
        rename(file, permit.root(), "CURRENT", true)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "consume the store-issued move-only deletion capability"
    )]
    pub(in super::super) fn delete_captured(
        file: File,
        permit: super::super::store::GcDeletion<'_>,
    ) -> io::Result<()> {
        let (parent, name, expected) = permit.parts();
        if !matches!(name, "captured" | "intent") {
            return Err(refused());
        }
        validate_named(parent, name, &file, expected)?;
        parent.authority()?.validate()?;
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: exact opened private capture/intent, caller verified committed
        // reachability intent, SDK structure and owning DELETE-capable handle.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        drop(file);
        parent.sync()?;
        match relative(&parent.file, OsStr::new(name), false, Role::Named, false) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(refused()),
        }
    }
    #[cfg(test)]
    pub(in super::super) fn fixture_anchor(path: &Path) -> io::Result<Arc<WindowsDirectory>> {
        let anchor = WindowsDirectory::anchor(path)?;
        let barrier = NtfsVolumeBarrier::authenticate(Arc::clone(&anchor))?;
        Ok(Arc::new(WindowsDirectory {
            file: anchor.file.try_clone()?,
            id: anchor.id,
            parent: anchor.parent.clone(),
            private: false,
            barrier: Some(barrier),
        }))
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
        fn fixture() -> (tempfile::TempDir, Arc<WindowsDirectory>) {
            let selected = std::env::var_os("QOS_CONNECTED_WINDOWS_FIXTURE").unwrap();
            let temp = tempfile::tempdir_in(selected).unwrap();
            let anchor = fixture_anchor(&temp.path().canonicalize().unwrap()).unwrap();
            let directory = anchor.create_child(OsStr::new("native")).unwrap();
            anchor.sync().unwrap();
            directory.sync().unwrap();
            (temp, directory)
        }
        #[test]
        #[ignore = "UNRUN: real retained-volume flush/revocation and wrong-session ordering"]
        fn native_windows_barrier_flush_and_revocation() {
            let (_temp, directory) = fixture();
            let authority = directory.authority().unwrap();
            let completed = authority
                .flush_namespace(&[&directory, &directory])
                .unwrap();
            assert!(Arc::ptr_eq(&completed.session, &authority.session));
            assert_eq!(
                completed.step.checked_add(1),
                Some(authority.step.load(Ordering::Acquire))
            );
            let before = authority.step.load(Ordering::Acquire);
            // Explicit session revocation is process-local, never a grant.
            authority.revoke();
            assert!(directory.sync().is_err());
            assert_eq!(authority.step.load(Ordering::Acquire), before);
            assert!(directory.create_child(OsStr::new("after-revoke")).is_err());
            assert!(!directory.file.metadata().unwrap().is_file());
        }
        #[test]
        #[ignore = "UNRUN: two independently authenticated native sessions cannot substitute for one another"]
        fn native_windows_wrong_barrier_invalidates_without_receipt() {
            let (_temp, directory) = fixture();
            let (_other_temp, other) = fixture();
            let authority = directory.authority().unwrap();
            let before = authority.step.load(Ordering::Acquire);
            assert!(authority.flush_namespace(&[&other]).is_err());
            assert_eq!(authority.step.load(Ordering::Acquire), before);
            assert!(!authority.live.load(Ordering::Acquire));
        }
        #[test]
        #[ignore = "UNRUN: actual hardlink, named-stream, reparse and private ACL refusal"]
        fn native_windows_private_metadata_and_ancestor_guards() {
            use windows_sys::Win32::Security::{
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
                DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
            };
            let (temp, directory) = fixture();
            let path = temp.path().join("native");
            for name in ["hardlink", "stream", "unprotected"] {
                let mut file = open_file(&directory, name, Role::Writer, true).unwrap();
                file.write_all(b"fixture").unwrap();
                flush_file(&file).unwrap();
            }
            std::fs::hard_link(path.join("hardlink"), path.join("alias")).unwrap();
            assert!(open_file(&directory, "hardlink", Role::StableRead, false).is_err());
            std::fs::write(path.join("stream:extra"), b"hidden").unwrap();
            assert!(open_file(&directory, "stream", Role::StableRead, false).is_err());
            std::os::windows::fs::symlink_file(path.join("stream"), path.join("reparse")).unwrap();
            assert!(open_file(&directory, "reparse", Role::StableRead, false).is_err());
            let file = std::fs::OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(7)
                .open(path.join("unprotected"))
                .unwrap();
            let mut allocation = ptr::null_mut();
            let mut dacl = ptr::null_mut();
            // SAFETY: disposable fixture-only handle; retain descriptor until
            // SetSecurityInfo completes, then release its SDK allocation.
            assert_eq!(
                unsafe {
                    GetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        &raw mut dacl,
                        ptr::null_mut(),
                        &raw mut allocation,
                    )
                },
                0
            );
            let result = unsafe {
                SetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    dacl,
                    ptr::null_mut(),
                )
            };
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(allocation);
            }
            assert_eq!(result, 0);
            drop(file);
            assert!(open_file(&directory, "unprotected", Role::StableRead, false).is_err());
            assert!(std::fs::rename(&path, temp.path().join("moved")).is_err());
            directory.validate_destination().unwrap();
        }

        #[test]
        #[ignore = "UNRUN: grants Everyone mutation rights only inside disposable fixture; protected-root reopen refuses"]
        fn native_windows_untrusted_private_root_refuses() {
            use windows_sys::Win32::Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
                    SetSecurityInfo,
                },
                DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
                PROTECTED_DACL_SECURITY_INFORMATION,
            };
            let (temp, directory) = fixture();
            let file = std::fs::OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(temp.path().join("native"))
                .unwrap();
            let text: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain(Some(0)).collect();
            let mut descriptor = ptr::null_mut();
            // SAFETY: terminated fixture-only SDDL, owned SDK allocation.
            assert_ne!(
                unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        text.as_ptr(),
                        1,
                        &raw mut descriptor,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let mut dacl = ptr::null_mut();
            let mut present = 0;
            let mut defaulted = 0;
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorDacl(
                        descriptor,
                        &raw mut present,
                        &raw mut dacl,
                        &raw mut defaulted,
                    )
                },
                0
            );
            let status = unsafe {
                SetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    dacl,
                    ptr::null_mut(),
                )
            };
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(descriptor);
            }
            assert_eq!(status, 0);
            drop(file);
            assert!(directory.validate_destination().is_err());
            assert!(
                directory
                    .create_child(OsStr::new("must-not-create"))
                    .is_err()
            );
            assert!(!temp.path().join("native/must-not-create").exists());
        }
        #[test]
        #[ignore = "UNRUN: real writable section and mapping lifetime/share behavior, no mock"]
        fn native_windows_writable_mapping_conflict() {
            use windows_sys::Win32::{
                Foundation::CloseHandle,
                System::Memory::{
                    CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
                    PAGE_READWRITE, UnmapViewOfFile,
                },
            };
            let (temp, directory) = fixture();
            let mut seed = open_file(&directory, "mapped", Role::Writer, true).unwrap();
            seed.write_all(&[0; 4096]).unwrap();
            flush_file(&seed).unwrap();
            drop(seed);
            let editor = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(7)
                .open(temp.path().join("native/mapped"))
                .unwrap();
            struct Mapping {
                section: HANDLE,
                view: MEMORY_MAPPED_VIEW_ADDRESS,
            }
            impl Drop for Mapping {
                fn drop(&mut self) {
                    unsafe {
                        UnmapViewOfFile(self.view);
                        CloseHandle(self.section);
                    }
                }
            }
            // SAFETY: fixture file, bounded page-sized mapping, no named object.
            let section = unsafe {
                CreateFileMappingW(
                    editor.as_raw_handle(),
                    ptr::null(),
                    PAGE_READWRITE,
                    0,
                    4096,
                    ptr::null(),
                )
            };
            assert!(!section.is_null());
            let view = unsafe { MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 4096) };
            let mapping = Mapping { section, view };
            assert!(!mapping.view.Value.is_null());
            assert!(open_file(&directory, "mapped", Role::Writer, false).is_err());
            drop(editor);
            // This is an intentionally strong qualification assertion: if this
            // stack loses share protection after last handle close while the
            // writable section lives, candidate support must remain refused.
            assert!(open_file(&directory, "mapped", Role::Writer, false).is_err());
            drop(mapping);
            assert!(open_file(&directory, "mapped", Role::Writer, false).is_ok());
        }
    }
}
#[cfg(windows)]
pub(super) use native::*;
