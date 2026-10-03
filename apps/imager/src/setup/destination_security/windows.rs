//! Native handle-based ACL validation and creation-time private descriptors.
//! No permission changes are made to existing parents or source objects.
use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::ptr;

use cap_primitives::fs::Metadata;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GENERIC_MAPPING, GetAce,
    GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation, INHERIT_ONLY_ACE, IsValidAcl,
    IsValidSid, MapGenericMask, OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, DELETE, FILE_ALL_ACCESS, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
    GetDriveTypeW, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, GetVolumePathNameW,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::refusal;

const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
// Windows Modules Installer is the privileged owner of standard OS directories.
const TRUSTED_INSTALLER_SID: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const FILE_PERSISTENT_ACLS: u32 = 8;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOVABLE: u32 = 2;

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: all non-null instances own a LocalAlloc-family SDK output.
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
        // SAFETY: owns the token handle returned by OpenProcessToken.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

unsafe fn sid_text(sid: PSID) -> io::Result<String> {
    // SAFETY: callers supply an SID within a live validated SDK buffer.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(refusal("invalid security principal"));
    }
    let mut text = ptr::null_mut();
    // SAFETY: valid SID; text is a writable pointer output.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocated = LocalAllocation(text.cast());
    let mut length = 0;
    // SAFETY: the API returns a nul-terminated LocalAlloc UTF-16 string.
    while unsafe { *text.add(length) } != 0 {
        length = length
            .checked_add(1)
            .ok_or_else(|| refusal("SID string length overflow"))?;
    }
    // SAFETY: length was measured within the returned terminated allocation.
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| refusal("invalid SID encoding"));
    drop(allocated);
    value
}

