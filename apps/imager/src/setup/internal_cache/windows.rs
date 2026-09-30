//! Private Windows installer cache. Cooperative current-user boundary only.
//! FlushFileBuffers orders data, not guaranteed power-loss persistence of directory entries.
fn require(ok: bool, why: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why.into()) }
}
#[derive(Debug, PartialEq, Eq)]
struct WindowsDirectoryPath {
    root: [u16; 3],
    names: Vec<Vec<u16>>,
}

fn windows_directory_leaf_supported(name: &[u16]) -> bool {
    if name.is_empty()
        || name.len() > 255
        || matches!(name.last(), Some(0x20 | 0x2e))
        || name
            .iter()
            .any(|&unit| unit <= 31 || b"<>:\"/\\|?*~".iter().any(|&c| unit == u16::from(c)))
    {
        return false;
    }
    // Fold only ASCII for reserved-device comparison. Retain the original
    // code units, including unpaired surrogates, for every actual open.
    let mut stem: Vec<_> = name
        .iter()
        .copied()
        .take_while(|&unit| unit != u16::from(b'.'))
        .map(|unit| match unit {
            0x61..=0x7a => unit - 0x20,
            _ => unit,
        })
        .collect();
    while stem.last() == Some(&u16::from(b' ')) {
        stem.pop();
    }
    !["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.iter().copied().eq(reserved.encode_utf16()))
        && !(stem.len() == 4
            && ["COM", "LPT"]
                .iter()
                .any(|prefix| stem[..3].iter().copied().eq(prefix.encode_utf16()))
            // Include DOS digit aliases and Windows superscript ¹, ² and ³.
            && matches!(stem[3], 0x30..=0x39 | 0xb9 | 0xb2 | 0xb3))
}

fn windows_directory_path(
    raw: impl IntoIterator<Item = u16>,
) -> Result<WindowsDirectoryPath, String> {
    // Validate raw units before Path::components can normalize aliases. One
    // extra unit detects overflow without collecting an unbounded input.
    let raw: Vec<_> = raw.into_iter().take(4097).collect();
    require(
        (3..=4096).contains(&raw.len())
            && matches!(raw[0], 0x41..=0x5a | 0x61..=0x7a)
            && raw[1..3] == [u16::from(b':'), u16::from(b'\\')],
        "require a bounded ordinary absolute Windows drive path",
    )?;
    let mut names = Vec::new();
    if raw.len() > 3 {
        for name in raw[3..].split(|&unit| unit == u16::from(b'\\')) {
            require(
                names.len() < 64 && windows_directory_leaf_supported(name),
                "unsupported Windows directory component",
            )?;
            names.push(name.to_vec());
        }
    }
    Ok(WindowsDirectoryPath {
        root: [raw[0], raw[1], raw[2]],
        names,
    })
}

fn windows_cache_path(raw: impl IntoIterator<Item = u16>) -> Result<WindowsDirectoryPath, String> {
    let parsed = windows_directory_path(raw)?;
    // Reserve the private namespace, manifest digest and payload directory before
    // creating anything; the final verifier must accept this exact raw ancestry.
    let mut final_names = parsed.names.clone();
    final_names.extend(
        ["tilo-internal-install", &"0".repeat(64), "payload"]
            .map(|s| s.encode_utf16().collect::<Vec<_>>()),
    );
    let length = 3 + final_names
        .iter()
        .map(|n| n.len() + 1)
        .sum::<usize>()
        .saturating_sub(1);
    require(
        final_names.len() <= 64 && length <= 4096,
        "Cache suffix exceeds verifier ancestry bounds",
    )?;
    Ok(parsed)
}

// Masks are mapped from generic rights before this policy is evaluated.
fn allowed_ace(principal_trusted: bool, mask: u32, private: bool) -> bool {
    mask & !0x001f_01ff == 0
        && (principal_trusted
            || if private {
                mask == 0
            } else {
                mask & (2 | 16 | 64 | 256 | 0x10000 | 0x40000 | 0x80000) == 0
            })
}
#[cfg(test)]
#[path = "windows/tests.rs"]
mod tests;
#[cfg(windows)]
pub(in crate::setup) use native::*;
#[cfg(windows)]
mod native {
    use super::super::{
        MAX_COLLISIONS, MAX_RECORD, MAX_SCAN, Ownership, PreparationComplete, Provider,
        checkpoint::Record, working_leaf,
    };
    use super::{allowed_ace, require, windows_cache_path, windows_directory_leaf_supported};
    use crate::setup::internal_download::SelectedManifest;
    use sha2::{Digest, Sha256};
    use std::{
        ffi::{OsString, c_void},
        fs::File,
        io::{Read, Seek, SeekFrom, Write},
        mem::{offset_of, size_of},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::Path,
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
            Foundation::{
                CloseHandle, ERROR_LOCK_VIOLATION, ERROR_NO_MORE_FILES, HANDLE,
                INVALID_HANDLE_VALUE, LocalFree, RtlNtStatusToDosError, UNICODE_STRING,
            },
            Security::{Authorization::*, *},
            Storage::FileSystem::*,
            System::{
                IO::{IO_STATUS_BLOCK, OVERLAPPED},
                Threading::{GetCurrentProcess, OpenProcessToken},
            },
        },
    };
    type Result<T> = std::result::Result<T, String>;
    fn io(e: std::io::Error) -> String {
        format!("Windows private cache: {e}")
    }
    fn last() -> String {
        io(std::io::Error::last_os_error())
    }
    fn stop(cancel: &AtomicBool) -> Result<()> {
        require(!cancel.load(Ordering::Acquire), "Cache operation cancelled")
    }
    fn wide(bytes: &[u8]) -> Vec<u16> {
        bytes.iter().map(|b| u16::from(*b)).collect()
    }
    struct LocalAllocation(*mut c_void);
    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    // All SID callers first establish a bounded SID inside a live native allocation.
    unsafe fn sid_text(sid: PSID) -> Result<String> {
        require(
            !sid.is_null() && unsafe { IsValidSid(sid) } != 0,
            "Invalid SID",
        )?;
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(last());
        }
        let _owner = LocalAllocation(text.cast());
        let mut n = 0;
        while n < 256 && unsafe { *text.add(n) } != 0 {
            n += 1;
        }
        require(n < 256, "SID text exceeds bound")?;
        String::from_utf16(unsafe { std::slice::from_raw_parts(text, n) })
            .map_err(|e| e.to_string())
    }
    fn current_user() -> Result<String> {
        let mut raw = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(last());
        }
        let token = Token(raw);
        let mut bytes = 0;
        unsafe {
            GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut bytes);
        }
        require(
            (size_of::<TOKEN_USER>()..=65536).contains(&(bytes as usize)),
            "Token size refusal",
        )?;
        let capacity = bytes;
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                capacity,
                &mut bytes,
            )
        } == 0
        {
            return Err(last());
        }
        require(bytes <= capacity, "Token grew")?;
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let start = buffer.as_ptr() as usize;
        let end = start + bytes as usize;
        bounded_sid(sid, start, end)?;
        unsafe { sid_text(sid) }
    }
    fn bounded_sid(sid: PSID, start: usize, end: usize) -> Result<()> {
        let p = sid as usize;
        require(
            p >= start && p.checked_add(8).is_some_and(|n| n <= end),
            "SID outside buffer",
        )?;
        let count = unsafe { *sid.cast::<u8>().add(1) } as usize;
        require(
            count <= 15 && p.checked_add(8 + 4 * count).is_some_and(|n| n <= end),
            "SID size refusal",
        )?;
        require(unsafe { IsValidSid(sid) } != 0, "Invalid bounded SID")
    }
    fn trusted(sid: &str, user: &str, private: bool) -> bool {
        sid == user
            || matches!(sid, "S-1-5-18" | "S-1-5-32-544")
            || (!private && sid == "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464")
    }
    fn descriptor(user: &str) -> Result<LocalAllocation> {
        let text: Vec<u16> =
            format!("O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\0")
                .encode_utf16()
                .collect();
        let mut output = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut output,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last());
        }
        Ok(LocalAllocation(output))
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Security([u8; 32]);
    fn security(file: &File, directory: bool, private: bool) -> Result<Security> {
        snapshot(file, directory)?;
        let mut sd = ptr::null_mut();
        let mut owner = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let rc = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        let _allocation = LocalAllocation(sd);
        if rc != 0 {
            return Err(io(std::io::Error::from_raw_os_error(rc as i32)));
        }
        require(
            !sd.is_null() && !owner.is_null() && !dacl.is_null(),
            "Null owner/DACL",
        )?;
        let length = unsafe { GetSecurityDescriptorLength(sd) } as usize;
        require((20..=65536).contains(&length), "Security descriptor bound")?;
        let start = sd as usize;
        let end = start.checked_add(length).ok_or("Descriptor overflow")?;
        bounded_sid(owner, start, end)?;
        let user = current_user()?;
        let owner_text = unsafe { sid_text(owner)? };
        require(
            trusted(&owner_text, &user, private) && (!private || owner_text == user),
            "Cache owner refusal",
        )?;
        let dp = dacl as usize;
        require(
            dp >= start && dp.checked_add(size_of::<ACL>()).is_some_and(|n| n <= end),
            "ACL outside descriptor",
        )?;
        let acl_size = unsafe { (*dacl).AclSize } as usize;
        require(
            acl_size >= size_of::<ACL>() && dp.checked_add(acl_size).is_some_and(|n| n <= end),
            "ACL size refusal",
        )?;
        require(
            unsafe { IsValidAcl(dacl) } != 0 && unsafe { (*dacl).AceCount } <= 128,
            "Invalid or excessive ACL",
        )?;
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
            return Err(last());
        }
        require(
            !private || control & SE_DACL_PROTECTED != 0,
            "Private DACL not protected",
        )?;
        let mut hash = Sha256::new();
        hash.update(owner_text.as_bytes());
        hash.update(control.to_le_bytes());
        for index in 0..unsafe { (*dacl).AceCount } {
            let mut ace: *mut c_void = ptr::null_mut();
            if unsafe { GetAce(dacl, index as u32, &mut ace) } == 0 {
                return Err(last());
            }
            let ap = ace as usize;
            require(
                ap >= dp && ap.checked_add(8).is_some_and(|n| n <= dp + acl_size),
                "ACE outside ACL",
            )?;
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            let size = header.AceSize as usize;
            require(
                size >= 16
                    && ap.checked_add(size).is_some_and(|n| n <= dp + acl_size)
                    && matches!(header.AceType, 0 | 1)
                    && header.AceFlags & !0x1f == 0,
                "Unknown/malformed ACE",
            )?;
            let sid = unsafe { ace.cast::<u8>().add(8).cast() };
            bounded_sid(sid, ap + 8, ap + size)?;
            let principal = unsafe { sid_text(sid)? };
            let mut mask = unsafe { (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask };
            let mapping = GENERIC_MAPPING {
                GenericRead: FILE_GENERIC_READ,
                GenericWrite: FILE_GENERIC_WRITE,
                GenericExecute: FILE_GENERIC_EXECUTE,
                GenericAll: FILE_ALL_ACCESS,
            };
            unsafe {
                MapGenericMask(&mut mask, &mapping);
            }
            require(mask & !0x001f_01ff == 0, "Unknown ACL access bits")?;
            // Denies cannot add authority. Private DACLs still contain only the three trusted principals.
            require(
                !private || trusted(&principal, &user, true),
                "Private ACL contains foreign principal",
            )?;
            if header.AceType == 0
                && (private || u32::from(header.AceFlags) & INHERIT_ONLY_ACE == 0)
            {
                require(
                    allowed_ace(trusted(&principal, &user, private), mask, private),
                    "Unsafe cache ACL rights",
                )?;
            }
            hash.update(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) });
        }
        Ok(Security(hash.finalize().into()))
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Identity {
        volume: u64,
        id: [u8; 16],
    }
    impl Identity {
        fn of(file: &File) -> Result<Self> {
            let mut id = FILE_ID_INFO::default();
            query(file, FileIdInfo, &mut id)?;
            require(id.FileId.Identifier != [0; 16], "Zero file identity")?;
            Ok(Self {
                volume: id.VolumeSerialNumber,
                id: id.FileId.Identifier,
            })
        }
        fn digest(self) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(b"tilo-windows-volume-fileid-v1");
            h.update(self.volume.to_le_bytes());
            h.update(self.id);
            h.finalize().into()
        }
    }
    fn query<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS, value: &mut T) -> Result<()> {
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                class,
                ptr::from_mut(value).cast(),
                size_of::<T>() as u32,
            )
        } == 0
        {
            return Err(last());
        }
        Ok(())
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Fingerprint {
        id: Identity,
        length: u64,
        created: i64,
        written: i64,
        changed: i64,
        attributes: u32,
    }
    fn snapshot(file: &File, directory: bool) -> Result<Fingerprint> {
        require(
            unsafe { GetFileType(file.as_raw_handle()) } == FILE_TYPE_DISK,
            "Non-disk cache object",
        )?;
        let mut basic = FILE_BASIC_INFO::default();
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        let mut standard = FILE_STANDARD_INFO::default();
        query(file, FileBasicInfo, &mut basic)?;
        query(file, FileAttributeTagInfo, &mut tag)?;
        query(file, FileStandardInfo, &mut standard)?;
        require(
            tag.FileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE) == 0
                && tag.ReparseTag == 0
                && basic.FileAttributes == tag.FileAttributes
                && standard.Directory == directory
                && !standard.DeletePending
                && (directory || standard.NumberOfLinks == 1)
                && standard.EndOfFile >= 0,
            "Cache type/link/reparse/delete/size refusal",
        )?;
        Ok(Fingerprint {
            id: Identity::of(file)?,
            length: standard.EndOfFile as u64,
            created: basic.CreationTime,
            written: basic.LastWriteTime,
            changed: basic.ChangeTime,
            attributes: basic.FileAttributes,
        })
    }
    fn fingerprint(file: &File) -> Result<Fingerprint> {
        snapshot(file, false)
    }
    #[derive(Clone, Copy)]
    enum Role {
        Directory,
        Lock,
        Writer,
        Readonly,
        Observe,
        Named,
        Delete,
    }
    impl Role {
        fn access(self) -> u32 {
            match self {
                Self::Directory => {
                    FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE
                }
                Self::Lock => FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                Self::Writer => FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                Self::Readonly | Self::Observe => FILE_GENERIC_READ,
                Self::Named => FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
                Self::Delete => DELETE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            }
        }
        fn share(self) -> u32 {
            match self {
                Self::Directory | Self::Lock => FILE_SHARE_READ | FILE_SHARE_WRITE,
                Self::Observe | Self::Named | Self::Delete => {
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
                }
                _ => FILE_SHARE_READ,
            }
        }
    }
    fn relative(parent: &File, leaf: &[u16], role: Role, create: bool) -> std::io::Result<File> {
        if !windows_directory_leaf_supported(leaf) {
            return Err(std::io::Error::other("Unsafe cache name"));
        }
        let mut units = leaf.to_vec();
        let length = (units.len() * 2) as u16;
        let mut unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: units.as_mut_ptr(),
        };
        let security = if create {
            Some(
                descriptor(&current_user().map_err(std::io::Error::other)?)
                    .map_err(std::io::Error::other)?,
            )
        } else {
            None
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &mut unicode,
            Attributes: 0,
            SecurityDescriptor: security.as_ref().map_or(ptr::null_mut(), |s| s.0.cast()),
            SecurityQualityOfService: ptr::null_mut(),
        };
        let mut raw = ptr::null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        let rc = unsafe {
            NtCreateFile(
                &mut raw,
                role.access(),
                &attributes,
                &mut status,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                role.share(),
                if create { FILE_CREATE } else { FILE_OPEN },
                FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | if matches!(role, Role::Directory) {
                        FILE_DIRECTORY_FILE
                    } else if matches!(role, Role::Named) {
                        0
                    } else {
                        FILE_NON_DIRECTORY_FILE
                    },
                ptr::null(),
                0,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(rc) } as i32,
            ));
        }
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::other("Invalid native handle"));
        }
        Ok(unsafe { File::from_raw_handle(raw) })
    }
    fn open(parent: &File, leaf: &[u8], role: Role, create: bool) -> std::io::Result<File> {
        relative(parent, &wide(leaf), role, create)
    }
    fn native_name(file: &File) -> Result<Vec<u16>> {
        let mut name = vec![0u16; 8192];
        let n = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                name.as_mut_ptr(),
                name.len() as u32,
                VOLUME_NAME_GUID | FILE_NAME_NORMALIZED,
            )
        } as usize;
        require(
            n > 0 && n < name.len(),
            "Native name unavailable or excessive",
        )?;
        name.truncate(n);
        Ok(name)
    }
    struct Held {
        file: File,
        id: Identity,
        security: Security,
        leaf: Vec<u8>,
        native_leaf: Vec<u16>,
        name: Vec<u16>,
        directory: bool,
        private: bool,
    }
    impl Held {
        fn new(file: File, leaf: &[u8], directory: bool, private: bool) -> Result<Self> {
            require(leaf.is_ascii(), "Non-ASCII cache control leaf")?;
            let mut held = Self::new_wide(file, &wide(leaf), directory, private)?;
            held.leaf = leaf.to_vec();
            Ok(held)
        }
        fn new_wide(file: File, leaf: &[u16], directory: bool, private: bool) -> Result<Self> {
            let id = Identity::of(&file)?;
            let security = security(&file, directory, private)?;
            let name = native_name(&file)?;
            Ok(Self {
                file,
                id,
                security,
                // Only new() fills the separate ASCII control-file label.
                // Raw ancestry uses native_leaf exclusively; never narrow UTF-16.
                leaf: Vec::new(),
                native_leaf: leaf.to_vec(),
                name,
                directory,
                private,
            })
        }
        fn check(&self, parent: Option<&File>) -> Result<()> {
            require(
                Identity::of(&self.file)? == self.id
                    && security(&self.file, self.directory, self.private)? == self.security
                    && native_name(&self.file)? == self.name,
                "Held cache identity/security/name changed",
            )?;
            if let Some(parent) = parent {
                let named = relative(parent, &self.native_leaf, Role::Named, false).map_err(io)?;
                require(
                    Identity::of(&named)? == self.id
                        && security(&named, self.directory, self.private)? == self.security
                        && native_name(&named)? == self.name,
                    "Named cache identity/security changed",
                )?;
            }
            Ok(())
        }
    }
    fn directory_wide(parent: &File, leaf: &[u16], create: bool, private: bool) -> Result<Held> {
        let file = match relative(parent, leaf, Role::Directory, false) {
            Ok(f) => f,
            Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
                relative(parent, leaf, Role::Directory, true).map_err(io)?
            }
            Err(e) => return Err(io(e)),
        };
        let held = Held::new_wide(file, leaf, true, private)?;
        require(
            held.id.volume == Identity::of(parent)?.volume,
            "Cache volume changed",
        )?;
        held.check(Some(parent))?;
        Ok(held)
    }
    fn directory(parent: &File, leaf: &[u8], create: bool, private: bool) -> Result<Held> {
        directory_wide(parent, &wide(leaf), create, private)
    }
    fn entries(parent: &File) -> Result<Vec<Vec<u8>>> {
        // Transient listing access is closed before held-handle rename uses its parent.
        let raw = unsafe {
            ReOpenFile(
                parent.as_raw_handle(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(last());
        }
        let listing = unsafe { File::from_raw_handle(raw) };
        require(
            Identity::of(&listing)? == Identity::of(parent)?,
            "Listing parent changed",
        )?;
        let mut out = Vec::new();
        let mut restart = true;
        loop {
            let mut buffer = vec![0u64; 8192 / 8];
            let rc = unsafe {
                GetFileInformationByHandleEx(
                    listing.as_raw_handle(),
                    if restart {
                        FileIdBothDirectoryRestartInfo
                    } else {
                        FileIdBothDirectoryInfo
                    },
                    buffer.as_mut_ptr().cast(),
                    8192,
                )
            };
            if rc == 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(io(e));
            }
            restart = false;
            let mut offset = 0usize;
            loop {
                let name_offset = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
                require(
                    offset + size_of::<FILE_ID_BOTH_DIR_INFO>() <= 8192,
                    "Directory entry header bound",
                )?;
                let entry = unsafe {
                    &*buffer
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<FILE_ID_BOTH_DIR_INFO>()
                };
                let n = entry.FileNameLength as usize;
                require(
                    n > 0 && n <= 510 && n % 2 == 0 && offset + name_offset + n <= 8192,
                    "Directory entry name bound",
                )?;
                let units = unsafe {
                    std::slice::from_raw_parts(ptr::addr_of!(entry.FileName).cast::<u16>(), n / 2)
                };
                if units != [46] && units != [46, 46] {
                    require(
                        out.len() < MAX_SCAN
                            && windows_directory_leaf_supported(units)
                            && units.iter().all(|u| *u <= 127),
                        "Unknown or excessive cache entries",
                    )?;
                    out.push(units.iter().map(|u| *u as u8).collect());
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                let step = entry.NextEntryOffset as usize;
                require(
                    step >= name_offset + n && step % 8 == 0 && offset + step < 8192,
                    "Directory offset bound",
                )?;
                offset += step;
            }
        }
        out.sort();
        require(
            !out.windows(2).any(|p| p[0] == p[1]),
            "Duplicate directory entry",
        )?;
        Ok(out)
    }
    fn drive(root: &[u16; 3]) -> Result<File> {
        let path = OsString::from_wide(root);
        let file = std::fs::OpenOptions::new()
            .access_mode(Role::Directory.access())
            .share_mode(Role::Directory.share())
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(io)?;
        snapshot(&file, true)?;
        let name = native_name(&file)?;
        let text = String::from_utf16(&name).map_err(|_| "Invalid volume root")?;
        let guid = text
            .strip_prefix(r"\\?\Volume{")
            .and_then(|s| s.strip_suffix(r"}\"))
            .ok_or("Drive is not volume root")?;
        require(
            guid.len() == 36
                && guid.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                }),
            "Invalid volume GUID",
        )?;
        let mut terminated = name;
        terminated.push(0);
        require(
            matches!(unsafe { GetDriveTypeW(terminated.as_ptr()) }, 2 | 3),
            "Nonlocal cache volume",
        )?;
        let mut flags = 0;
        let mut fs = [0u16; 32];
        if unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                fs.as_mut_ptr(),
                32,
            )
        } == 0
        {
            return Err(last());
        }
        let n = fs
            .iter()
            .position(|u| *u == 0)
            .ok_or("Filesystem name bound")?;
        require(
            flags & 8 != 0 && (fs[..n] == wide(b"NTFS") || fs[..n] == wide(b"ReFS")),
            "Unsupported volume/ACL semantics",
        )?;
        Ok(file)
    }
    struct LockedRoot {
        drive_root: [u16; 3],
        ancestry: Vec<Held>,
        lock: Held,
        payload: Held,
        journals: [Held; 2],
    }
    impl LockedRoot {
        fn root(&self) -> &File {
            &self.ancestry.last().unwrap().file
        }
        fn check(&self) -> Result<()> {
            let named_root = drive(&self.drive_root)?;
            require(
                Identity::of(&named_root)? == self.ancestry[0].id
                    && native_name(&named_root)? == self.ancestry[0].name,
                "Drive-root mapping changed",
            )?;
            for (i, held) in self.ancestry.iter().enumerate() {
                held.check(i.checked_sub(1).map(|n| &self.ancestry[n].file))?;
            }
            self.lock.check(Some(self.root()))?;
            self.payload.check(Some(self.root()))?;
            for journal in &self.journals {
                journal.check(Some(self.root()))?;
            }
            require(
                entries(self.root())?
                    == vec![
                        b"journal-0".to_vec(),
                        b"journal-1".to_vec(),
                        b"ownership.lock".to_vec(),
                        b"payload".to_vec(),
                    ],
                "Unknown manifest-cache residue",
            )
        }
    }
    /// The per-user cache directory that holds `tilo-internal-install`.
    pub(in crate::setup) fn cache_base() -> Result<std::path::PathBuf> {
        tilo_paths::cache_dir().ok_or_else(|| "Current-user LocalAppData cache unavailable".into())
    }
    pub(in crate::setup) struct CacheRoot {
        lease: Arc<LockedRoot>,
        manifest: String,
    }
    impl CacheRoot {
        pub(in crate::setup) fn acquire(selected: &SelectedManifest) -> Result<Self> {
            Self::acquire_at(&cache_base()?, selected)
        }
        fn acquire_at(cache: &Path, selected: &SelectedManifest) -> Result<Self> {
            let parsed = windows_cache_path(cache.as_os_str().encode_wide())?;
            let root_file = drive(&parsed.root)?;
            let mut ancestry = vec![Held::new(root_file, b"", true, false)?];
            for part in parsed.names {
                ancestry.push(directory_wide(
                    &ancestry.last().unwrap().file,
                    &part,
                    false,
                    false,
                )?);
            }
            for leaf in [
                b"tilo-internal-install".as_slice(),
                selected.manifest_sha256().as_bytes(),
            ] {
                ancestry.push(directory(&ancestry.last().unwrap().file, leaf, true, true)?);
            }
            let root = &ancestry.last().unwrap().file;
            let known = [
                b"ownership.lock".as_slice(),
                b"payload",
                b"journal-0",
                b"journal-1",
            ];
            require(
                entries(root)?.iter().all(|n| known.contains(&n.as_slice())),
                "Unknown cache-root residue",
            )?;
            let lock_file = match open(root, b"ownership.lock", Role::Lock, true) {
                Ok(f) => {
                    f.sync_all().map_err(io)?;
                    f
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    open(root, b"ownership.lock", Role::Lock, false).map_err(io)?
                }
                Err(e) => return Err(io(e)),
            };
            let lock = Held::new(lock_file, b"ownership.lock", false, true)?;
            require(
                lock.file.metadata().map_err(io)?.len() == 0,
                "Unknown lock contents",
            )?;
            let mut overlapped = OVERLAPPED::default();
            if unsafe {
                LockFileEx(
                    lock.file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            } == 0
            {
                let e = std::io::Error::last_os_error();
                return Err(if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                    "Cache Busy".into()
                } else {
                    io(e)
                });
            }
            // Closing the permanent lock handle releases the kernel byte lock.
            lock.check(Some(root))?;
            let payload = directory(root, b"payload", true, true)?;
            let journals = [
                directory(root, b"journal-0", true, true)?,
                directory(root, b"journal-1", true, true)?,
            ];
            let lease = Arc::new(LockedRoot {
                drive_root: parsed.root,
                ancestry,
                lock,
                payload,
                journals,
            });
            lease.check()?;
            Ok(Self {
                lease,
                manifest: selected.manifest_sha256().into(),
            })
        }
    }

    struct Journal {
        snapshot: Fingerprint,
        held: Held,
        bytes: Vec<u8>,
        generation: u64,
    }
    fn journals(
        root: &LockedRoot,
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Vec<Journal>> {
        journals_visit(root, selected, index, &mut |_| Ok(()))
    }
    fn journals_visit(
        root: &LockedRoot,
        selected: &SelectedManifest,
        index: usize,
        visit: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Vec<Journal>> {
        root.check()?;
        let parent = &root.journals[index].file;
        let names = entries(parent)?;
        require(names.len() <= 3, "Excessive journal survivors")?;
        let mut found = Vec::new();
        for leaf in names {
            let file = open(parent, &leaf, Role::Observe, false).map_err(io)?;
            let mut held = Held::new(file, &leaf, false, true)?;
            held.check(Some(parent))?;
            let before = fingerprint(&held.file)?;
            let len = held.file.metadata().map_err(io)?.len();
            require(
                len > 0 && len <= MAX_RECORD as u64,
                "Unknown/torn/oversized journal",
            )?;
            let mut bytes = vec![0; len as usize];
            held.file.read_exact(&mut bytes).map_err(io)?;
            let mut end = [0];
            require(held.file.read(&mut end).map_err(io)? == 0, "Journal grew")?;
            let record = Record::decode(&bytes)?;
            let hint = record.hint_for(selected, index)?;
            require(
                leaf == format!("record-{:010}", hint.generation).as_bytes(),
                "Unknown journal leaf/generation",
            )?;
            held.check(Some(parent))?;
            require(
                fingerprint(&held.file)? == before,
                "Journal changed while reading",
            )?;
            visit(&bytes)?;
            held.check(Some(parent))?;
            found.push(Journal {
                snapshot: before,
                held,
                bytes,
                generation: hint.generation,
            });
        }
        root.check()?;
        Ok(found)
    }
    fn same_scan(a: &[Journal], b: &[Journal]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(a, b)| {
                a.snapshot == b.snapshot
                    && a.held.id == b.held.id
                    && a.held.security == b.held.security
                    && a.held.leaf == b.held.leaf
                    && a.bytes == b.bytes
            })
    }
    fn candidate(
        records: &[Journal],
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Option<(String, [u8; 32], [u8; 32])>> {
        let mut result = None;
        for journal in records {
            let record = Record::decode(&journal.bytes)?;
            let h = record.hint_for(selected, index)?;
            let item = (h.leaf.to_owned(), *h.parent_digest, *h.file_digest);
            require(
                result.as_ref().is_none_or(|prior| prior == &item),
                "Conflicting journal candidates",
            )?;
            result = Some(item);
        }
        Ok(result)
    }
    pub(in crate::setup) struct WindowsProvider<'a> {
        lease: Arc<LockedRoot>,
        selected: &'a SelectedManifest,
        index: usize,
        payload: Held,
        own: Ownership,
        records: Vec<Journal>,
        scanned: bool,
        prepared: bool,
        uncertain: bool,
        // One-shot admission minted only in the successful exclusive-create branch.
        fresh: Option<(Identity, Identity)>,
    }
    pub(in crate::setup) struct Partial<'a> {
        pub(in crate::setup) provider: WindowsProvider<'a>,
        pub(in crate::setup) file: File,
    }
    pub(in crate::setup) struct FinalHandle<'a> {
        lease: Arc<LockedRoot>,
        held: Held,
        selected: &'a SelectedManifest,
        index: usize,
        residue: bool,
    }
    pub(in crate::setup) enum Component<'a> {
        Partial(Partial<'a>),
        Final(FinalHandle<'a>),
    }
    fn verify_bytes(
        file: &mut File,
        selected: &SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        stop(cancel)?;
        let c = selected
            .bundle()
            .component(index)
            .ok_or("Invalid cache component")?;
        let before = fingerprint(&file)?;
        require(
            before.length == c.byte_size,
            "Final component length mismatch",
        )?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        let mut hash = Sha256::new();
        let mut left = c.byte_size;
        let mut buffer = [0; 64 * 1024];
        while left > 0 {
            stop(cancel)?;
            let n = file
                .read(&mut buffer[..left.min(65536) as usize])
                .map_err(io)?;
            require(n > 0, "Truncated final component")?;
            hash.update(&buffer[..n]);
            left -= n as u64;
        }
        stop(cancel)?;
        let mut eof = [0];
        require(
            file.read(&mut eof).map_err(io)? == 0 && hex::encode(hash.finalize()) == c.sha256,
            "Final component digest/EOF mismatch",
        )?;
        require(
            fingerprint(&file)? == before,
            "Final component changed during authentication",
        )?;
        stop(cancel)
    }
    fn final_handle<'a>(
        lease: Arc<LockedRoot>,
        selected: &'a SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<FinalHandle<'a>>> {
        lease.check()?;
        let leaf = selected
            .bundle()
            .component(index)
            .ok_or("Invalid component")?
            .filename;
        let file = match open(&lease.payload.file, leaf.as_bytes(), Role::Readonly, false) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        let mut held = Held::new(file, leaf.as_bytes(), false, true)?;
        held.check(Some(&lease.payload.file))?;
        verify_bytes(&mut held.file, selected, index, cancel)?;
        held.check(Some(&lease.payload.file))?;
        namespace_boundary(&lease.payload.file)?;
        held.check(Some(&lease.payload.file))?;
        lease.check()?;
        stop(cancel)?;
        Ok(Some(FinalHandle {
            lease,
            held,
            selected,
            index,
            residue: true,
        }))
    }
    fn generated() -> Result<String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ticks = ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let mut h = Sha256::new();
        h.update(ticks.to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let digest = h.finalize();
        // Uniqueness only. Exclusive FILE_CREATE supplies the no-adoption boundary.
        Ok(working_leaf(u128::from_le_bytes(
            digest[..16].try_into().unwrap(),
        )))
    }
    impl CacheRoot {
        pub(in crate::setup) fn bootstrap<'a>(
            self,
            selected: &'a SelectedManifest,
            cancel: &AtomicBool,
        ) -> Result<[Component<'a>; 2]> {
            stop(cancel)?;
            require(
                self.manifest == selected.manifest_sha256(),
                "Cache selection changed",
            )?;
            self.lease.check()?;
            // Finals authenticate first, independently of stale partial records.
            let mut finals = [
                final_handle(self.lease.clone(), selected, 0, cancel)?,
                final_handle(self.lease.clone(), selected, 1, cancel)?,
            ];
            require(
                finals[0]
                    .as_ref()
                    .zip(finals[1].as_ref())
                    .is_none_or(|(a, b)| a.held.id != b.held.id),
                "Final component alias",
            )?;
            let mut scans = [
                journals(&self.lease, selected, 0)?,
                journals(&self.lease, selected, 1)?,
            ];
            let candidates = [
                candidate(&scans[0], selected, 0)?,
                candidate(&scans[1], selected, 1)?,
            ];
            require(
                candidates[0]
                    .as_ref()
                    .zip(candidates[1].as_ref())
                    .is_none_or(|(a, b)| a.0 != b.0 && a.2 != b.2),
                "Component partial alias",
            )?;
            let names = entries(&self.lease.payload.file)?;
            for leaf in &names {
                let final_known = (0..2).any(|i| {
                    finals[i].is_some()
                        && selected.bundle().component(i).unwrap().filename.as_bytes() == leaf
                });
                let partial_known = candidates.iter().flatten().any(|c| c.0.as_bytes() == leaf);
                require(
                    final_known || partial_known,
                    "Unknown payload residue; no adoption or cleanup",
                )?;
            }
            let mut result = Vec::with_capacity(2);
            for index in 0..2 {
                stop(cancel)?;
                self.lease.check()?;
                if let Some(mut final_file) = finals[index].take() {
                    final_file.residue = !scans[index].is_empty();
                    result.push(Component::Final(final_file));
                    continue;
                }
                let parent = &self.lease.payload.file;
                let mut fresh_creation = None;
                let held = if let Some((leaf, parent_id, file_id)) = &candidates[index] {
                    let file = open(parent, leaf.as_bytes(), Role::Writer, false).map_err(io)?;
                    let held = Held::new(file, leaf.as_bytes(), false, true)?;
                    held.check(Some(parent))?;
                    require(
                        self.lease.payload.id.digest() == *parent_id
                            && held.id.digest() == *file_id,
                        "Journal candidate native identity mismatch",
                    )?;
                    held
                } else {
                    let mut created = None;
                    for _ in 0..MAX_COLLISIONS {
                        let leaf = generated()?;
                        stop(cancel)?;
                        match open(parent, leaf.as_bytes(), Role::Writer, true) {
                            Ok(file) => {
                                let held = Held::new(file, leaf.as_bytes(), false, true)?;
                                held.check(Some(parent))?;
                                require(
                                    held.file.metadata().map_err(io)?.len() == 0,
                                    "Created partial not empty",
                                )?;
                                held.file.sync_all().map_err(io)?;
                                namespace_boundary(parent)?;
                                #[cfg(test)]
                                super::tests::boundary("partial-flushed")?;
                                fresh_creation = Some((self.lease.payload.id, held.id));
                                created = Some(held);
                                break;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                            Err(e) => return Err(io(e)),
                        }
                    }
                    created.ok_or("Cache exclusive-create collision limit")?
                };
                require(
                    held.file.metadata().map_err(io)?.len()
                        <= selected.bundle().component(index).unwrap().byte_size,
                    "Partial exceeds selected length",
                )?;
                let own = Ownership {
                    parent: self.lease.payload.id.digest(),
                    file: held.id.digest(),
                    leaf: String::from_utf8(held.leaf.clone())
                        .map_err(|_| "Invalid partial leaf")?,
                };
                let mut file = held.file.try_clone().map_err(io)?;
                let mut provider = WindowsProvider {
                    lease: self.lease.clone(),
                    selected,
                    index,
                    payload: held,
                    own,
                    records: std::mem::take(&mut scans[index]),
                    scanned: false,
                    prepared: false,
                    uncertain: false,
                    fresh: fresh_creation,
                };
                if fresh_creation.is_some() {
                    super::super::initialize_fresh(selected, index, &mut provider, &mut file)?;
                    #[cfg(test)]
                    super::tests::boundary("fresh-settled")?;
                }
                stop(cancel)?;
                result.push(Component::Partial(Partial { provider, file }));
            }
            self.lease.check()?;
            result
                .try_into()
                .map_err(|_| "Internal component count".into())
        }
    }
    impl super::super::FreshProvider for WindowsProvider<'_> {
        fn take_fresh(&mut self, selected: &SelectedManifest, index: usize) -> Result<Ownership> {
            // Consume before any validation, including failing binding/native checks.
            let fresh = self.fresh.take();
            self.revoke_preparation();
            require(
                fresh == Some((self.lease.payload.id, self.payload.id))
                    && !self.uncertain
                    && index == self.index
                    && selected.manifest_sha256() == self.selected.manifest_sha256(),
                "No exclusive fresh creation admission",
            )?;
            self.lease.check()?;
            self.payload.check(Some(&self.lease.payload.file))?;
            require(
                fingerprint(&self.payload.file)?.length == 0 && self.records.is_empty(),
                "Fresh owner is not empty",
            )?;
            Ok(self.own.clone())
        }
        fn initialization_failed(&mut self) {
            self.uncertain = true;
            self.revoke_preparation();
        }
    }
    impl Provider for WindowsProvider<'_> {
        fn revoke_preparation(&mut self) {
            self.prepared = false;
            self.scanned = false;
            // Uncertain publication is deliberately not cleared by revocation.
        }
        fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<()> {
            self.prepared = false;
            require(
                self.scanned
                    && !self.uncertain
                    && proof.authorizes(self.selected, self.index, &self.own)?,
                "No successful cache preparation receipt",
            )?;
            self.lease.check()?;
            self.payload.check(Some(&self.lease.payload.file))?;
            require(
                fingerprint(&self.payload.file)?.length
                    <= self
                        .selected
                        .bundle()
                        .component(self.index)
                        .unwrap()
                        .byte_size,
                "Prepared payload grew",
            )?;
            self.prepared = true;
            Ok(())
        }
        fn ownership(&self) -> Ownership {
            self.own.clone()
        }
        fn verify(&mut self, file: &File, ownership: &Ownership) -> Result<()> {
            require(
                !self.uncertain && ownership == &self.own,
                "Cache ownership mismatch or uncertain operation",
            )?;
            self.lease.check()?;
            self.payload.check(Some(&self.lease.payload.file))?;
            require(
                Identity::of(file)? == self.payload.id
                    && security(file, false, true)? == self.payload.security
                    && file.metadata().map_err(io)?.len()
                        <= self
                            .selected
                            .bundle()
                            .component(self.index)
                            .unwrap()
                            .byte_size,
                "Borrowed payload changed",
            )
        }
        fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
            self.revoke_preparation();
            require(!self.uncertain, "Uncertain cache operation")?;
            let mut count = 0;
            let fresh = journals_visit(&self.lease, self.selected, self.index, &mut |bytes| {
                require(
                    self.records.get(count).is_some_and(|r| r.bytes == bytes),
                    "Bootstrap journal bytes changed",
                )?;
                count += 1;
                visit(bytes)
            })?;
            require(
                same_scan(&fresh, &self.records),
                "Bootstrap journal substitution",
            )?;
            let parent = &self.lease.journals[self.index].file;
            require(
                entries(parent)?
                    == fresh
                        .iter()
                        .map(|r| r.held.leaf.clone())
                        .collect::<Vec<_>>(),
                "Journal namespace changed across visitor",
            )?;
            for journal in &fresh {
                journal.held.check(Some(parent))?;
                require(
                    fingerprint(&journal.held.file)? == journal.snapshot,
                    "Journal bytes changed across visitor",
                )?;
            }
            self.records = fresh;
            self.scanned = true;
            Ok(())
        }
        fn retain(&mut self, keep: &[u64]) -> Result<()> {
            require(
                self.scanned && !self.uncertain && keep.len() <= 2,
                "No validated journal retirement authority",
            )?;
            let fresh = journals(&self.lease, self.selected, self.index)?;
            require(
                same_scan(&fresh, &self.records),
                "Journal changed before retirement",
            )?;
            self.uncertain = true;
            let parent = &self.lease.journals[self.index].file;
            drop(fresh);
            let mut retained = Vec::new();
            for journal in std::mem::take(&mut self.records) {
                if keep.contains(&journal.generation) {
                    retained.push(journal);
                } else {
                    dispose(parent, &journal.held)?;
                }
            }
            self.records = retained;
            namespace_boundary(parent)?;
            require(
                entries(parent)?
                    == self
                        .records
                        .iter()
                        .map(|r| r.held.leaf.clone())
                        .collect::<Vec<_>>(),
                "Journal retirement left residue",
            )?;
            self.uncertain = false;
            Ok(())
        }
        fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<()> {
            let mut created = false;
            (|| {
                require(
                    self.scanned
                        && !self.uncertain
                        && bytes.len() <= MAX_RECORD
                        && self.records.len() < 3
                        && keep.len() <= 2
                        && keep.contains(&generation),
                    "Invalid journal publication",
                )?;
                self.lease.check()?;
                self.payload.check(Some(&self.lease.payload.file))?;
                let record = Record::decode(bytes)?;
                let hint = record.hint_for(self.selected, self.index)?;
                require(
                    hint.generation == generation
                        && hint.leaf == self.own.leaf
                        && *hint.parent_digest == self.own.parent
                        && *hint.file_digest == self.own.file,
                    "Journal publication binding mismatch",
                )?;
                require(
                    same_scan(
                        &journals(&self.lease, self.selected, self.index)?,
                        &self.records,
                    ),
                    "Journal changed before publication",
                )?;
                let leaf = format!("record-{generation:010}");
                let parent = &self.lease.journals[self.index].file;
                // Exclusive record leaf is the publication. Any later failure is
                // uncertain; torn/unknown records survive for conservative recovery.
                self.uncertain = true;
                let file = open(parent, leaf.as_bytes(), Role::Writer, true).map_err(io)?;
                created = true;
                let mut held = Held::new(file, leaf.as_bytes(), false, true)?;
                held.check(Some(parent))?;
                require(
                    held.file.metadata().map_err(io)?.len() == 0,
                    "New journal not empty",
                )?;
                #[cfg(test)]
                super::tests::boundary("record-created")?;
                held.file.write_all(bytes).map_err(io)?;
                #[cfg(test)]
                super::tests::boundary("record-written")?;
                held.file.flush().map_err(io)?;
                held.file.sync_all().map_err(io)?;
                held.check(Some(parent))?;
                namespace_boundary(parent)?;
                #[cfg(test)]
                super::tests::boundary("record-flushed")?;
                self.lease.check()?;
                let id = held.id;
                let expected_security = held.security.clone();
                drop(held);
                let fresh = journals(&self.lease, self.selected, self.index)?;
                require(
                    fresh.iter().any(|r| {
                        r.generation == generation
                            && r.held.id == id
                            && r.held.security == expected_security
                            && r.bytes == bytes
                    }),
                    "Journal close/reopen substitution",
                )?;
                self.records = fresh;
                self.records.sort_by(|a, b| a.held.leaf.cmp(&b.held.leaf));
                self.uncertain = false;
                self.retain(keep)
            })()
            .map_err(|e| {
                if created || self.uncertain {
                    self.uncertain = true;
                    format!("Journal publication uncertain; retained residue: {e}")
                } else {
                    e
                }
            })
        }
    }
    impl Provider for &mut WindowsProvider<'_> {
        fn revoke_preparation(&mut self) {
            (**self).revoke_preparation();
        }
        fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<()> {
            (**self).prepared(proof)
        }

        fn ownership(&self) -> Ownership {
            (**self).ownership()
        }
        fn verify(&mut self, file: &File, own: &Ownership) -> Result<()> {
            (**self).verify(file, own)
        }
        fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
            (**self).scan(visit)
        }
        fn retain(&mut self, keep: &[u64]) -> Result<()> {
            (**self).retain(keep)
        }
        fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<()> {
            (**self).publish(generation, bytes, keep)
        }
    }
    /// This is a validation boundary, NOT a directory durability barrier.
    /// Unprivileged FlushFileBuffers on files cannot promise directory entries
    /// survive power loss. Missing/latest-lost records are independently recovered.
    fn namespace_boundary(parent: &File) -> Result<()> {
        snapshot(parent, true).map(|_| ())
    }
    fn dispose(parent: &File, held: &Held) -> Result<()> {
        held.check(Some(parent))?;
        let deleting = relative(parent, &held.native_leaf, Role::Delete, false).map_err(io)?;
        require(
            Identity::of(&deleting)? == held.id
                && security(&deleting, false, true)? == held.security,
            "Delete acquisition changed",
        )?;
        held.check(Some(parent))?;
        disposition(&deleting)
    }
    fn disposition(file: &File) -> Result<()> {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(last());
        }
        Ok(())
    }
    fn rename(file: &File, parent: &File, leaf: &[u8]) -> std::io::Result<()> {
        let units = wide(leaf);
        if !windows_directory_leaf_supported(&units) {
            return Err(std::io::Error::other("Invalid final name"));
        }
        let length = units
            .len()
            .checked_mul(2)
            .ok_or_else(|| std::io::Error::other("Rename overflow"))?;
        let offset = offset_of!(FILE_RENAME_INFO, FileName);
        let bytes = offset
            .checked_add(length)
            .ok_or_else(|| std::io::Error::other("Rename overflow"))?
            .max(size_of::<FILE_RENAME_INFO>());
        let mut aligned = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        // Allocation is aligned for the SDK header, zero-filled, and bounded by a 255-unit leaf.
        let info = aligned.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = parent.as_raw_handle();
            (*info).FileNameLength = length as u32;
            ptr::copy_nonoverlapping(
                units.as_ptr(),
                ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                units.len(),
            );
            if SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
    impl<'a> Partial<'a> {
        pub(in crate::setup) fn publish(mut self, cancel: &AtomicBool) -> Result<FinalHandle<'a>> {
            stop(cancel)?;
            let own = self.provider.own.clone();
            self.provider.verify(&self.file, &own)?;
            require(
                self.provider.prepared,
                "Publication requires successful Session preparation",
            )?;
            self.file.flush().map_err(io)?;
            self.file.sync_all().map_err(io)?;
            verify_bytes(
                &mut self.file,
                self.provider.selected,
                self.provider.index,
                cancel,
            )?;
            self.provider.verify(&self.file, &own)?;
            stop(cancel)?;
            let final_name = self
                .provider
                .selected
                .bundle()
                .component(self.provider.index)
                .unwrap()
                .filename;
            let lease = self.provider.lease.clone();
            match rename(&self.file, &lease.payload.file, final_name.as_bytes()) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // The winner is authenticated independently. Our partial and its records remain residue.
                    return final_handle(
                        lease,
                        self.provider.selected,
                        self.provider.index,
                        cancel,
                    )?
                    .ok_or("Final collision disappeared".into());
                }
                Err(e) => return Err(io(e)),
            }
            let result = (|| {
                #[cfg(test)]
                super::tests::boundary("final-renamed")?;
                self.provider.payload.leaf = final_name.as_bytes().to_vec();
                self.provider.payload.native_leaf = wide(final_name.as_bytes());
                self.provider.payload.name = native_name(&self.file)?;
                self.provider.payload.check(Some(&lease.payload.file))?;
                let expected_id = self.provider.payload.id;
                let expected_security = self.provider.payload.security.clone();
                let Partial { provider, file } = self;
                let WindowsProvider {
                    selected,
                    index,
                    payload,
                    mut records,
                    ..
                } = provider;
                // Both duplicated write/DELETE handles must close before READ-only sharing can open.
                drop(file);
                drop(payload);
                #[cfg(test)]
                super::tests::boundary("writer-closed")?;
                let mut final_file = final_handle(lease.clone(), selected, index, cancel)?
                    .ok_or("Published final disappeared")?;
                require(
                    final_file.held.id == expected_id
                        && final_file.held.security == expected_security,
                    "Published close/reopen identity/security changed",
                )?;
                // final_handle has rehashed exact bytes/EOF through the close/reopen gap.
                #[cfg(test)]
                super::tests::boundary("final-verified")?;
                stop(cancel)?;
                let fresh = journals(&lease, selected, index)?;
                require(
                    same_scan(&fresh, &records),
                    "Journal changed after final verification",
                )?;
                drop(fresh);
                for record in records.drain(..) {
                    dispose(&lease.journals[index].file, &record.held)?;
                }
                namespace_boundary(&lease.journals[index].file)?;
                require(
                    entries(&lease.journals[index].file)?.is_empty(),
                    "Journal retirement left residue",
                )?;
                final_file.residue = false;
                Ok(final_file)
            })();
            result
                .map_err(|e: String| format!("Final publication uncertain; retained residue: {e}"))
        }
        /// Resume later drops the owner and keeps records. Discard requires fresh
        /// Session preparation, exact DELETE-capable ownership and closed handles.
        pub(in crate::setup) fn discard(
            mut self,
            operation: u64,
            cancel: &AtomicBool,
        ) -> Result<()> {
            (|| {
                {
                    let selected = self.provider.selected;
                    let mut session = super::super::Session::new(
                        selected,
                        self.provider.index,
                        operation,
                        &mut self.provider,
                    )?;
                    drop(session.prepare(selected, &mut self.file, cancel)?);
                }
                stop(cancel)?;
                let own = self.provider.own.clone();
                self.provider.verify(&self.file, &own)?;
                require(
                    self.provider.prepared,
                    "Discard requires successful preparation",
                )?;
                disposition(&self.provider.payload.file)?;
                let Partial { provider, file } = self;
                let WindowsProvider {
                    lease,
                    selected,
                    index,
                    payload,
                    records,
                    ..
                } = provider;
                drop(file);
                drop(payload);
                // Positively owned source is deleted before any journal retirement.
                require(
                    !entries(&lease.payload.file)?
                        .iter()
                        .any(|n| n == own.leaf.as_bytes()),
                    "Discard retained payload residue",
                )?;
                let fresh = journals(&lease, selected, index)?;
                require(same_scan(&fresh, &records), "Discard journal changed")?;
                drop(fresh);
                for journal in records {
                    dispose(&lease.journals[index].file, &journal.held)?;
                }
                require(
                    entries(&lease.journals[index].file)?.is_empty(),
                    "Discard retained journal residue",
                )
            })()
            .map_err(|e: String| format!("Cache discard incomplete; residue may remain: {e}"))
        }
    }
    impl FinalHandle<'_> {
        pub(in crate::setup) fn has_retained_residue(&self) -> bool {
            self.residue
        }
        /// Verify once at actual consumption, then retain native guards through
        /// the callback, including its error path. Restore the original cursor.
        pub(in crate::setup) fn with_verified_reader<T>(
            &mut self,
            cancel: &AtomicBool,
            consume: impl FnOnce(&mut dyn super::super::HeldReadSeek) -> Result<T>,
        ) -> Result<T> {
            let position = self.held.file.stream_position().map_err(io)?;
            let result = (|| {
                self.verify(cancel)?;
                stop(cancel)?;
                self.held.file.rewind().map_err(io)?;
                consume(&mut self.held.file)
            })();
            // Run every postcheck even on callback/verification failure. Do not
            // short circuit away restoration or the final cancellation check.
            let restored = self
                .held
                .file
                .seek(std::io::SeekFrom::Start(position))
                .map_err(io);
            let held = self.held.check(Some(&self.lease.payload.file));
            let root = self.lease.check();
            let stopped = stop(cancel);
            super::super::finish_held_read(result, restored, held, root, stopped)
        }

        pub(in crate::setup) fn verify(&mut self, cancel: &AtomicBool) -> Result<()> {
            self.lease.check()?;
            self.held.check(Some(&self.lease.payload.file))?;
            verify_bytes(&mut self.held.file, self.selected, self.index, cancel)?;
            self.held.check(Some(&self.lease.payload.file))?;
            self.lease.check()?;
            stop(cancel)
        }
    }
    #[cfg(test)]
    pub(super) fn fixture_acquire(path: &Path, selected: &SelectedManifest) -> Result<CacheRoot> {
        CacheRoot::acquire_at(path, selected)
    }
    #[cfg(test)]
    pub(super) fn fixture_file(path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = std::fs::OpenOptions::new()
            .access_mode(Role::Directory.access())
            .share_mode(Role::Directory.share())
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path.parent().unwrap())
            .map_err(io)?;
        let mut file = relative(
            &parent,
            &path.file_name().unwrap().encode_wide().collect::<Vec<_>>(),
            Role::Writer,
            true,
        )
        .map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)
    }
    #[cfg(test)]
    pub(super) fn fixture_acl(path: &Path, protected: bool) -> Result<()> {
        let file = std::fs::OpenOptions::new()
            .access_mode(WRITE_DAC | READ_CONTROL)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(io)?;
        let user = current_user()?;
        let text: Vec<u16> = format!(
            "D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){}\0",
            if protected { "(A;;0x40;;;WD)" } else { "" }
        )
        .encode_utf16()
        .collect();
        let mut sd = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last());
        }
        let _allocation = LocalAllocation(sd);
        let mut dacl = ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) } == 0 {
            return Err(last());
        }
        let rc = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION
                    | if protected {
                        PROTECTED_DACL_SECURITY_INFORMATION
                    } else {
                        UNPROTECTED_DACL_SECURITY_INFORMATION
                    },
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(io(std::io::Error::from_raw_os_error(rc as i32)));
        }
        Ok(())
    }
}
