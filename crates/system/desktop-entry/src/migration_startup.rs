//! Admission policy for autostart entries imported by home migration.
//!
//! The required marker is published before the record and imported names. A
//! required but unreadable policy refuses activation; an absent marker preserves
//! normal XDG behavior. All traversal is relative to pinned directory handles.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const REQUIRED_NAME: &str = "migration-startup-required";
pub const RECORD_NAME: &str = "migration-startup.json";
pub const REQUIRED_PATH: &str = ".config/tilo/migration-startup-required";
pub const RECORD_PATH: &str = ".config/tilo/migration-startup.json";
mod native;

const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// Durable marker binding the required policy to one migration operation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Required {
    pub schema: u32,
    pub operation: String,
}

/// The exact imported entries; collisions left in place are never included.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub operation: String,
    pub entries: Vec<Entry>,
}

/// Enforce the existing installed consumer's bounded startup policy while
/// collecting, rather than materializing arbitrary imported startup entries.
pub struct RecordBuilder {
    record: Record,
    bytes: usize,
    paths: BTreeSet<Vec<u8>>,
}
impl RecordBuilder {
    /// Start a bounded policy record for one migration operation.
    pub fn new(operation: String) -> io::Result<Self> {
        if !valid_operation(&operation) {
            return Err(invalid("invalid migration startup operation"));
        }
        let record = Record {
            schema: 1,
            operation,
            entries: Vec::new(),
        };
        let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?.len();
        Ok(Self {
            record,
            bytes,
            paths: BTreeSet::new(),
        })
    }
    /// Add an imported identity within the consumer's count and byte limits.
    pub fn push(&mut self, entry: Entry) -> io::Result<()> {
        if self.record.entries.len() >= MAX_ENTRIES {
            return Err(invalid("migration startup entries exceed consumer policy"));
        }
        if entry.relative.len() > MAX_ENTRY_BYTES {
            return Err(invalid("migration startup path exceeds bounds"));
        }
        let mut encoded = EncodedSize(0);
        serde_json::to_writer(&mut encoded, &entry).map_err(io::Error::other)?;
        let encoded = encoded.0;
        let additional = encoded
            .checked_add(usize::from(!self.record.entries.is_empty()))
            .ok_or_else(|| invalid("migration startup record overflow"))?;
        let bytes = self
            .bytes
            .checked_add(additional)
            .filter(|n| *n <= MAX_RECORD_BYTES)
            .ok_or_else(|| invalid("migration startup record exceeds consumer policy"))?;
        if !self.paths.insert(entry.relative.clone()) {
            return Err(invalid("duplicate imported autostart path"));
        }
        self.record.entries.push(entry);
        self.bytes = bytes;
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.record.entries.is_empty()
    }
    /// Validate the collected imported identities.
    ///
    /// # Errors
    /// Refuses malformed paths, duplicate entries, or an invalid operation or schema.
    pub fn finish(self) -> io::Result<Record> {
        self.record.validate()?;
        Ok(self.record)
    }
}

struct EncodedSize(usize);
impl Write for EncodedSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_RECORD_BYTES)
            .ok_or_else(|| invalid("migration startup record exceeds consumer policy"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stable identity survives rename and reboot. Links hash their literal target.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub relative: Vec<u8>,
    pub inode: u64,
    pub uid: u32,
    pub mode: u32,
    pub len: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub digest: [u8; 32],
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_operation(operation: &str) -> bool {
    !operation.is_empty() && operation.len() <= 256 && !operation.chars().any(char::is_control)
}

fn boundary_path(path: &Path) -> bool {
    matches!(
        path.as_os_str().as_bytes(),
        b".config" | b".config/autostart"
    )
}

fn entry_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(value)) if value == ".config")
        && matches!(components.next(), Some(Component::Normal(value)) if value == "autostart")
        && matches!(components.next(), Some(Component::Normal(value)) if value.as_bytes().ends_with(b".desktop"))
        && components.next().is_none()
        && path.as_os_str().as_bytes()
            == path
                .components()
                .collect::<PathBuf>()
                .as_os_str()
                .as_bytes()
}

