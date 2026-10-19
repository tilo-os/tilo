//! Closed Linux POSIX ACL metadata. Named principals require authenticated mapping.
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
};
const NAMES: [&std::ffi::CStr; 2] = [c"system.posix_acl_access", c"system.posix_acl_default"];
const LIMIT: usize = 65536;
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Linux POSIX ACL metadata",
    )
}
fn unmapped() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "This file grants access to named users or groups. Their identities must be mapped to the destination before Home adoption.",
    )
}
pub fn parse_text(bytes: &[u8], default: bool) -> io::Result<Option<Vec<u8>>> {
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut entries = BTreeMap::new();
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() != 3 {
            return Err(invalid());
        }
        let tag: u16 = match (fields[0], fields[1]) {
            ("user", "") => 1,
            ("group", "") => 4,
            ("mask", "") => 16,
            ("other", "") => 32,
            ("user" | "group", id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                id.parse::<u32>().map_err(|_| invalid())?;
                return Err(unmapped());
            }
            _ => return Err(invalid()),
        };
        let permissions = fields[2].as_bytes();
        if permissions.len() != 3
            || !matches!(permissions[0], b'r' | b'-')
            || !matches!(permissions[1], b'w' | b'-')
            || !matches!(permissions[2], b'x' | b'-')
        {
            return Err(invalid());
        }
        let value: u16 = u16::from(permissions[0] == b'r') * 4
            + u16::from(permissions[1] == b'w') * 2
            + u16::from(permissions[2] == b'x');
        if entries.insert(tag, value).is_some() {
            return Err(invalid());
        }
    }
    if ![1, 4, 32].iter().all(|tag| entries.contains_key(tag)) {
        return Err(invalid());
    }
    let mut binary = 2u32.to_le_bytes().to_vec();
    for (tag, permissions) in entries {
        binary.extend(tag.to_le_bytes());
        binary.extend(permissions.to_le_bytes());
        binary.extend(u32::MAX.to_le_bytes());
    }
    if !default && binary.len() == 28 {
        Ok(None)
    } else {
        Ok(Some(binary))
    }
}
fn validate_binary(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > LIMIT
        || (bytes.len() < 28 || !(bytes.len() - 4).is_multiple_of(8))
        || bytes[..4] != 2u32.to_le_bytes()
    {
        return Err(invalid());
    }
    let mut tags = Vec::new();
    for entry in bytes[4..].chunks_exact(8) {
        let tag = u16::from_le_bytes([entry[0], entry[1]]);
        if matches!(tag, 2 | 8) {
            return Err(unmapped());
        }
        let permission = u16::from_le_bytes([entry[2], entry[3]]);
        if !matches!(tag, 1 | 4 | 16 | 32) || permission > 7 || entry[4..] != u32::MAX.to_le_bytes()
        {
            return Err(invalid());
        }
        tags.push(tag);
    }
    if tags != [1, 4, 32] && tags != [1, 4, 16, 32] {
        return Err(invalid());
    }
    Ok(())
}
fn read(file: &File, index: usize) -> io::Result<Option<Vec<u8>>> {
    // SAFETY: descriptor/name live; null output obtains the bounded size.
    let length = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            NAMES[index].as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if length < 0 {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if length as usize > LIMIT {
        return Err(invalid());
    }
    let mut bytes = vec![0; length as usize];
    // SAFETY: descriptor/name and bounded writable buffer live.
    let got = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            NAMES[index].as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    if got as usize != bytes.len() {
        return Err(invalid());
    }
    validate_binary(&bytes)?;
    Ok(Some(bytes))
}
pub fn policy(file: &File) -> io::Result<[Option<Vec<u8>>; 2]> {
    Ok([read(file, 0)?, read(file, 1)?])
}
pub fn validate_policy(policy: &[Option<Vec<u8>>; 2]) -> io::Result<()> {
    for value in policy.iter().flatten() {
        validate_binary(value)?;
    }
    Ok(())
}
pub fn verify(file: &File, expected: &[Option<Vec<u8>>; 2]) -> io::Result<()> {
    for (index, value) in expected.iter().enumerate() {
        if read(file, index)?.as_ref() != value.as_ref() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Transferred POSIX ACL differs",
            ));
        }
    }
    Ok(())
}
fn with_mode(mut expected: [Option<Vec<u8>>; 2], mode: u32) -> io::Result<[Option<Vec<u8>>; 2]> {
    if mode & !0o777 != 0 {
        return Err(invalid());
    }
    if let Some(access) = &mut expected[0] {
        validate_binary(access)?;
        let has_mask = access[4..]
            .chunks_exact(8)
            .any(|entry| u16::from_le_bytes([entry[0], entry[1]]) == 16);
        for entry in access[4..].chunks_exact_mut(8) {
            let permissions = match u16::from_le_bytes([entry[0], entry[1]]) {
                1 => Some((mode >> 6) & 7),
                4 if !has_mask => Some((mode >> 3) & 7),
                16 => Some((mode >> 3) & 7),
                32 => Some(mode & 7),
                _ => None,
            };
            if let Some(value) = permissions {
                entry[2..4].copy_from_slice(&(value as u16).to_le_bytes());
            }
        }
    }
    Ok(expected)
}
pub fn verify_private_root(file: &File, expected: &[Option<Vec<u8>>; 2]) -> io::Result<()> {
    if file.metadata()?.mode() & 0o777 != 0o700 {
        return Err(invalid());
    }
    verify(file, &with_mode(expected.clone(), 0o700)?)
}

