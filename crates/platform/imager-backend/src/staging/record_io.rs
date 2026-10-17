//! Native durable staging journal I/O and privileged directory protection.

use std::path::{Path, PathBuf};

/// Linux: `/var/lib/tilo-imager/staging`; Windows: `%ProgramData%\Tilo\staging`.
pub fn system_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData").map(PathBuf::from)?;
        Some(base.join("Tilo").join("staging"))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from("/var/lib/tilo-imager/staging"))
    }
}

/// The world-readable status mirror next to [`system_dir`].
pub fn public_dir() -> Option<PathBuf> {
    let dir = system_dir()?;
    Some(dir.with_file_name("staging-status"))
}

const MAX_RECORD_BYTES: u64 = 4 << 20;

pub fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("record is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(std::io::Error::other("record is unexpectedly large"));
    }
    Ok(bytes)
}

/// Write `bytes` to `dir/name` durably and atomically. `public` selects 0644
/// over 0600.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8], public: bool) -> std::io::Result<()> {
    let path = dir.join(name);
    #[cfg(unix)]
    {
        tilo_files::native::atomic::write_with_mode(
            &path,
            bytes,
            if public { 0o644 } else { 0o600 },
        )
    }
    #[cfg(windows)]
    {
        let _ = public;
        tilo_files::native::atomic::write(&path, bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, bytes, public);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "staging journal storage is unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
pub mod secure {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    /// Create `dir` (and parents) and make it root-safe: private 0700 or
    /// public 0755, never group/world writable, owned by the current euid.
    pub fn ensure_dir(dir: &Path, public: bool) -> Result<(), String> {
        let io = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(dir).map_err(io)?;
        let meta = std::fs::symlink_metadata(dir).map_err(io)?;
        let euid = tilo_process::effective_uid();
        if !meta.is_dir() || meta.uid() != euid {
            return Err(format!(
                "{} is not a directory owned by this helper",
                dir.display()
            ));
        }
        let mode = if public { 0o755 } else { 0o700 };
        if meta.permissions().mode() & 0o7777 != mode {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).map_err(io)?;
        }
        Ok(())
    }

    pub fn check_owner(path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .map(std::fs::symlink_metadata)
            .transpose()
            .map_err(|e| e.to_string())?;
        let root_only = |m: &std::fs::Metadata| m.uid() == 0 && m.permissions().mode() & 0o022 == 0;
        if !meta.is_file() || !root_only(&meta) || !parent.as_ref().is_some_and(root_only) {
            return Err(format!(
                "{} is not a root-owned staging record",
                path.display()
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
pub mod secure {
    //! Protected DACLs for the record directories. `%ProgramData%` lets
    //! ordinary users create subdirectories, so a pre-planted directory must
    //! be re-owned and re-protected rather than trusted.
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID,
    };

    const PRIVATE: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
    const PUBLIC: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)";

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by a LocalAlloc-returning API below.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    pub fn ensure_dir(dir: &Path, public: bool) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let text: Vec<u16> = if public { PUBLIC } else { PRIVATE }
            .encode_utf16()
            .chain([0])
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid NUL-terminated SDDL; output freed by `Local`.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let _sd = Local(sd);
        let mut owner: PSID = ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        let mut dacl = ptr::null_mut();
        // SAFETY: `sd` is a live self-relative descriptor.
        unsafe {
            GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted);
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
        }
        let path = wide(dir);
        // SAFETY: path is NUL-terminated; owner/dacl point into `sd`.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(format!("{}: security update failed ({rc})", dir.display()));
        }
        Ok(())
    }

    pub fn check_owner(path: &Path) -> Result<(), String> {
        let wide_path = wide(path);
        let mut owner: PSID = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: path NUL-terminated; `sd` freed by `Local`.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut sd,
            )
        };
        let _sd = Local(sd);
        if rc != ERROR_SUCCESS || owner.is_null() {
            return Err(format!("{}: owner unreadable ({rc})", path.display()));
        }
        let mut text = ptr::null_mut();
        // SAFETY: `owner` points into the live descriptor.
        if unsafe { ConvertSidToStringSidW(owner, &mut text) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let _text = Local(text.cast());
        let mut n = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated string.
        while n < 256 && unsafe { *text.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: `n` units were just read.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
        if !matches!(sid.as_str(), "S-1-5-18" | "S-1-5-32-544") {
            return Err(format!(
                "{} is not owned by SYSTEM or Administrators",
                path.display()
            ));
        }
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
pub mod secure {
    use std::path::Path;
    pub fn ensure_dir(_: &Path, _: bool) -> Result<(), String> {
        Err("staging records are unsupported on this platform".into())
    }
    pub fn check_owner(_: &Path) -> Result<(), String> {
        Err("staging records are unsupported on this platform".into())
    }
}