fn open_at(parent: &File, name: &Path, flags: i32) -> io::Result<File> {
    let name = CString::new(name.as_os_str().as_bytes()).map_err(|_| invalid("NUL in path"))?;
    // SAFETY: the parent fd and NUL-terminated name remain valid during openat.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn directory(parent: &File, name: &str) -> io::Result<File> {
    open_at(parent, Path::new(name), libc::O_RDONLY | libc::O_DIRECTORY)
}

impl Entry {
    /// Capture one native immediate autostart name without following any links.
    ///
    /// # Errors
    /// Refuses unsafe traversal, unsupported types, oversized content, or entries
    /// that change during capture; also propagates filesystem errors.
    pub fn capture(root: &File, relative: &Path) -> io::Result<Option<Self>> {
        if !entry_path(relative) && !boundary_path(relative) {
            return Ok(None);
        }
        let result = Self::capture_existing(root, relative);
        match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            other => other,
        }
    }

    fn capture_existing(root: &File, relative: &Path) -> io::Result<Option<Self>> {
        let config;
        let autostart;
        let parent = if relative == Path::new(".config") {
            root
        } else {
            config = directory(root, ".config")?;
            if relative == Path::new(".config/autostart") {
                &config
            } else {
                autostart = directory(&config, "autostart")?;
                &autostart
            }
        };
        let name = relative
            .file_name()
            .ok_or_else(|| invalid("missing entry name"))?;
        let pin = open_at(parent, Path::new(name), libc::O_PATH)?;
        let metadata = pin.metadata()?;
        if boundary_path(relative) && !metadata.file_type().is_symlink() {
            return Ok(None);
        }
        if metadata.is_dir() {
            return Ok(None);
        }
        let digest = if metadata.file_type().is_symlink() {
            let bytes = native::read_pinned_link(&pin, MAX_ENTRY_BYTES)?;
            *blake3::hash(&bytes).as_bytes()
        } else if metadata.is_file() {
            let file = open_at(parent, Path::new(name), libc::O_RDONLY | libc::O_NONBLOCK)?;
            if file.metadata()?.ino() != metadata.ino() {
                return Err(invalid("autostart entry replaced during capture"));
            }
            let bytes = bounded(file.try_clone()?, MAX_ENTRY_BYTES)?;
            let after = file.metadata()?;
            if metadata.ino() != after.ino()
                || metadata.len() != after.len()
                || metadata.mtime() != after.mtime()
                || metadata.mtime_nsec() != after.mtime_nsec()
                || metadata.mode() != after.mode()
                || metadata.uid() != after.uid()
            {
                return Err(invalid("autostart entry changed during capture"));
            }
            *blake3::hash(&bytes).as_bytes()
        } else {
            return Err(invalid("unsupported autostart entry type"));
        };
        Ok(Some(Self {
            relative: relative.as_os_str().as_bytes().to_vec(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode(),
            len: metadata.len(),
            mtime_sec: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            digest,
        }))
    }
}

impl Record {
    fn validate(&self) -> io::Result<()> {
        if self.schema != 1 || !valid_operation(&self.operation) || self.entries.len() > MAX_ENTRIES
        {
            return Err(invalid("invalid migration startup record"));
        }
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            let path = Path::new(std::ffi::OsStr::from_bytes(&entry.relative));
            if !(entry_path(path)
                || (boundary_path(path) && entry.mode & libc::S_IFMT == libc::S_IFLNK))
                || !paths.insert(&entry.relative)
            {
                return Err(invalid("invalid or duplicate imported autostart path"));
            }
        }
        Ok(())
    }

    /// Refuse exactly a recorded imported entry; identity changes fail closed.
    /// Absolute names must lie under the pinned home directory.
    pub fn blocks(&self, root: &File, path: &Path) -> io::Result<bool> {
        self.validate()?;
        let home;
        let relative = if path.is_absolute() {
            home = std::fs::read_link(format!("/proc/self/fd/{}", root.as_raw_fd()))?;
            match path.strip_prefix(&home) {
                Ok(relative) => relative,
                Err(_) => return Ok(false),
            }
        } else {
            path
        };
        let Some(expected) = self.entries.iter().find(|entry| {
            let recorded = Path::new(std::ffi::OsStr::from_bytes(&entry.relative));
            entry.relative == relative.as_os_str().as_bytes()
                || (boundary_path(recorded)
                    && entry_path(relative)
                    && relative.starts_with(recorded))
        }) else {
            return Ok(false);
        };
        let recorded = Path::new(std::ffi::OsStr::from_bytes(&expected.relative));
        match Entry::capture(root, recorded)? {
            Some(actual) if actual == *expected => Ok(true),
            _ => Err(invalid("imported autostart entry changed or disappeared")),
        }
    }
}