pub fn copy(
    source: &File,
    destination: &File,
    verify_only: bool,
    destination_mode: u32,
) -> io::Result<()> {
    if destination_mode & !0o777 != 0 {
        return Err(invalid());
    }
    apply(
        destination,
        &[read(source, 0)?, read(source, 1)?],
        destination_mode,
        verify_only,
    )
}
pub fn restore_authenticated(
    destination: &File,
    expected: &[Option<Vec<u8>>; 2],
    destination_mode: u32,
) -> io::Result<()> {
    apply(destination, expected, destination_mode, false)
}
fn apply(
    destination: &File,
    expected: &[Option<Vec<u8>>; 2],
    destination_mode: u32,
    verify_only: bool,
) -> io::Result<()> {
    validate_policy(expected)?;
    let expected = with_mode(expected.clone(), destination_mode)?;
    for (index, value) in expected.iter().enumerate() {
        if read(destination, index)?.as_ref() == value.as_ref() {
            continue;
        }
        if verify_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Imported POSIX ACL changed; review retained data",
            ));
        }
        // SAFETY: held destination and fixed ACL names; bounded validated bytes.
        let result = unsafe {
            match value {
                Some(bytes) => libc::fsetxattr(
                    destination.as_raw_fd(),
                    NAMES[index].as_ptr(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    0,
                ),
                None => libc::fremovexattr(destination.as_raw_fd(), NAMES[index].as_ptr()),
            }
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if !(value.is_none()
                && matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)))
            {
                return Err(error);
            }
        }
    }
    if !verify_only {
        destination.set_permissions(std::fs::Permissions::from_mode(destination_mode))?;
    }
    if destination.metadata()?.mode() & 0o777 != destination_mode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Imported POSIX ACL base permissions changed",
        ));
    }
    verify(destination, &expected)
}

