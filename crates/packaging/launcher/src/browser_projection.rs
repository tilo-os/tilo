//! Admit only the authenticated helper's bounded browser subset, never a home.
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

use browser_continuity::{MAX_PLAN_BYTES, PLAN_FILE, PROFILES_DIRECTORY, Plan};

use crate::manifest::ManifestView;
use crate::run::{LaunchMode, RunError, RunOptions};

pub(crate) fn eligible(manifest: &ManifestView, options: &RunOptions) -> bool {
    manifest.package_id == "so.tilo.browser"
        && matches!(options.mode, LaunchMode::Gui)
        && options.browser_continuity_host_path.is_some()
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid browser continuity projection",
    )
}
fn uid() -> u32 {
    // SAFETY: getuid has no pointer arguments or preconditions.
    unsafe { libc::getuid() }
}

fn child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(name.as_bytes()).map_err(|_| invalid())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent and name remain live for openat; owned successful fd below.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor solely owned by this File.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn private(file: &File, directory: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != uid()
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(invalid());
    }
    Ok(())
}

/// Walk every component without following a link, retaining the final directory.
fn held_root(path: &Path, home: &Path) -> io::Result<File> {
    let base = home.join(".local/state/tilo/browser-continuity");
    let tail = path.strip_prefix(&base).map_err(|_| invalid())?;
    let parts: Vec<_> = tail.components().collect();
    let [
        Component::Normal(operation),
        Component::Normal(container),
        Component::Normal(import),
        Component::Normal(data),
    ] = parts.as_slice()
    else {
        return Err(invalid());
    };
    let operation = operation.to_str().ok_or_else(invalid)?;
    if uuid::Uuid::parse_str(operation)
        .map_err(|_| invalid())?
        .to_string()
        != operation
        || *import != "import"
        || *data != "home"
    {
        return Err(invalid());
    }
    let container = container.to_str().ok_or_else(invalid)?;
    let suffix = container
        .strip_prefix("browser-import-")
        .ok_or_else(invalid)?;
    if suffix.is_empty()
        || suffix.len() > 128
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid());
    }
    if !home.is_absolute() || !path.is_absolute() {
        return Err(invalid());
    }
    let mut held = File::open("/")?;
    let mut traversed = std::path::PathBuf::from("/");
    for part in path.components().skip(1) {
        let Component::Normal(name) = part else {
            return Err(invalid());
        };
        held = child(&held, name, true)?;
        traversed.push(name);
        let metadata = held.metadata()?;
        if metadata.uid() != 0 && metadata.uid() != uid() {
            return Err(invalid());
        }
        // Root-owned sticky ancestors permit private temporary test homes, but
        // no grant subtree or user-controlled ancestor may be writable by others.
        if metadata.mode() & 0o022 != 0
            && !(metadata.uid() == 0
                && metadata.mode() & libc::S_ISVTX != 0
                && !traversed.starts_with(home))
        {
            return Err(invalid());
        }
        if traversed.starts_with(&base) {
            private(&held, true)?;
        }
    }
    Ok(held)
}

fn names(directory: &File) -> io::Result<Vec<std::ffi::OsString>> {
    // This proc path identifies the held descriptor, not a mutable source path.
    fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}
fn exact_children(directory: &File, expected: &[&str]) -> io::Result<()> {
    let actual = names(directory)?;
    if actual.len() != expected.len()
        || actual
            .iter()
            .any(|name| !expected.iter().any(|expected| name == *expected))
    {
        return Err(invalid());
    }
    Ok(())
}
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}
fn mkdir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn materialize(path: &Path, home: &Path) -> io::Result<tempfile::TempDir> {
    let root = held_root(path, home)?;
    exact_children(&root, &[PLAN_FILE, PROFILES_DIRECTORY])?;
    let plan_file = child(&root, PLAN_FILE.as_ref(), false)?;
    private(&plan_file, false)?;
    let mut bytes = Vec::new();
    plan_file
        .take(MAX_PLAN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let plan = Plan::decode(&bytes).map_err(|_| invalid())?;
    if plan.profiles.is_empty() {
        return Err(invalid());
    }
    let profiles = child(&root, PROFILES_DIRECTORY.as_ref(), true)?;
    private(&profiles, true)?;
    let ids: Vec<_> = plan
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect();
    exact_children(&profiles, &ids)?;
    let output = tempfile::Builder::new()
        .prefix("tilo-browser-projection-")
        .tempdir()?;
    fs::set_permissions(output.path(), fs::Permissions::from_mode(0o700))?;
    write_private(
        &output.path().join(PLAN_FILE),
        &plan.encode().map_err(|_| invalid())?,
    )?;
    mkdir(&output.path().join(PROFILES_DIRECTORY))?;
    let mut total = 0u64;
    for profile in &plan.profiles {
        let source = child(&profiles, profile.id.as_ref(), true)?;
        private(&source, true)?;
        for name in names(&source)? {
            if !profile
                .allowed_files()
                .iter()
                .any(|allowed| name == *allowed)
            {
                return Err(invalid());
            }
            private(&child(&source, &name, false)?, false)?;
        }
        let snapshot = browser_continuity::snapshot::capture(source, profile)?;
        total = total.checked_add(snapshot.bytes()).ok_or_else(invalid)?;
        if total > browser_continuity::snapshot::MAX_SNAPSHOT_BYTES {
            return Err(invalid());
        }
        let target = output.path().join(PROFILES_DIRECTORY).join(&profile.id);
        mkdir(&target)?;
        for file in snapshot.files() {
            write_private(&target.join(file.name()), file.bytes())?;
        }
    }
    Ok(output)
}

pub(crate) fn splice(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    options: &RunOptions,
) -> Result<Option<tempfile::TempDir>, RunError> {
    if !eligible(manifest, options) {
        return Ok(None);
    }
    if matches!(options.identity, crate::run::IdentitySource::None) {
        return Err(invalid().into());
    }
    let home = std::env::var_os("HOME").ok_or_else(invalid)?;
    let path = options
        .browser_continuity_host_path
        .as_deref()
        .ok_or_else(invalid)?;
    let projection = materialize(path, Path::new(&home))?;
    let entry = argv.pop().ok_or_else(invalid)?;
    argv.extend([
        "--ro-bind".into(),
        projection.path().to_string_lossy().into_owned(),
        browser_continuity::PROJECTION_DIRECTORY.into(),
        entry,
    ]);
    Ok(Some(projection))
}

#[cfg(test)]
mod tests;
