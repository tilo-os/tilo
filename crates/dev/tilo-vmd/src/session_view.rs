//! One readonly overlay per stand-in session, independent of the host sysext.
//! A nonrecursive bind of the boot root retains its underlying /usr rather
//! than the overmount installed by systemd-sysext. Slot services bind the
//! combined view; they never stack a third overlay over the host's /usr.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::error::DaemonError;

fn io(error: std::io::Error) -> DaemonError {
    DaemonError::io("prepare the readonly slot filesystem view")(error)
}

fn text(path: &Path) -> Result<CString, DaemonError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| DaemonError::InvalidParams("invalid slot view path".into()))
}

fn mount(
    source: Option<&Path>,
    target: &Path,
    kind: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<(), DaemonError> {
    let source = source.map(text).transpose()?;
    let target = text(target)?;
    let string = |value: &str| {
        CString::new(value).map_err(|_| io(std::io::Error::other("invalid mount option")))
    };
    let kind = kind.map(string).transpose()?;
    let data = data.map(string).transpose()?;
    // SAFETY: All strings stay alive until mount returns; pointers are NUL
    // terminated or null. None are interpreted as shell text.
    let result = unsafe {
        libc::mount(
            source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            target.as_ptr(),
            kind.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            flags,
            data.as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
        )
    };
    if result != 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn mounted(path: &Path) -> Result<bool, DaemonError> {
    let path = path
        .to_str()
        .ok_or_else(|| io(std::io::Error::other("non-UTF8 mount path")))?;
    Ok(fs::read_to_string("/proc/self/mountinfo")
        .map_err(io)?
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(path)))
}

fn unmount(path: &Path) -> Result<(), DaemonError> {
    if !mounted(path)? {
        return Ok(());
    }
    let path = text(path)?;
    // SAFETY: The validated NUL-terminated path remains alive through umount.
    if unsafe { libc::umount2(path.as_ptr(), 0) } != 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn directory(path: &Path) -> Result<(), DaemonError> {
    if !path.exists() {
        fs::create_dir(path).map_err(io)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io)?;
    }
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(DaemonError::InvalidParams(
            "slot view directory is not root controlled".into(),
        ));
    }
    if path
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || b":,\\".contains(byte))
    {
        return Err(DaemonError::InvalidParams(
            "unsafe slot view mount path".into(),
        ));
    }
    Ok(())
}

fn readonly(path: &Path) -> Result<bool, DaemonError> {
    let path = text(path)?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: statvfs initializes the pointed-to structure on success, and
    // the NUL-terminated path remains alive throughout the call.
    if unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    // SAFETY: the successful statvfs call initialized status.
    Ok(unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY != 0)
}

fn isolate_base(base: &Path) -> Result<(), DaemonError> {
    let mut children = Vec::new();
    for line in fs::read_to_string("/proc/self/mountinfo")
        .map_err(io)?
        .lines()
    {
        let Some(path) = line.split_whitespace().nth(4) else {
            return Err(DaemonError::InvalidParams("invalid mount table".into()));
        };
        let path = Path::new(path);
        if path != base && path.starts_with(base) {
            // Kernel mountinfo escapes whitespace and backslashes. An
            // unexpected escaped descendant is refused, never interpreted
            // as a different mountpoint or handed to a shell.
            if path.as_os_str().as_bytes().contains(&b'\\') || children.len() >= 512 {
                return Err(DaemonError::InvalidParams(
                    "unexpected retained-base mount hierarchy".into(),
                ));
            }
            children.push(path.to_path_buf());
        }
    }
    // A bind of a shared boot root inherits its propagation group. Disconnect
    // every retained child BEFORE unmounting it: otherwise repair would also
    // unmount the host's live /usr. Fresh bases must never receive later
    // systemd-sysext refresh mounts.
    mount(None, base, None, libc::MS_PRIVATE | libc::MS_REC, None)?;
    children.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for child in children {
        unmount(&child)?;
    }
    Ok(())
}

