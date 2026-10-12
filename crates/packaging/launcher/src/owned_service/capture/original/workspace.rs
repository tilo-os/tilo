//! Fresh protected working ancestry; only new fixed worker leaves delegated.
use super::{Facts, refused, root};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
const APP: &str = "tilo-rooms/personal/so.tilo.imager.home-export";
const PERSON: &str = "tilo-person/so.tilo.imager.home-export";
fn open(parent: &File, name: &std::ffi::CStr) -> io::Result<File> {
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}
fn child(parent: &File, name: &std::ffi::CStr) -> io::Result<File> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
    }
    open(parent, name)
}
fn controlled(path: &Path) -> io::Result<Vec<File>> {
    if !path.is_absolute() {
        return Err(refused("Worker root must be absolute"));
    }
    let root = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    let mut chain = vec![root];
    for component in path.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(refused("Worker ancestry is not canonical"));
        };
        let parent = chain.last().ok_or(io::ErrorKind::InvalidData)?;
        StorageDirectory::from_file(parent.try_clone()?)?.validate_namespace_control()?;
        let name = std::ffi::CString::new(name.as_encoded_bytes()).map_err(io::Error::other)?;
        chain.push(open(parent, &name)?);
    }
    let leaf = chain.last().ok_or(io::ErrorKind::InvalidData)?;
    let storage = StorageDirectory::from_file(leaf.try_clone()?)?;
    storage.validate_namespace_control()?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    Ok(chain)
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
fn delegate(directory: &File, facts: &Facts) -> io::Result<()> {
    if unsafe { libc::fchown(directory.as_raw_fd(), facts.uid, facts.gid) } < 0
        || unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != facts.uid
        || metadata.gid() != facts.gid
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(refused("Fresh worker leaf delegation failed"));
    }
    Ok(())
}
pub(super) struct Workspace {
    path: PathBuf,
    chain: Vec<File>,
    app: File,
    person: File,
}
impl Workspace {
    pub(super) fn check(&self) -> io::Result<()> {
        root()?;
        let current = controlled(&self.path)?;
        if current.len() != self.chain.len() {
            return Err(refused("Worker ancestry changed"));
        }
        for (expected, actual) in self.chain.iter().zip(&current) {
            StorageDirectory::from_file(expected.try_clone()?)?.validate_namespace_control()?;
            if identity(expected)? != identity(actual)? {
                return Err(refused("Worker ancestor was replaced"));
            }
        }
        let held = self.chain.last().ok_or(io::ErrorKind::InvalidData)?;
        for (relative, expected) in [(APP, &self.app), (PERSON, &self.person)] {
            let name = std::ffi::CString::new(relative).map_err(io::Error::other)?;
            if identity(&open(held, &name)?)? != identity(expected)? {
                return Err(refused("Worker projection was replaced"));
            }
        }
        Ok(())
    }
}
pub(super) fn prepare(path: &Path, facts: &Facts) -> io::Result<Workspace> {
    root()?;
    facts.validate()?;
    let chain = controlled(path)?;
    let held = chain.last().ok_or(io::ErrorKind::InvalidData)?;
    if std::fs::read_dir(path)?.next().transpose()?.is_some() {
        return Err(refused("Original reader work root must be fresh"));
    }
    let rooms = child(held, c"tilo-rooms")?;
    let room = child(&rooms, c"personal")?;
    let app = child(&room, c"so.tilo.imager.home-export")?;
    let local = child(&app, c".local")?;
    for directory in [
        &app,
        &local,
        &child(&local, c"share")?,
        &child(&local, c"state")?,
        &child(&app, c".config")?,
        &child(&app, c".cache")?,
    ] {
        delegate(directory, facts)?;
    }
    let people = child(held, c"tilo-person")?;
    let person = child(&people, c"so.tilo.imager.home-export")?;
    delegate(&person, facts)?;
    let result = Workspace {
        path: path.into(),
        chain,
        app,
        person,
    };
    result.check()?;
    Ok(result)
}
