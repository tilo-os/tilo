//! Raw tar iteration bounds extension records before allocation and rejects link traversal.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::Component,
};
mod index;
pub(super) use index::Records;
pub use index::Scratch;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) enum Kind {
    Directory,
    File([u8; 32]),
    Sparse(super::sparse::Witness),
    Link(#[serde(with = "index::native_path")] PathBuf),
    HardLink(#[serde(with = "index::native_path")] PathBuf),
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Record {
    #[serde(with = "index::native_path")]
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    mode: u32,
    mtime: (i64, i64),
    xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    acls: [Option<Vec<u8>>; 2],
}
pub(super) fn logical_bytes(records: &Records) -> io::Result<u64> {
    let mut total = 0u64;
    for record in records.iter() {
        let record = record?;
        let size = match &record.kind {
            Kind::File(_) | Kind::Sparse(_) => record.size,
            Kind::HardLink(target) => {
                records
                    .get(target)?
                    .ok_or_else(|| changed("Hardlink size missing"))?
                    .size
            }
            _ => continue,
        };
        total = total
            .checked_add(size)
            .filter(|value| i64::try_from(*value).is_ok())
            .ok_or_else(|| changed("Logical archive size exceeds native bounds"))?;
    }
    Ok(total)
}

fn path(bytes: &[u8], root: bool) -> io::Result<PathBuf> {
    if bytes.len() > 4096 || bytes.contains(&0) {
        return Err(changed("Archive path exceeds native bounds"));
    }
    let raw = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    let mut clean = PathBuf::new();
    for part in raw.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => clean.push(name),
            _ => return Err(changed("Archive path leaves its tree")),
        }
    }
    if clean.components().count() > 128 || (!root && clean.as_os_str().is_empty()) {
        return Err(changed("Invalid archive path"));
    }
    Ok(clean)
}
fn timestamp(bytes: &[u8]) -> io::Result<(i64, i64)> {
    let text = std::str::from_utf8(bytes).map_err(|_| changed("Invalid archive timestamp"))?;
    let (seconds, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > 9 || !fraction.bytes().all(|v| v.is_ascii_digit()) {
        return Err(changed("Unsupported archive timestamp precision"));
    }
    let secs = seconds
        .parse::<i64>()
        .map_err(|_| changed("Invalid archive timestamp"))?;
    let nanos = format!("{fraction:0<9}")
        .parse::<i64>()
        .map_err(|_| changed("Invalid archive timestamp"))?;
    if text.starts_with('-') && nanos != 0 {
        Ok((secs - 1, 1_000_000_000 - nanos))
    } else {
        Ok((secs, nanos))
    }
}
// PAX lengths include the decimal prefix and the final newline. Values may
// contain newlines (notably GNU tar POSIX ACLs), so line splitting is invalid.
fn decode_pax(bytes: &[u8], pending: &mut BTreeMap<Vec<u8>, Vec<u8>>) -> io::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let separator = rest
            .iter()
            .position(|byte| *byte == b' ')
            .filter(|length| *length > 0 && *length <= 20)
            .ok_or_else(|| changed("Invalid PAX length framing"))?;
        let digits = &rest[..separator];
        if !digits.iter().all(u8::is_ascii_digit) || digits[0] == b'0' {
            return Err(changed("Invalid PAX decimal length"));
        }
        let length = std::str::from_utf8(digits)
            .map_err(|_| changed("Invalid PAX length"))?
            .parse::<usize>()
            .map_err(|_| changed("PAX length exceeds native bounds"))?;
        if length < separator + 4 || length > rest.len() || rest[length - 1] != b'\n' {
            return Err(changed("PAX record does not match its length"));
        }
        let contents = &rest[separator + 1..length - 1];
        let equals = contents
            .iter()
            .position(|byte| *byte == b'=')
            .filter(|length| *length > 0 && *length <= 255)
            .ok_or_else(|| changed("Invalid PAX key framing"))?;
        let key = &contents[..equals];
        if !key.iter().all(|byte| (0x21..=0x7e).contains(byte)) {
            return Err(changed("Invalid PAX key bytes"));
        }
        if pending
            .insert(key.to_vec(), contents[equals + 1..].to_vec())
            .is_some()
        {
            return Err(changed("Duplicate archive metadata"));
        }
        offset = offset
            .checked_add(length)
            .ok_or_else(|| changed("PAX offset exceeds native bounds"))?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn scan<R: Read>(
    opener: codec::Opener<'_, R>,
    limits: Limits,
) -> io::Result<(Records, codec::Footer)> {
    scan_reporting(opener, limits, &mut |_| Ok(()))
}
pub(super) fn scan_reporting<R: Read>(
    opener: codec::Opener<'_, R>,
    limits: Limits,
    observer: &mut super::Observer<'_>,
) -> io::Result<(Records, codec::Footer)> {
    scan_in(opener, limits, observer, Scratch::default())
}
pub(super) fn scan_in<R: Read>(
    opener: codec::Opener<'_, R>,
    limits: Limits,
    observer: &mut super::Observer<'_>,
    scratch: Scratch,
) -> io::Result<(Records, codec::Footer)> {
    let cancel = opener.cancellation();
    let mut archive = tar::Archive::new(super::progress::Stream::new(
        opener,
        super::ArchivePhase::VerifyingArchive,
        observer,
    )?);
    let mut result = Records::new(scratch);
    let mut pending = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    let mut metadata = 0usize;
    for item in archive.entries()?.raw(true) {
        let mut entry = item?;
        let header = entry.header().clone();
        let kind = header.entry_type();
        if kind.is_pax_local_extensions() {
            let length = usize::try_from(entry.size())
                .map_err(|_| changed("Archive metadata length invalid"))?;
            metadata = metadata
                .checked_add(length)
                .filter(|v| *v <= limits.metadata_bytes)
                .ok_or_else(|| changed("Archive metadata exceeds review bound"))?;
            if length > 1024 * 1024 || !pending.is_empty() {
                return Err(changed("Archive extension record exceeds bounds"));
            }
            let mut bytes = Vec::with_capacity(length);
            entry.read_to_end(&mut bytes)?;
            if bytes.len() != length {
                return Err(changed("Archive metadata record was truncated"));
            }
            decode_pax(&bytes, &mut pending)?;
            continue;
        }
        if result.len() >= limits.entries {
            return Err(changed("Archive entry count exceeds review bound"));
        }
        let data_size = entry.size();
        let sparse = super::sparse::parse(&pending, data_size)?;
        if sparse.is_some() && !kind.is_file() {
            return Err(changed("Sparse metadata requires a regular file"));
        }
        let sparse_name = pending.get(b"GNU.sparse.name".as_slice());
        let regular_name = pending.get(b"path".as_slice());
        if sparse.is_some() {
            // GNU PAX 0.1 uses an ordinary path for its generated placeholder
            // and GNU.sparse.name for the actual member. Neither may traverse.
            if let Some(placeholder) = regular_name {
                path(placeholder, false)?;
            }
        }
        let name = path(
            sparse_name
                .or(regular_name)
                .map_or(&entry.path_bytes(), Vec::as_slice),
            true,
        )?;
        let mode = header.mode()?;
        if mode & !0o777 != 0 {
            return Err(changed(
                "Privileged or special permission bits are unsupported",
            ));
        }
        let mtime = match pending.get(b"mtime".as_slice()) {
            Some(v) => timestamp(v)?,
            None => (
                i64::try_from(header.mtime()?).map_err(|_| changed("Invalid timestamp"))?,
                0,
            ),
        };
        if let Some(charset) = pending.get(b"hdrcharset".as_slice()) {
            if charset != b"BINARY" {
                return Err(changed("Archive filename encoding is unsupported"));
            }
        }
        let mut xattrs = Vec::new();
        let mut acls = [None, None];
        for (key, value) in &pending {
            if key.starts_with(b"GNU.sparse.") && sparse.is_some() {
                // The closed sparse parser validated every GNU key above.
                continue;
            } else if key == b"SCHILY.acl.access" {
                if kind.is_symlink() {
                    return Err(changed("Literal links cannot carry POSIX ACLs"));
                }
                acls[0] = super::acl::parse_text(value, false)?;
            } else if key == b"SCHILY.acl.default" {
                if !kind.is_dir() {
                    return Err(changed("Default POSIX ACL belongs only to a directory"));
                }
                acls[1] = super::acl::parse_text(value, true)?;
            } else if key.starts_with(b"SCHILY.xattr.user.") {
                let name = key[b"SCHILY.xattr.".len()..].to_vec();
                if name.contains(&0) || name.len() > 255 || value.len() > 65536 {
                    return Err(changed("Invalid archive xattr"));
                }
                xattrs.push((name, value.clone()));
            } else if !matches!(
                key.as_slice(),
                b"path" | b"linkpath" | b"mtime" | b"atime" | b"ctime" | b"hdrcharset"
            ) {
                return Err(changed(
                    "Archive contains unsupported metadata, including security labels",
                ));
            }
        }
        let link = pending
            .get(b"linkpath".as_slice())
            .cloned()
            .or_else(|| header.link_name_bytes().map(|v| v.to_vec()));
        let logical_size = sparse.as_ref().map_or(data_size, |sparse| sparse.logical);
        let node = if kind.is_dir() {
            if data_size != 0 {
                return Err(changed("Directory has data"));
            }
            Kind::Directory
        } else if let Some(sparse) = sparse {
            Kind::Sparse(sparse.hash(&mut entry, cancel)?)
        } else if kind.is_file() {
            let mut digest = Sha256::new();
            let mut buffer = [0; 65536];
            loop {
                let n = entry.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                digest.update(&buffer[..n]);
            }
            Kind::File(digest.finalize().into())
        } else if kind.is_symlink() {
            let target = link.ok_or_else(|| changed("Link target missing"))?;
            if target.len() > 4096 || target.contains(&0) || data_size != 0 {
                return Err(changed("Invalid literal link"));
            }
            Kind::Link(PathBuf::from(OsString::from_vec(target)))
        } else if kind.is_hard_link() {
            let target = path(
                &link.ok_or_else(|| changed("Hardlink target missing"))?,
                false,
            )?;
            if data_size != 0 {
                return Err(changed("Hardlink target is not an earlier regular file"));
            }
            Kind::HardLink(target)
        } else {
            return Err(changed("Archive entry type is unsupported"));
        };
        if name.as_os_str().is_empty() && !matches!(node, Kind::Directory) {
            return Err(changed("Archive root is not a directory"));
        }
        let explicit_acl = pending.contains_key(b"SCHILY.acl.access".as_slice())
            || pending.contains_key(b"SCHILY.acl.default".as_slice());
        result.push(
            Record {
                path: name,
                kind: node,
                size: logical_size,
                mode,
                mtime,
                xattrs,
                acls,
            },
            explicit_acl,
            cancel,
        )?;
        pending.clear();
    }
    if !pending.is_empty() {
        return Err(changed("Archive ends with incomplete metadata"));
    }
    let mut opener = archive.into_inner();
    let mut padding = [0; 65536];
    loop {
        let n = opener.read(&mut padding)?;
        if n == 0 {
            break;
        }
        if padding[..n].iter().any(|v| *v != 0) {
            return Err(changed("Archive contains trailing non-padding data"));
        }
    }
    let mut opener = opener.into_inner();
    if opener.footer.is_none() || result.is_empty() {
        return Err(changed("Archive is incomplete"));
    }
    let footer = opener
        .footer
        .take()
        .ok_or_else(|| changed("Archive footer missing"))?;
    result.finish(cancel)?;
    let logical = logical_bytes(&result)?;
    if footer.logical_bytes.is_some_and(|bound| bound != logical)
        || (footer.logical_bytes.is_none()
            && result.iter().try_fold(false, |found, record| {
                Ok::<_, io::Error>(found || matches!(record?.kind, Kind::Sparse(_)))
            })?)
    {
        return Err(changed(
            "Sparse logical size lacks its authenticated source bound",
        ));
    }
    Ok((result, footer))
}

pub(super) fn parent(
    root: &Arc<PinnedDirectory>,
    path: &Path,
) -> io::Result<(Arc<PinnedDirectory>, OsString)> {
    let mut directory = Arc::clone(root);
    let name = path
        .file_name()
        .ok_or_else(|| changed("Entry name missing"))?
        .to_owned();
    for part in path.parent().unwrap_or(Path::new("")).components() {
        let Component::Normal(component) = part else {
            return Err(changed("Unsafe archive path"));
        };
        let metadata =
            super::super::nofollow::metadata_nofollow(&directory.file, Path::new(component))?;
        directory = directory.open_child(component, &metadata)?;
    }
    Ok((directory, name))
}
pub(super) fn restore_authenticated_acls(
    root: &Arc<PinnedDirectory>,
    records: &Records,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    for record in records.iter() {
        let record = record?;
        check_cancel(cancel)?;
        if matches!(record.kind, Kind::Link(_)) {
            continue;
        }
        let file = if record.path.as_os_str().is_empty() {
            durability::readable_directory(&root.file)?
        } else {
            let (directory, name) = parent(root, &record.path)?;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            let file = cap_primitives::fs::open(&directory.file, Path::new(&name), &options)?;
            directory.validate()?;
            file
        };
        let metadata = file.metadata()?;
        if matches!(record.kind, Kind::Directory) != metadata.is_dir()
            || (!matches!(record.kind, Kind::Directory) && !metadata.is_file())
        {
            return Err(changed(
                "Extracted inode kind differs before ACL restoration",
            ));
        }
        let mode = if record.path.as_os_str().is_empty() {
            0o700
        } else {
            record.mode
        };
        super::acl::restore_authenticated(&file, &record.acls, mode)?;
        durability::sync_file(&file)?;
    }
    root.validate()
}
fn verify_root_acl(
    records: &Records,
    root: &Arc<PinnedDirectory>,
    private_root: bool,
) -> io::Result<()> {
    if let Some(record) = records.get(Path::new(""))? {
        let file = durability::readable_directory(&root.file)?;
        if private_root {
            super::acl::verify_private_root(&file, &record.acls)?;
        } else {
            super::acl::verify(&file, &record.acls)?;
        }
    }
    Ok(())
}
pub(super) fn verify_mapped(
    records: &Records,
    witness: &super::super::home_transfer::promotion::AdoptionWitness,
    cancel: &AtomicBool,
) -> io::Result<()> {
    verify_root_acl(records, &witness.quarantine, true)?;
    let paths = records.iter().filter_map(|record| match record {
        Ok(record) if record.path.as_os_str().is_empty() => None,
        Ok(record) => Some(Ok(record.path)),
        Err(error) => Some(Err(error)),
    });
    witness.verify_archive_items(paths)?;
    verify_records(records, false, cancel, |path: &Path| {
        Ok((witness.target(path)?, witness.changed_parent(path)?))
    })
}
pub(super) fn verify_mapped_after_applications(
    records: &Records,
    witness: &super::super::home_transfer::promotion::AdoptionWitness,
    cancel: &AtomicBool,
) -> io::Result<()> {
    verify_root_acl(records, &witness.quarantine, true)?;
    let paths = records.iter().filter_map(|record| match record {
        Ok(record) if record.path.as_os_str().is_empty() => None,
        Ok(record) => Some(Ok(record.path)),
        Err(error) => Some(Err(error)),
    });
    witness.verify_archive_items(paths)?;
    let installation = Path::new(".local/share/flatpak");
    let mutable = |record: &Record| {
        matches!(record.kind, Kind::File(_) | Kind::Sparse(_))
            && [
                installation.join("repo/config"),
                installation.join(".changed"),
            ]
            .contains(&record.path)
    };
    for record in records.iter() {
        let record = record?;
        if mutable(&record) {
            verify_mutable_application_file(&record, &witness.target(&record.path)?)?;
        }
    }
    verify_records_skipping(
        records,
        false,
        cancel,
        |path| {
            Ok((
                witness.target(path)?,
                witness.changed_parent(path)?
                    || path.starts_with(installation)
                    || installation.starts_with(path),
            ))
        },
        mutable,
    )
}
fn verify_mutable_application_file(record: &Record, root: &Arc<PinnedDirectory>) -> io::Result<()> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    let (directory, name) = parent(root, &record.path)?;
    let metadata = super::super::nofollow::metadata_nofollow(&directory.file, Path::new(&name))?;
    if !metadata.is_file() || metadata.is_symlink() {
        return Err(changed("Mutable application metadata changed inode kind"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&directory.file, Path::new(&name), &options)?;
    let actual = file.metadata()?;
    // SAFETY: geteuid has no preconditions and only reads current credentials.
    if actual.mode() & 0o777 != record.mode || actual.uid() != rustix::process::geteuid().as_raw() {
        return Err(changed("Mutable application metadata changed authority"));
    }
    verify_xattrs(&file, &record.xattrs)?;
    super::acl::verify(&file, &record.acls)?;
    directory.validate()
}
fn verify_records(
    records: &Records,
    private_root: bool,
    cancel: &AtomicBool,
    location: impl Fn(&Path) -> io::Result<(Arc<PinnedDirectory>, bool)>,
) -> io::Result<()> {
    verify_records_skipping(records, private_root, cancel, location, |_| false)
}
fn verify_records_skipping(
    records: &Records,
    private_root: bool,
    cancel: &AtomicBool,
    mut location: impl FnMut(&Path) -> io::Result<(Arc<PinnedDirectory>, bool)>,
    mut skip: impl FnMut(&Record) -> bool,
) -> io::Result<()> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    for record in records.iter() {
        let record = record?;
        check_cancel(cancel)?;
        if record.path.as_os_str().is_empty() || skip(&record) {
            continue;
        }
        let (root, changed_directory) = location(&record.path)?;
        let (directory, name) = parent(&root, &record.path)?;
        let metadata =
            super::super::nofollow::metadata_nofollow(&directory.file, Path::new(&name))?;
        match &record.kind {
            Kind::Link(target) => {
                if !metadata.is_symlink()
                    || cap_primitives::fs::read_link_contents(&directory.file, Path::new(&name))?
                        != *target
                {
                    return Err(changed("Literal link differs from reviewed archive"));
                }
                continue;
            }
            Kind::Directory => {
                if !metadata.is_dir() || metadata.is_symlink() {
                    return Err(changed("Imported directory differs"));
                }
            }
            Kind::File(_) | Kind::Sparse(_) | Kind::HardLink(_) => {
                if !metadata.is_file() || metadata.is_symlink() {
                    return Err(changed("Imported file differs"));
                }
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = cap_primitives::fs::open(&directory.file, Path::new(&name), &options)?;
        let before = file.metadata()?;
        if before.mode() & 0o777 != record.mode
            || (!(changed_directory && matches!(record.kind, Kind::Directory))
                && (before.mtime(), before.mtime_nsec()) != record.mtime)
        {
            return Err(changed("Transferred permissions or timestamp differ"));
        }
        match &record.kind {
            Kind::File(hash) => {
                if before.len() != record.size || super::hash_file(&mut file, cancel)? != *hash {
                    return Err(changed("Transferred file content differs"));
                }
            }
            Kind::Sparse(witness) => {
                witness.verify(&file, cancel)?;
            }
            Kind::HardLink(target) => {
                let target_record = records
                    .get(target)?
                    .ok_or_else(|| changed("Hardlink target missing"))?;
                if skip(&target_record) {
                    return Err(changed("Hardlink target was excluded from verification"));
                }
                let (target_root, _) = location(target)?;
                let (target_directory, target_name) = parent(&target_root, target)?;
                let target_file = cap_primitives::fs::open(
                    &target_directory.file,
                    Path::new(&target_name),
                    &options,
                )?;
                let target_metadata = target_file.metadata()?;
                if !target_metadata.is_file()
                    || (target_metadata.dev(), target_metadata.ino())
                        != (before.dev(), before.ino())
                {
                    return Err(changed("Transferred hardlink identity differs"));
                }
                target_directory.validate()?;
            }
            _ => {}
        }
        verify_xattrs(&file, &record.xattrs)?;
        super::acl::verify(&file, &record.acls)?;
        let after = file.metadata()?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.uid() != after.uid()
            || before.gid() != after.gid()
            || (!(changed_directory && matches!(record.kind, Kind::Directory))
                && (before.len() != after.len()
                    || before.mtime() != after.mtime()
                    || before.mtime_nsec() != after.mtime_nsec()
                    || before.ctime() != after.ctime()
                    || before.ctime_nsec() != after.ctime_nsec()))
        {
            return Err(changed("Transferred file changed during verification"));
        }
        if private_root {
            durability::sync_file(&file)?;
        }
        directory.validate()?;
    }
    Ok(())
}
fn verify_xattrs(file: &File, xattrs: &[(Vec<u8>, Vec<u8>)]) -> io::Result<()> {
    for (name, value) in xattrs {
        let name =
            std::ffi::CString::new(name.as_slice()).map_err(|_| changed("Invalid xattr name"))?;
        let mut buffer = vec![0; value.len() + 1];
        // SAFETY: valid descriptor, C name and writable buffer; bounded allocation above.
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if n < 0 || buffer[..n as usize] != *value {
            return Err(changed("Transferred user xattr differs"));
        }
    }
    Ok(())
}
pub(super) fn verify_tree(
    root: &Arc<PinnedDirectory>,
    records: &Records,
    private_root: bool,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let inventory = SourceInventory::capture_home(root)?;
    verify_tree_inventory(root, records, private_root, &inventory, cancel)
}
pub(super) fn verify_source_tree(
    root: &Arc<PinnedDirectory>,
    records: &Records,
    inventory: &SourceInventory,
    cancel: &AtomicBool,
) -> io::Result<()> {
    inventory.validate(root)?;
    verify_tree_inventory(root, records, false, inventory, cancel)
}
fn verify_tree_inventory(
    root: &Arc<PinnedDirectory>,
    records: &Records,
    private_root: bool,
    inventory: &SourceInventory,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let excluded = inventory.excluded_paths();
    let mut count = 0u64;
    for record in records.iter() {
        let record = record?;
        if excluded.iter().any(|path| record.path.starts_with(path)) {
            return Err(changed(
                "Archive contains installer custody outside the reviewed user data",
            ));
        }
        if !matches!(record.kind, Kind::Directory) {
            count = count
                .checked_add(1)
                .ok_or_else(|| changed("Archive count overflow"))?;
        }
    }
    verify_root_acl(records, root, private_root)?;
    verify_records(records, private_root, cancel, |path| {
        Ok((Arc::clone(root), inventory.mutable_directory(path)))
    })?;
    let summary = inventory.summary()?;
    if summary.files + summary.links + summary.special_files != count
        || (private_root && summary.runtime_sockets != 0)
    {
        return Err(changed("Transferred tree has unexpected entries"));
    }
    fn walk(
        root: &Arc<PinnedDirectory>,
        relative: &Path,
        records: &Records,
        inventory: &SourceInventory,
        private_root: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        use cap_std::fs::Dir;
        let dir = Dir::from_std_file(durability::readable_directory(&root.file)?);
        for item in dir.entries()? {
            check_cancel(cancel)?;
            let item = item?;
            let name = item.file_name();
            let path = relative.join(&name);
            if inventory.excluded(&path)? {
                continue;
            }
            let metadata = super::super::nofollow::metadata_nofollow(&root.file, Path::new(&name))?;
            if !records.contains(&path)? {
                if private_root || !super::super::migration_review::runtime_socket(&metadata) {
                    return Err(changed("Transferred tree contains an unreviewed entry"));
                }
                // Only an original, individually reviewed runtime socket may be
                // omitted from the portable archive. New sockets are not consent.
                inventory.check(&path, &metadata)?;
            }
            if metadata.is_dir() && !metadata.is_symlink() {
                walk(
                    &root.open_child(&name, &metadata)?,
                    &path,
                    records,
                    inventory,
                    private_root,
                    cancel,
                )?;
            }
        }
        Ok(())
    }
    walk(
        root,
        Path::new(""),
        records,
        inventory,
        private_root,
        cancel,
    )?;
    root.validate()
}

#[cfg(test)]
mod pax_tests {
    use super::*;
    fn record(key: &str, value: &[u8]) -> Vec<u8> {
        let mut length = key.len() + value.len() + 4;
        loop {
            let actual = length.to_string().len() + 1 + key.len() + 1 + value.len() + 1;
            if actual == length {
                break;
            }
            length = actual;
        }
        let mut bytes = format!("{length} {key}=").into_bytes();
        bytes.extend(value);
        bytes.push(b'\n');
        bytes
    }
    #[test]
    fn pax_decoder_obeys_lengths_for_multiline_and_binary_values() {
        let acl = b"user::rwx\ngroup::r-x\nmask::r-x\nother::---";
        let mut bytes = record("SCHILY.acl.access", acl);
        bytes.extend(record(
            "SCHILY.xattr.user.binary",
            b"binary\0value\nwith newline",
        ));
        let mut actual = BTreeMap::new();
        decode_pax(&bytes, &mut actual).unwrap();
        assert_eq!(actual[b"SCHILY.acl.access".as_slice()], acl);
        assert_eq!(
            actual[b"SCHILY.xattr.user.binary".as_slice()],
            b"binary\0value\nwith newline"
        );
        assert_eq!(actual.len(), 2);
    }
    #[test]
    fn pax_decoder_refuses_truncation_bad_lengths_and_duplicate_keys() {
        let good = record("path", b"./document");
        let mut duplicate = good.clone();
        duplicate.extend(&good);
        let mut wrong = good.clone();
        wrong[0] = b'9';
        for bad in [
            good[..good.len() - 1].to_vec(),
            wrong,
            duplicate,
            b"0 path=x\n".to_vec(),
            b"999999999999999999999 path=x\n".to_vec(),
            b"12 path=x\n".to_vec(),
            b"9 a\nb=x\n".to_vec(),
        ] {
            assert!(decode_pax(&bad, &mut BTreeMap::new()).is_err());
        }
    }
}