/// Install a real kernel ACL without depending on the optional acl command-line tools.
#[cfg(test)]
pub(crate) fn set_test_acl(path: &std::path::Path, default: bool, text: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = parse_text(text, default)?.ok_or_else(invalid)?;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: path/name are live C strings and bytes holds the encoded Linux ACL.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            NAMES[usize::from(default)].as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Kernel POSIX ACLs for tests, written and read as raw extended attributes
/// so fixtures need neither the acl command-line tools nor the code under test.
#[cfg(test)]
pub(crate) mod fixture {
    use std::{
        io,
        os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        path::Path,
    };

    const ACCESS: &std::ffi::CStr = c"system.posix_acl_access";
    const DEFAULT: &std::ffi::CStr = c"system.posix_acl_default";
    const USER_OBJ: u16 = 0x01;
    const GROUP_OBJ: u16 = 0x04;
    const MASK: u16 = 0x10;
    const OTHER: u16 = 0x20;

    fn c_path(path: &Path) -> std::ffi::CString {
        std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap()
    }

    /// The raw attribute, or `None` when the file has none.
    fn raw(path: &Path, name: &std::ffi::CStr) -> Option<Vec<u8>> {
        let path = c_path(path);
        let mut bytes = vec![0u8; 65536];
        // SAFETY: live C strings and a writable buffer of the given length.
        let length = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length < 0 {
            let error = io::Error::last_os_error();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::ENODATA),
                "read ACL: {error}"
            );
            return None;
        }
        bytes.truncate(usize::try_from(length).unwrap());
        Some(bytes)
    }

    fn entries(bytes: &[u8]) -> Vec<(u16, u16, u32)> {
        assert_eq!(bytes[..4], 2u32.to_le_bytes(), "ACL version");
        bytes[4..]
            .chunks_exact(8)
            .map(|e| {
                (
                    u16::from_le_bytes([e[0], e[1]]),
                    u16::from_le_bytes([e[2], e[3]]),
                    u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
                )
            })
            .collect()
    }

    fn write(path: &Path, name: &std::ffi::CStr, mut entries: Vec<(u16, u16, u32)>) {
        entries.sort_unstable();
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in entries {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(permissions.to_le_bytes());
            bytes.extend(id.to_le_bytes());
        }
        let path = c_path(path);
        // SAFETY: live C strings and the encoded ACL.
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        assert_eq!(result, 0, "write ACL: {}", io::Error::last_os_error());
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode()
    }

    /// `setfacl -m` for owner, group, mask and other entries: `spec` is a
    /// comma-separated list such as `u::rwx,g::r-x,m::---,o::---,d:m::r-x`.
    pub(crate) fn modify(path: &Path, spec: &str) {
        let mut access = raw(path, ACCESS).map_or_else(
            || {
                let mode = mode(path);
                let bits = |shift: u32| u16::try_from((mode >> shift) & 7).unwrap();
                vec![
                    (USER_OBJ, bits(6), u32::MAX),
                    (GROUP_OBJ, bits(3), u32::MAX),
                    (OTHER, bits(0), u32::MAX),
                ]
            },
            |bytes| entries(&bytes),
        );
        let mut default = raw(path, DEFAULT).map_or_else(Vec::new, |bytes| entries(&bytes));
        let (mut access_changed, mut default_changed) = (false, false);
        for item in spec.split(',') {
            let (target, item) = if let Some(item) = item.strip_prefix("d:") {
                default_changed = true;
                (&mut default, item)
            } else {
                access_changed = true;
                (&mut access, item)
            };
            let (tag, permissions) = item.split_once("::").expect("unnamed ACL entry");
            let tag = match tag {
                "u" => USER_OBJ,
                "g" => GROUP_OBJ,
                "m" => MASK,
                "o" => OTHER,
                other => panic!("unsupported ACL entry {other}"),
            };
            let bits = permissions.as_bytes();
            assert_eq!(bits.len(), 3, "ACL permissions {permissions}");
            let value = u16::from(bits[0] == b'r') * 4
                + u16::from(bits[1] == b'w') * 2
                + u16::from(bits[2] == b'x');
            target.retain(|&(existing, _, _)| existing != tag);
            target.push((tag, value, u32::MAX));
        }
        if access_changed {
            write(path, ACCESS, access);
        }
        if default_changed {
            write(path, DEFAULT, default);
        }
    }

    /// `setfacl -b -k`: the file keeps only its owner, group and other
    /// permissions, the group's being its ACL group entry.
    pub(crate) fn remove(path: &Path) {
        let cpath = c_path(path);
        if let Some(bytes) = raw(path, ACCESS) {
            let group = entries(&bytes)
                .into_iter()
                .find(|&(tag, _, _)| tag == GROUP_OBJ)
                .map_or(0, |(_, permissions, _)| u32::from(permissions));
            // SAFETY: live C strings.
            assert_eq!(
                unsafe { libc::removexattr(cpath.as_ptr(), ACCESS.as_ptr()) },
                0
            );
            let mode = mode(path) & !0o070 | group << 3;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        if raw(path, DEFAULT).is_some() {
            // SAFETY: live C strings.
            assert_eq!(
                unsafe { libc::removexattr(cpath.as_ptr(), DEFAULT.as_ptr()) },
                0
            );
        }
    }

    /// What `getfacl` reports: the permission bits and both raw ACLs.
    pub(crate) fn state(path: &Path) -> (u32, Option<Vec<u8>>, Option<Vec<u8>>) {
        (mode(path) & 0o7777, raw(path, ACCESS), raw(path, DEFAULT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acl_parser_rejects_unmapped_principals_and_malformed_metadata() {
        for value in [
            b"user::rwx\nuser:99:r--\ngroup::r--\nmask::r--\nother::---".as_slice(),
            b"user::rwx\ngroup::r--\ngroup:99:r--\nmask::r--\nother::---",
        ] {
            assert_eq!(
                parse_text(value, false).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        for value in [
            b"user::rwx\ngroup::r-x".as_slice(),
            b"user::rwx\nuser::rwx\ngroup::r-x\nother::---",
            b"user::rwx\ngroup::r-x\nother::rwx:99",
            b"user::rwx\ngroup::rx-\nother::---",
        ] {
            assert!(parse_text(value, false).is_err());
        }
        assert!(
            parse_text(b"user::rwx\ngroup::r-x\nother::---", false)
                .unwrap()
                .is_none()
        );
        assert!(
            parse_text(b"user::rwx\ngroup::r-x\nother::---", true)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn actual_acl_copy_preserves_default_and_mask_and_detects_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        fixture::modify(
            &source,
            "u::rwx,g::r-x,m::r-x,o::---,d:u::rwx,d:g::r-x,d:m::r-x,d:o::---",
        );
        let source_file = File::open(&source).unwrap();
        let target_file = File::open(&target).unwrap();
        copy(
            &source_file,
            &target_file,
            false,
            source_file.metadata().unwrap().mode() & 0o777,
        )
        .unwrap();
        let acl = fixture::state;
        assert_eq!(acl(&source), acl(&target));
        copy(
            &source_file,
            &target_file,
            true,
            source_file.metadata().unwrap().mode() & 0o777,
        )
        .unwrap();
        fixture::modify(&target, "m::---");
        assert!(
            copy(
                &source_file,
                &target_file,
                true,
                source_file.metadata().unwrap().mode() & 0o777
            )
            .is_err()
        );
        fixture::remove(&source);
        copy(
            &source_file,
            &target_file,
            false,
            source_file.metadata().unwrap().mode() & 0o777,
        )
        .unwrap();
        assert_eq!(acl(&source), acl(&target));
    }
}

#[cfg(test)]
mod private_root_tests {
    use super::*;
    #[test]
    fn private_root_restricts_access_mask_and_preserves_default_acl() {
        let temporary = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["source", "target", "reference"]
            .iter()
            .map(|name| temporary.path().join(name))
            .collect();
        for path in &paths {
            std::fs::create_dir(path).unwrap();
        }
        for path in [&paths[0], &paths[2]] {
            fixture::modify(
                path,
                "u::rwx,g::r-x,m::r-x,o::r-x,d:u::rwx,d:g::r-x,d:m::r-x,d:o::---",
            );
        }
        let source = File::open(&paths[0]).unwrap();
        let target = File::open(&paths[1]).unwrap();
        let original = [read(&source, 0).unwrap(), read(&source, 1).unwrap()];
        std::fs::set_permissions(&paths[2], std::fs::Permissions::from_mode(0o700)).unwrap();
        copy(&source, &target, false, 0o700).unwrap();
        verify_private_root(&target, &original).unwrap();
        copy(&source, &target, true, 0o700).unwrap();
        let get = fixture::state;
        assert_eq!(get(&paths[1]), get(&paths[2]));
        assert_eq!(target.metadata().unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            [read(&source, 0).unwrap(), read(&source, 1).unwrap()],
            original
        );
        assert_eq!(read(&target, 1).unwrap(), original[1]);
    }
}
