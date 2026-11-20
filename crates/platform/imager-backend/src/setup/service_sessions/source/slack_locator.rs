//! Fixed native path and canonical Flatpak deployment; no executable launches.
use super::{SourceError, slack_linux::SlackApplication};
use crate::setup::flatpak_transfer::{NativeRunner, Runner, discover};
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
    sync::Arc,
};
struct Edge {
    parent: File,
    name: std::ffi::CString,
    identity: (u64, u64, u32, u32),
}
pub(super) struct PublicFile {
    pub file: File,
    edges: Vec<Edge>,
    facts: std::fs::Metadata,
}
fn denied() -> SourceError {
    SourceError::UnsupportedEncryption
}
fn safe(facts: &std::fs::Metadata) -> bool {
    let owner = rustix::process::getuid().as_raw();
    (facts.uid() == 0 || facts.uid() == owner)
        && (facts.mode() & 0o022 == 0
            || facts.is_dir() && facts.uid() == 0 && facts.mode() & 0o1000 != 0)
}
impl PublicFile {
    pub(super) fn open(path: &Path) -> Result<Self, SourceError> {
        if !path.is_absolute() {
            return Err(denied());
        }
        let mut current = File::open("/").map_err(|_| denied())?;
        let mut edges = Vec::new();
        let names: Vec<_> = path
            .components()
            .filter_map(|part| match part {
                Component::RootDir => None,
                Component::Normal(name) => Some(Ok(name)),
                _ => Some(Err(denied())),
            })
            .collect::<Result<_, _>>()?;
        if names.is_empty() || names.len() > 64 {
            return Err(denied());
        }
        for (index, name) in names.iter().enumerate() {
            if !safe(&current.metadata().map_err(|_| denied())?) {
                return Err(denied());
            }
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| denied())?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if index + 1 < names.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                };
            let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(denied());
            }
            let next = unsafe { File::from_raw_fd(fd) };
            let facts = next.metadata().map_err(|_| denied())?;
            if !safe(&facts) || index + 1 == names.len() && !facts.is_file() {
                return Err(denied());
            }
            edges.push(Edge {
                parent: current,
                name,
                identity: (facts.dev(), facts.ino(), facts.uid(), facts.mode()),
            });
            current = next;
        }
        let facts = current.metadata().map_err(|_| denied())?;
        let this = Self {
            file: current,
            edges,
            facts,
        };
        this.validate()?;
        Ok(this)
    }
    pub(super) fn validate(&self) -> Result<(), SourceError> {
        for edge in &self.edges {
            if !safe(&edge.parent.metadata().map_err(|_| denied())?) {
                return Err(denied());
            }
            let mut actual: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::fstatat(
                    edge.parent.as_raw_fd(),
                    edge.name.as_ptr(),
                    &raw mut actual,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
                || (actual.st_dev, actual.st_ino, actual.st_uid, actual.st_mode) != edge.identity
            {
                return Err(denied());
            }
        }
        let after = self.file.metadata().map_err(|_| denied())?;
        let before = &self.facts;
        if (
            after.dev(),
            after.ino(),
            after.uid(),
            after.mode(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) != (
            before.dev(),
            before.ino(),
            before.uid(),
            before.mode(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) {
            return Err(denied());
        }
        Ok(())
    }
}
/// Called only by migration-consented source discovery, never by presence reading.
/// The public package check grants no profile or keychain access.
pub fn native() -> Option<Arc<SlackApplication>> {
    ["/usr/lib/slack"]
        .into_iter()
        .find_map(|root| SlackApplication::at(Path::new(root)).ok())
}
pub fn flatpak(home: &Path) -> Option<Arc<SlackApplication>> {
    flatpak_with(&NativeRunner, home).ok()
}
fn flatpak_with(runner: &impl Runner, home: &Path) -> Result<Arc<SlackApplication>, SourceError> {
    let inventory = discover(runner, home).map_err(|_| denied())?;
    let selected: Vec<_> = inventory
        .refs
        .iter()
        .filter(|item| item.app_id() == Some("com.slack.Slack"))
        .collect();
    // Ambiguous installations cannot bind one profile to a unique application.
    if selected.len() != 1 {
        return Err(denied());
    }
    let item = selected[0];
    let args = [
        "info".into(),
        item.installation.argument(),
        "--show-location".into(),
        item.reference.clone(),
    ];
    let location = runner.run(home, &args).map_err(|_| denied())?;
    let path = Path::new(location.trim_end_matches('\n'));
    let parts: Vec<_> = path.components().collect();
    let reference: Vec<_> = item.reference.split('/').collect();
    if !path.is_absolute() || parts.len() < 5 {
        return Err(denied());
    }
    let suffix = &parts[parts.len() - 5..];
    for (part, expected) in suffix[..4].iter().zip(reference) {
        if part.as_os_str() != expected {
            return Err(denied());
        }
    }
    let commit = suffix[4].as_os_str().to_str().ok_or_else(denied)?;
    if commit.len() != 64
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(denied());
    }
    let proof = SlackApplication::at(&path.join("files/extra"))?;
    if runner.run(home, &args).map_err(|_| denied())? != location {
        return Err(denied());
    }
    proof.validate()?;
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn public_package_descriptor_refuses_links_replacement_and_foreign_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("public");
        std::fs::write(&path, b"public pinned bytes").unwrap();
        let held = PublicFile::open(&path).unwrap();
        held.validate().unwrap();
        std::fs::rename(&path, directory.path().join("old")).unwrap();
        std::fs::write(&path, b"public pinned bytes").unwrap();
        assert!(held.validate().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(PublicFile::open(&path).is_err());
        let link = directory.path().join("link");
        symlink(directory.path().join("old"), &link).unwrap();
        assert!(PublicFile::open(&link).is_err());
        let fifo = directory.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(PublicFile::open(&fifo).is_err());
    }
}