fn current_user() -> io::Result<String> {
    let mut raw = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle; raw is an output.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(raw);
    let mut bytes = 0;
    // SAFETY: zero-length query obtains the required TOKEN_USER buffer size.
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &raw mut bytes);
    }
    if (bytes as usize) < std::mem::size_of::<TOKEN_USER>() || bytes > 1024 * 1024 {
        return Err(refusal("cannot size current-user security token"));
    }
    // usize storage provides alignment for TOKEN_USER on both Windows ABIs.
    let mut buffer = vec![0_usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: aligned buffer has at least bytes capacity; token is live.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &raw mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful TokenUser query initialized a TOKEN_USER with SID
    // storage inside buffer, which remains live until sid_text returns.
    unsafe { sid_text((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid) }
}

fn trusted(sid: &str, user: &str) -> bool {
    sid == user || matches!(sid, SYSTEM_SID | ADMINISTRATORS_SID | TRUSTED_INSTALLER_SID)
}

struct Security {
    allocation: LocalAllocation,
    owner: PSID,
    dacl: *mut ACL,
}
impl Security {
    fn query(file: &File) -> io::Result<Self> {
        let mut descriptor = ptr::null_mut();
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        // SAFETY: handle stays live; all pointers are SDK outputs. Cap's read
        // directory handles request GENERIC_READ, including READ_CONTROL.
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        let allocation = LocalAllocation(descriptor);
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result.cast_signed()));
        }
        // NULL DACL means unrestricted access; never interpret it as empty.
        if owner.is_null()
            || dacl.is_null()
            || descriptor.is_null()
            || unsafe { IsValidAcl(dacl) } == 0
        {
            return Err(refusal(
                "destination security descriptor is unavailable or unrestricted",
            ));
        }
        Ok(Self {
            allocation,
            owner,
            dacl,
        })
    }

    fn check(&self, user: &str, private: bool) -> io::Result<()> {
        // SAFETY: owner belongs to the live GetSecurityInfo allocation.
        let owner = unsafe { sid_text(self.owner)? };
        if !trusted(&owner, user) || (private && owner != user) {
            return Err(refusal("destination owner is not a trusted principal"));
        }
        // SAFETY: dacl was returned and validated by the SDK.
        let count = unsafe { (*self.dacl).AceCount };
        for index in 0..count {
            let mut ace = ptr::null_mut();
            // SAFETY: native ACL remains allocated; index is within AceCount.
            if unsafe { GetAce(self.dacl, u32::from(index), &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: GetAce returned a valid ACE_HEADER within the ACL.
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            // Ordinary allow and deny entries are understood. Callback,
            // object-specific and conditional entries require another adapter.
            if !matches!(header.AceType, 0 | 1) {
                return Err(refusal("unsupported destination ACL entry type"));
            }
            if header.AceType == 1 {
                continue;
            }
            if !private && u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            if header.AceSize < 16 {
                return Err(refusal("malformed destination ACL entry"));
            }
            // SAFETY: allow ACE has header/mask/SID layout; minimum SID fits.
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            // SAFETY: the SID is in an OS-validated ACE; verify its entire
            // length is within that ACE before converting it to a string.
            if unsafe { GetLengthSid(sid) } > u32::from(header.AceSize).saturating_sub(8) {
                return Err(refusal("destination ACL principal exceeds its entry"));
            }
            let principal = unsafe { sid_text(sid)? };
            if trusted(&principal, user) {
                continue;
            }
            let mut mask = allowed.Mask;
            let mapping = GENERIC_MAPPING {
                GenericRead: FILE_GENERIC_READ,
                GenericWrite: FILE_GENERIC_WRITE,
                GenericExecute: FILE_GENERIC_EXECUTE,
                GenericAll: FILE_ALL_ACCESS,
            };
            // SAFETY: pointers reference initialized local values.
            unsafe {
                MapGenericMask(&raw mut mask, &raw const mapping);
            }
            let unsafe_rights = DELETE
                | FILE_DELETE_CHILD
                | WRITE_DAC
                | WRITE_OWNER
                | FILE_WRITE_DATA
                | FILE_WRITE_EA
                | FILE_WRITE_ATTRIBUTES;
            // ADD_SUBDIRECTORY alone is permitted for ordinary C:\ ancestry:
            // it cannot replace protected existing children. Exclusive mkdir
            // rejects name collisions; new children receive an explicit DACL.
            if (private && mask != 0) || mask & unsafe_rights != 0 || mask & !0x001f_01ff != 0 {
                return Err(refusal(
                    "destination ACL grants another principal namespace mutation rights",
                ));
            }
        }
        Ok(())
    }

    fn protected(&self) -> io::Result<bool> {
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: descriptor remains allocated; outputs are correctly sized.
        if unsafe {
            GetSecurityDescriptorControl(self.allocation.0, &raw mut control, &raw mut revision)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(control & SE_DACL_PROTECTED != 0)
    }
}

fn handle_path(file: &File) -> io::Result<PathBuf> {
    // SAFETY: live directory handle; zero capacity queries required length.
    let length = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), ptr::null_mut(), 0, 0) };
    if length == 0 || length > 65_536 {
        return Err(refusal("cannot resolve pinned destination handle"));
    }
    let mut name = vec![0_u16; length as usize];
    // SAFETY: allocated buffer length matches supplied capacity.
    let written =
        unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), name.as_mut_ptr(), length, 0) };
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = name
        .get(..written as usize)
        .filter(|_| written < length)
        .ok_or_else(|| refusal("pinned destination spelling changed"))?;
    Ok(PathBuf::from(OsString::from_wide(text)))
}

fn terminated(path: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<u16> = path.encode_wide().collect();
    if result.contains(&0) {
        return Err(refusal("destination name contains a nul"));
    }
    result.push(0);
    Ok(result)
}

fn check_volume(file: &File) -> io::Result<()> {
    let path = terminated(handle_path(file)?.as_os_str())?;
    let mut root = vec![0_u16; 32_768];
    // SAFETY: path is terminated; root is a writable MAX_PATH_EXTENDED buffer.
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), 32_768) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetVolumePathNameW produced a terminated volume-root spelling.
    if !matches!(
        unsafe { GetDriveTypeW(root.as_ptr()) },
        DRIVE_FIXED | DRIVE_REMOVABLE
    ) {
        return Err(refusal(
            "destination needs a supported local Windows volume",
        ));
    }
    let mut flags = 0;
    // SAFETY: live file handle and flags output; unused outputs may be NULL.
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut flags,
            ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(refusal(
            "destination volume does not enforce persistent ACLs",
        ));
    }
    Ok(())
}