fn bounded(file: File, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let read_limit = limit
        .checked_add(1)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| invalid("migration startup read limit overflow"))?;
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid("migration startup data exceeds limit"));
    }
    Ok(bytes)
}

fn owned_private(file: &File, private: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    let uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != uid || metadata.mode() & if private { 0o077 } else { 0o022 } != 0 {
        return Err(invalid(
            "migration startup policy has foreign or public custody",
        ));
    }
    Ok(())
}

fn policy_file(parent: &File, name: &str) -> io::Result<File> {
    let file = open_at(parent, Path::new(name), libc::O_RDONLY | libc::O_NONBLOCK)?;
    owned_private(&file, true)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("migration policy is not a regular file"));
    }
    Ok(file)
}

fn home_file(home: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home)
}

fn read_root(root: &File) -> io::Result<Option<Record>> {
    let config = match directory(root, ".config") {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let tilo = match directory(&config, "tilo") {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let marker = match policy_file(&tilo, REQUIRED_NAME) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match policy_file(&tilo, RECORD_NAME) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
                Ok(_) => return Err(invalid("migration startup record has no required marker")),
            }
        }
        result => result?,
    };
    owned_private(root, false)?;
    owned_private(&config, false)?;
    owned_private(&tilo, true)?;
    let required: Required = serde_json::from_slice(&bounded(marker, 1024)?)
        .map_err(|_| invalid("malformed required migration marker"))?;
    if required.schema != 1 || !valid_operation(&required.operation) {
        return Err(invalid("invalid required migration marker"));
    }
    let record: Record = serde_json::from_slice(&bounded(
        policy_file(&tilo, RECORD_NAME)?,
        MAX_RECORD_BYTES,
    )?)
    .map_err(|_| invalid("malformed migration startup record"))?;
    record.validate()?;
    if record.operation != required.operation {
        return Err(invalid("migration startup operation mismatch"));
    }
    Ok(Some(record))
}

/// Read the canonical target-user policy. A required policy must be complete.
///
/// # Errors
/// Refuses incomplete, malformed, oversized, or insecure policy files, operation
/// mismatches, and unsafe home traversal; propagates filesystem errors.
pub fn read(home: &Path) -> io::Result<Option<Record>> {
    read_root(&home_file(home)?)
}

