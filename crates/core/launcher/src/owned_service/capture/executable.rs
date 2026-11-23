//! Sealed executable admission and canonical portable sandbox command.
use super::{CaptureAdmission, sealed_shim, snapshot};
use std::{
    fs::File,
    io::{self, Read},
    os::{
        fd::RawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    process::Command,
};
pub(crate) const BWRAP_FD: RawFd = 47;
pub(crate) const LOADER_FD: RawFd = 48;
pub(super) fn pin(
    runtime: &Path,
    bundle: &runtime_bundle::BundleManifest,
    relative: &str,
) -> io::Result<File> {
    pin_file(runtime, bundle, relative, true)
}
pub(super) fn pin_file(
    runtime: &Path,
    bundle: &runtime_bundle::BundleManifest,
    relative: &str,
    executable: bool,
) -> io::Result<File> {
    let expected = bundle
        .files
        .iter()
        .find(|file| file.path == relative)
        .ok_or_else(|| io::Error::other("Signed runtime lacks its required executable"))?;
    let path = runtime.join(relative);
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    let before = file.metadata()?;
    if !before.is_file()
        || (executable && before.mode() & 0o111 == 0)
        || before.len() > 64 * 1024 * 1024
        || before.len() != expected.size
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if blake3::hash(&bytes).to_hex().as_str() != expected.hash
        || snapshot(&before) != snapshot(&after)
        || snapshot(&std::fs::symlink_metadata(path)?) != snapshot(&after)
    {
        return Err(io::Error::other("Signed migration executable changed"));
    }
    sealed_shim(&bytes)
}
impl CaptureAdmission {
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(format!("/proc/self/fd/{LOADER_FD}"));
        command
            .arg("--inhibit-cache")
            .arg("--library-path")
            .arg(self.runtime.join("lib"))
            .arg(format!("/proc/self/fd/{BWRAP_FD}"));
        command
    }
}

pub(super) fn check_closure(
    runtime: &Path,
    bundle: &runtime_bundle::BundleManifest,
    executable: &File,
) -> io::Result<()> {
    use std::{
        collections::{HashSet, VecDeque},
        os::fd::AsRawFd,
    };
    let initial = crate::loader_preflight::read_dynamic_section(Path::new(&format!(
        "/proc/self/fd/{}",
        executable.as_raw_fd()
    )))?
    .ok_or_else(|| io::Error::other("Portable sandbox ELF is unsupported"))?;
    let mut pending = VecDeque::from(initial.needed);
    let mut seen = HashSet::new();
    while let Some(name) = pending.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > 256
            || Path::new(&name).components().count() != 1
            || !matches!(
                Path::new(&name).components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(io::Error::other("Portable sandbox dependency is invalid"));
        }
        let relative = format!("lib/{name}");
        let expected = bundle
            .files
            .iter()
            .find(|file| file.path == relative)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "Signed portable sandbox dependency missing: {name}"
                ))
            })?;
        let path = runtime.join(relative);
        let mut file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let before = file.metadata()?;
        if !before.is_file() || before.len() > 64 * 1024 * 1024 || before.len() != expected.size {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if snapshot(&before) != snapshot(&file.metadata()?)
            || bytes.len() > 64 * 1024 * 1024
            || blake3::hash(&bytes).to_hex().as_str() != expected.hash
        {
            return Err(io::Error::other(
                "Signed portable sandbox dependency changed",
            ));
        }
        let dynamic = crate::loader_preflight::read_dynamic_section(Path::new(&format!(
            "/proc/self/fd/{}",
            file.as_raw_fd()
        )))?
        .ok_or_else(|| io::Error::other("Portable sandbox dependency ELF is unsupported"))?;
        pending.extend(dynamic.needed);
    }
    Ok(())
}