pub(crate) fn check_directory(
    file: &File,
    _child: Option<&Metadata>,
    private: bool,
) -> io::Result<()> {
    check_volume(file)?;
    if !file.metadata()?.is_dir() {
        return Err(refusal("destination ancestor is not a directory"));
    }
    let security = Security::query(file)?;
    security.check(&current_user()?, private)?;
    if private && !security.protected()? {
        return Err(refusal("new destination directory ACL is not protected"));
    }
    Ok(())
}

pub(crate) fn check_private_file(file: &File) -> io::Result<()> {
    Security::query(file)?.check(&current_user()?, true)
}

fn descriptor(text: &str) -> io::Result<LocalAllocation> {
    let text = terminated(OsStr::new(text))?;
    let mut output = ptr::null_mut();
    // SAFETY: valid terminated SDDL and SDK-owned pointer output; revision 1.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &raw mut output,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(output))
}

pub(crate) fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    // The caller holds/revalidates the full cap directory chain. On Windows
    // those handles deny FILE_SHARE_DELETE, pinning all ancestor names while
    // CreateDirectoryW uses this handle-derived spelling.
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(refusal("destination child must have one normal component"));
    }
    let user = current_user()?;
    let security = descriptor(&format!(
        "O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    ))?;
    create_secured(parent, name, &security)
}

fn create_secured(parent: &File, name: &OsStr, security: &LocalAllocation) -> io::Result<()> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| refusal("security attributes size overflow"))?,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    let path = terminated(handle_path(parent)?.join(name).as_os_str())?;
    // SAFETY: live descriptor, attributes and terminated path. No inherited
    // permissive-DACL window exists: security is applied at creation itself.
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::migration::copy_folder;
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicBool;

    fn acl_fixture(rule: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hello"), b"hello").unwrap();
        let parent =
            cap_primitives::fs::open_ambient_dir(&base, cap_primitives::ambient_authority())
                .unwrap();
        let user = current_user().unwrap();
        let security = descriptor(&format!(
            "O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){rule}"
        ))
        .unwrap();
        create_secured(&parent, OsStr::new("parent"), &security).unwrap();
        (temp, source, base.join("parent"))
    }

    /// Windows parent ACL checks and protected private ACL on creation.
    #[test]
    fn windows_parent_acls_and_private_creation_are_enforced() {
        // Phase `windows_refuses_parent_with_untrusted_delete_child_acl`.
        // Catches trusting ownership or READ_CONTROL alone while another account
        // has authority to replace children between validation and mkdir/open.
        {
            let (_temp, source, parent) = acl_fixture("(A;;0x40;;;WD)");
            fs::write(parent.join("sentinel"), b"keep").unwrap();
            assert!(
                copy_folder(
                    &source,
                    &parent.join("copy"),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
            assert!(!parent.join("copy").exists());
            assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `windows_allows_parent_add_subdirectory_without_replacement_rights`.
        // Ancestor child-creation rights alone cannot replace our protected child.
        {
            let (_temp, source, parent) = acl_fixture("(A;;0x4;;;WD)");
            let report = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(fs::read(parent.join("copy/hello")).unwrap(), b"hello");
        }

        // Phase `windows_creation_uses_protected_private_acl_despite_parent_inheritance`.
        // Catches inherited permissive ACLs or protecting the DACL only after
        // directory creation. The private descriptor overrides inheritance.
        {
            let (_temp, source, parent) = acl_fixture("(A;OICIIO;FA;;;WD)");
            let destination = parent.join("copy");
            let report =
                copy_folder(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(report.complete(), "{report:?}");
            let dir = cap_primitives::fs::open_ambient_dir(
                &destination,
                cap_primitives::ambient_authority(),
            )
            .unwrap();
            check_directory(&dir, None, true).unwrap();
            let copied = File::open(destination.join("hello")).unwrap();
            check_private_file(&copied).unwrap();
            assert_eq!(fs::read(destination.join("hello")).unwrap(), b"hello");
        }
    }
}