/// Return permitted input names; a required policy error refuses the whole set.
///
/// # Errors
/// Returns an error if the required policy cannot be read securely or any
/// protected identity changed or disappeared, even if absent from the input.
pub fn filter(home: &Path, paths: &[PathBuf]) -> io::Result<BTreeSet<PathBuf>> {
    let root = home_file(home)?;
    let Some(record) = read_root(&root)? else {
        return Ok(paths.iter().cloned().collect());
    };
    // A scanner may omit unreadable or changed imports. Their absence from its
    // output is not evidence that the protected policy still describes reality.
    for entry in &record.entries {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&entry.relative));
        if !record.blocks(&root, relative)? {
            return Err(invalid("recorded migration startup entry is not protected"));
        }
    }
    let mut permitted = BTreeSet::new();
    for path in paths {
        // The caller selects the home namespace. /home may be an OS alias for
        // /var/home, whereas the pinned descriptor resolves to /var/home.
        let checked = path.strip_prefix(home).unwrap_or(path);
        if !record.blocks(&root, checked)? {
            permitted.insert(path.clone());
        }
    }
    Ok(permitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const OPERATION: &str = "fixture";

    fn write_private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn prepare(home: &Path) {
        std::fs::create_dir_all(home.join(".config/autostart")).unwrap();
        std::fs::create_dir(home.join(".config/tilo")).unwrap();
        std::fs::set_permissions(
            home.join(".config/tilo"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    fn publish(home: &Path, record: &Record) {
        write_private(
            &home.join(REQUIRED_PATH),
            &serde_json::to_vec(&Required {
                schema: 1,
                operation: OPERATION.into(),
            })
            .unwrap(),
        );
        write_private(
            &home.join(RECORD_PATH),
            &serde_json::to_vec(record).unwrap(),
        );
    }

    #[test]
    fn startup_collection_preserves_real_consumer_wire_and_refuses_before_overflow() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        prepare(source.path());
        prepare(target.path());
        let relative = Path::new(".config/autostart/import.desktop");
        std::fs::write(
            source.path().join(relative),
            b"[Desktop Entry]\nExec=imported\n",
        )
        .unwrap();
        let entry = Entry::capture(&home_file(source.path()).unwrap(), relative)
            .unwrap()
            .unwrap();
        let mut builder = RecordBuilder::new(OPERATION.into()).unwrap();
        builder.push(entry.clone()).unwrap();
        assert!(builder.push(entry.clone()).is_err());
        let record = builder.finish().unwrap();
        assert_eq!(record.entries, vec![entry.clone()]);
        publish(target.path(), &record);
        std::fs::rename(source.path().join(relative), target.path().join(relative)).unwrap();
        assert_eq!(read(target.path()).unwrap().unwrap().entries, vec![entry]);
        assert!(
            filter(target.path(), &[target.path().join(relative)])
                .unwrap()
                .is_empty()
        );
        let mut builder = RecordBuilder::new(OPERATION.into()).unwrap();
        // Distinct actual captured paths exceed the existing 4096-entry policy;
        // refusal leaves the accepted record intact and independently valid.
        for number in 0..=MAX_ENTRIES {
            let relative = PathBuf::from(format!(".config/autostart/item-{number}.desktop"));
            std::fs::write(
                source.path().join(&relative),
                b"[Desktop Entry]\nExec=imported\n",
            )
            .unwrap();
            let entry = Entry::capture(&home_file(source.path()).unwrap(), &relative)
                .unwrap()
                .unwrap();
            if number < MAX_ENTRIES {
                builder.push(entry).unwrap();
            } else {
                assert!(builder.push(entry).is_err());
            }
        }
        assert_eq!(builder.finish().unwrap().entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn renamed_import_is_blocked_and_unrelated_entries_survive() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        prepare(source.path());
        prepare(target.path());
        let name = Path::new(".config/autostart/import.desktop");
        std::fs::write(
            source.path().join(name),
            b"[Desktop Entry]\nExec=imported\n",
        )
        .unwrap();
        let entry = Entry::capture(&home_file(source.path()).unwrap(), name)
            .unwrap()
            .unwrap();
        let record = Record {
            schema: 1,
            operation: OPERATION.into(),
            entries: vec![entry],
        };
        publish(target.path(), &record);
        std::fs::rename(source.path().join(name), target.path().join(name)).unwrap();
        let unrelated = target.path().join(".config/autostart/existing.desktop");
        std::fs::write(&unrelated, b"existing").unwrap();
        let imported = target.path().join(name);
        let system = PathBuf::from("/etc/xdg/autostart/system.desktop");
        assert_eq!(
            filter(
                target.path(),
                &[imported.clone(), unrelated.clone(), system.clone()]
            )
            .unwrap(),
            BTreeSet::from([unrelated, system])
        );
        assert!(
            read(target.path())
                .unwrap()
                .unwrap()
                .blocks(&home_file(target.path()).unwrap(), name)
                .unwrap()
        );
        std::fs::write(imported, b"changed").unwrap();
        assert!(filter(target.path(), &[name.to_path_buf()]).is_err());
        assert!(filter(target.path(), &[]).is_err());
    }

    #[test]
    fn caller_home_namespace_alias_remains_protected() {
        let directory = tempfile::tempdir().unwrap();
        let physical = directory.path().join("physical/home");
        std::fs::create_dir_all(&physical).unwrap();
        prepare(&physical);
        symlink(
            directory.path().join("physical"),
            directory.path().join("alias"),
        )
        .unwrap();
        let logical = directory.path().join("alias/home");
        let name = Path::new(".config/autostart/import.desktop");
        std::fs::write(physical.join(name), b"imported").unwrap();
        let entry = Entry::capture(&home_file(&physical).unwrap(), name)
            .unwrap()
            .unwrap();
        publish(
            &physical,
            &Record {
                schema: 1,
                operation: OPERATION.into(),
                entries: vec![entry],
            },
        );
        assert!(filter(&logical, &[logical.join(name)]).unwrap().is_empty());
    }

    #[test]
    fn required_policy_missing_malformed_public_and_foreign_operation_refuse() {
        let home = tempfile::tempdir().unwrap();
        prepare(home.path());
        assert!(read(home.path()).unwrap().is_none());
        let record = Record {
            schema: 1,
            operation: OPERATION.into(),
            entries: vec![],
        };
        publish(home.path(), &record);
        std::fs::remove_file(home.path().join(REQUIRED_PATH)).unwrap();
        assert!(read(home.path()).is_err());
        assert!(filter(home.path(), &[]).is_err());
        publish(home.path(), &record);
        std::fs::remove_file(home.path().join(RECORD_PATH)).unwrap();
        assert!(filter(home.path(), &[]).is_err());
        write_private(&home.path().join(RECORD_PATH), b"{");
        assert!(read(home.path()).is_err());
        publish(home.path(), &record);
        std::fs::set_permissions(
            home.path().join(RECORD_PATH),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(read(home.path()).is_err());
        publish(
            home.path(),
            &Record {
                operation: "different".into(),
                ..record
            },
        );
        assert!(read(home.path()).is_err());
    }

    #[test]
    fn literal_links_and_imported_boundary_are_blocked_without_following() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        prepare(source.path());
        prepare(target.path());
        let name = Path::new(".config/autostart/link.desktop");
        symlink("/missing/entry.desktop", source.path().join(name)).unwrap();
        let entry = Entry::capture(&home_file(source.path()).unwrap(), name)
            .unwrap()
            .unwrap();
        publish(
            target.path(),
            &Record {
                schema: 1,
                operation: OPERATION.into(),
                entries: vec![entry],
            },
        );
        std::fs::rename(source.path().join(name), target.path().join(name)).unwrap();
        assert!(
            filter(target.path(), &[name.to_path_buf()])
                .unwrap()
                .is_empty()
        );
        std::fs::remove_file(target.path().join(name)).unwrap();
        symlink("/changed", target.path().join(name)).unwrap();
        assert!(filter(target.path(), &[name.to_path_buf()]).is_err());

        std::fs::remove_dir(source.path().join(".config/autostart")).unwrap();
        symlink("/outside", source.path().join(".config/autostart")).unwrap();
        let boundary = Entry::capture(
            &home_file(source.path()).unwrap(),
            Path::new(".config/autostart"),
        )
        .unwrap()
        .unwrap();
        publish(
            target.path(),
            &Record {
                schema: 1,
                operation: OPERATION.into(),
                entries: vec![boundary],
            },
        );
        std::fs::remove_file(target.path().join(name)).unwrap();
        std::fs::remove_dir(target.path().join(".config/autostart")).unwrap();
        std::fs::rename(
            source.path().join(".config/autostart"),
            target.path().join(".config/autostart"),
        )
        .unwrap();
        assert!(
            filter(target.path(), &[name.to_path_buf()])
                .unwrap()
                .is_empty()
        );
    }
}