fn base(root: &Path, source: &Path) -> Result<PathBuf, DaemonError> {
    let base = root.join("base");
    directory(&base)?;
    if !mounted(&base)? {
        mount(Some(source), &base, None, libc::MS_BIND, None)?;
        let isolated = mount(None, &base, None, libc::MS_PRIVATE | libc::MS_REC, None);
        if let Err(error) = isolated.and_then(|()| {
            mount(
                None,
                &base,
                None,
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NODEV,
                None,
            )
        }) {
            unmount(&base)?;
            return Err(error);
        }
    }
    let expected = fs::metadata(source).map_err(io)?;
    let actual = fs::metadata(&base).map_err(io)?;
    if expected.dev() != actual.dev() || expected.ino() != actual.ino() || !readonly(&base)? {
        return Err(DaemonError::InvalidParams(
            "the retained boot-root mount has an unexpected origin or is mutable".into(),
        ));
    }
    isolate_base(&base)?;
    // A separate /usr filesystem not present in the boot-root backing tree
    // cannot be silently replaced by an empty directory.
    if !base.join("usr/lib/os-release").is_file() {
        return Err(DaemonError::InvalidParams(
            "the immutable boot root does not contain the base /usr".into(),
        ));
    }
    Ok(base)
}

fn hierarchy(base: &Path, view: &Path, layers: &[PathBuf], name: &str) -> Result<(), DaemonError> {
    let target = view.join(name);
    // The mounted root presents imported permissions, not the protected
    // backing directory. Release that old view before validating the backing
    // mountpoint; running session bind mounts retain their old view.
    unmount(&target)?;
    directory(&target)?;
    let mut lowers = Vec::new();
    for layer in layers.iter().rev() {
        let source = layer.join(name);
        if source.is_dir() {
            let canonical = source.canonicalize().map_err(io)?;
            if !canonical.starts_with(layer.canonicalize().map_err(io)?) {
                return Err(DaemonError::InvalidParams(
                    "a layer hierarchy escapes its imported root".into(),
                ));
            }
            let metadata = fs::symlink_metadata(layer).map_err(io)?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Err(DaemonError::InvalidParams(
                    "a view layer is not root controlled".into(),
                ));
            }
            if canonical
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|byte| byte.is_ascii_whitespace() || b":,\\".contains(byte))
            {
                return Err(DaemonError::InvalidParams("unsafe layer mount path".into()));
            }
            lowers.push(canonical);
        }
    }
    let base = base.join(name);
    if !base.is_dir() {
        return Err(DaemonError::InvalidParams(
            "a base hierarchy is unavailable".into(),
        ));
    }
    lowers.push(base);
    if let [only] = lowers.as_slice() {
        mount(Some(only), &target, None, libc::MS_BIND, None)?;
        return mount(
            None,
            &target,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NODEV,
            None,
        );
    }
    let options = format!(
        "lowerdir={}",
        lowers
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    );
    mount(
        Some(Path::new("overlay")),
        &target,
        Some("overlay"),
        libc::MS_RDONLY | libc::MS_NODEV,
        Some(&options),
    )
}

/// Build the slot's fixed mount points. Existing service namespaces keep
/// their old bind mounts until the ordinary session restart closes them.
pub(crate) fn prepare(root: &Path, slot: &str, layers: &[PathBuf]) -> Result<PathBuf, DaemonError> {
    let manager = fs::metadata("/proc/1/ns/mnt").map_err(io)?;
    let own = fs::metadata("/proc/self/ns/mnt").map_err(io)?;
    if manager.dev() != own.dev() || manager.ino() != own.ino() {
        return Err(DaemonError::InvalidParams(
            "slot views must be prepared in the service manager mount namespace".into(),
        ));
    }
    prepare_from(root, slot, layers, Path::new("/"))
}

fn prepare_from(
    root: &Path,
    slot: &str,
    layers: &[PathBuf],
    boot_root: &Path,
) -> Result<PathBuf, DaemonError> {
    if !tilo_vm_protocol::valid_slot_name(slot) || layers.len() > 512 {
        return Err(DaemonError::InvalidParams(
            "invalid slot view or excessive layer count".into(),
        ));
    }
    directory(root)?;
    let base = base(root, boot_root)?;
    let view = root.join(format!("slot-{slot}"));
    directory(&view)?;
    hierarchy(&base, &view, layers, "usr")?;
    hierarchy(&base, &view, layers, "opt")?;
    Ok(view)
}

pub(crate) fn remove(root: &Path, slot: &str) -> Result<(), DaemonError> {
    if !tilo_vm_protocol::valid_slot_name(slot) {
        return Err(DaemonError::InvalidParams("invalid slot view".into()));
    }
    let view = root.join(format!("slot-{slot}"));
    for name in ["usr", "opt"] {
        unmount(&view.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
