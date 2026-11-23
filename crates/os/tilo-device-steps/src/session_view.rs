//! One readonly overlay per stand-in session, independent of the host sysext. A
//! nonrecursive bind of the boot root retains its underlying `/usr` rather than the
//! overmount `systemd-sysext` installs, so account services bind the combined view and
//! never stack a third overlay over the host's `/usr`. The system extensions the host
//! merged are mounted on their own and stacked under the account's layers instead.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::error::DaemonError;

fn io(error: std::io::Error) -> DaemonError {
    DaemonError::io("prepare the readonly account filesystem view")(error)
}

fn text(path: &Path) -> Result<CString, DaemonError> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| DaemonError::InvalidParams("invalid account view path".into()))
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
            "account view directory is not root controlled".into(),
        ));
    }
    if path
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || b":,\\".contains(byte))
    {
        return Err(DaemonError::InvalidParams(
            "unsafe account view mount path".into(),
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
            // Kernel mountinfo escapes whitespace and backslashes; an unexpected escaped descendant
            // is refused, never interpreted as a different mountpoint or handed to a shell.
            if path.as_os_str().as_bytes().contains(&b'\\') || children.len() >= 512 {
                return Err(DaemonError::InvalidParams(
                    "unexpected retained-base mount hierarchy".into(),
                ));
            }
            children.push(path.to_path_buf());
        }
    }
    // A bind of a shared boot root inherits its propagation group, so every retained child is
    // disconnected before it is unmounted: otherwise repair would also unmount the host's live
    // `/usr`.
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
    // A separate `/usr` filesystem missing from the boot-root backing tree cannot be silently
    // replaced by an empty directory.
    if !base.join("usr/lib/os-release").is_file() {
        return Err(DaemonError::InvalidParams(
            "the immutable boot root does not contain the base /usr".into(),
        ));
    }
    Ok(base)
}

fn hierarchy(base: &Path, view: &Path, layers: &[PathBuf], name: &str) -> Result<(), DaemonError> {
    let target = view.join(name);
    // The mounted root presents imported permissions, not the protected backing directory, so that
    // old view is released before the backing mountpoint is validated; running sessions keep their
    // bind mounts.
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

/// Build the account's fixed mount points; existing service namespaces keep their old bind mounts
/// until the ordinary session restart closes them.
pub(crate) fn prepare(
    root: &Path,
    account: &str,
    layers: &[PathBuf],
) -> Result<PathBuf, DaemonError> {
    let manager = fs::metadata("/proc/1/ns/mnt").map_err(io)?;
    let own = fs::metadata("/proc/self/ns/mnt").map_err(io)?;
    if manager.dev() != own.dev() || manager.ino() != own.ino() {
        return Err(DaemonError::InvalidParams(
            "account views must be prepared in the service manager mount namespace".into(),
        ));
    }
    // The extensions are mounted, and earlier ones let go, under the views' root: on a computer's
    // first account it does not exist yet.
    directory(root)?;
    let mut stacked = merged_extensions(root)?;
    stacked.extend_from_slice(layers);
    prepare_from(root, account, &stacked, Path::new("/"))
}

fn command(step: &str, program: &str, args: &[&std::ffi::OsStr]) -> Result<String, DaemonError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| DaemonError::Apply {
            step: step.to_owned(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(DaemonError::Apply {
            step: step.to_owned(),
            detail: format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The system extensions `systemd-sysext` merged into the host, each a tree of its own under
/// `root`, lowest first. The view stacks on the boot root rather than on the merged `/usr` (a
/// third overlay the kernel refuses), so without these an account would not see an extension the
/// computer has installed, such as Windows support.
fn merged_extensions(root: &Path) -> Result<Vec<PathBuf>, DaemonError> {
    // Without systemd-sysext nothing can have merged.
    if std::process::Command::new("systemd-sysext")
        .arg("--version")
        .output()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    let status = command(
        "read the merged system extensions",
        "systemd-sysext",
        &["status".as_ref(), "--json=short".as_ref()],
    )?;
    let list = command(
        "read the merged system extensions",
        "systemd-sysext",
        &["list".as_ref(), "--json=short".as_ref()],
    )?;
    let extensions = merged(&status, &list)?;
    let mut trees = Vec::with_capacity(extensions.len());
    let mut kept = Vec::with_capacity(extensions.len());
    for extension in extensions {
        let tree = match extension.kind {
            ExtensionKind::Directory => directory_tree(&extension.path)?,
            ExtensionKind::Image => {
                let identity = fs::metadata(&extension.path).map_err(io)?;
                let tree = root.join(format!(
                    "extension-{}-{:x}-{:x}",
                    extension.name,
                    identity.ino(),
                    identity.mtime()
                ));
                if !mounted(&tree)? {
                    directory(&tree)?;
                    command(
                        "mount a system extension for the account view",
                        "systemd-dissect",
                        &[
                            "--mount".as_ref(),
                            "--read-only".as_ref(),
                            extension.path.as_os_str(),
                            tree.as_os_str(),
                        ],
                    )?;
                }
                kept.push(tree.clone());
                tree
            }
        };
        trees.push(tree);
    }
    release_replaced_extensions(root, &kept)?;
    Ok(trees)
}

/// The tree a merged directory extension is stacked from: `/var/lib/extensions` keeps it as a
/// link, and the link's own mode says nothing about the tree it names, so the tree is what is
/// resolved and stacked.
fn directory_tree(path: &Path) -> Result<PathBuf, DaemonError> {
    let tree = path.canonicalize().map_err(io)?;
    if !tree.is_dir() {
        return Err(DaemonError::InvalidParams(
            "a merged extension is not a directory".into(),
        ));
    }
    Ok(tree)
}

/// An extension image replaced since an earlier view was mounted is let go once nothing holds it:
/// a running session keeps the tree it bound until it restarts.
fn release_replaced_extensions(root: &Path, kept: &[PathBuf]) -> Result<(), DaemonError> {
    for entry in fs::read_dir(root).map_err(io)? {
        let path = entry.map_err(io)?.path();
        let replaced = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("extension-"))
            && !kept.contains(&path);
        if replaced && mounted(&path)? {
            let target = text(&path)?;
            // SAFETY: The validated NUL-terminated path remains alive through umount.
            if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } != 0 {
                return Err(io(std::io::Error::last_os_error()));
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ExtensionKind {
    Image,
    Directory,
}

#[derive(Debug, PartialEq, Eq)]
struct Extension {
    name: String,
    kind: ExtensionKind,
    path: PathBuf,
}

/// The extensions `systemd-sysext status` names as merged into any hierarchy, with the paths
/// `systemd-sysext list` reads them from, in the order the status names them.
fn merged(status: &str, list: &str) -> Result<Vec<Extension>, DaemonError> {
    #[derive(serde::Deserialize)]
    struct Hierarchy {
        extensions: serde_json::Value,
    }
    #[derive(serde::Deserialize)]
    struct Listed {
        name: String,
        #[serde(rename = "type")]
        kind: String,
        path: PathBuf,
    }
    let invalid = |detail: String| DaemonError::Apply {
        step: "read the merged system extensions".into(),
        detail,
    };
    let hierarchies: Vec<Hierarchy> =
        serde_json::from_str(status).map_err(|error| invalid(error.to_string()))?;
    let listed: Vec<Listed> =
        serde_json::from_str(list).map_err(|error| invalid(error.to_string()))?;
    let mut names = Vec::new();
    for hierarchy in hierarchies {
        // A hierarchy without extensions says so with the string "none".
        let serde_json::Value::Array(merged) = hierarchy.extensions else {
            continue;
        };
        for name in merged {
            let name = name
                .as_str()
                .ok_or_else(|| invalid("an extension name is not text".into()))?;
            if !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
    }
    names
        .into_iter()
        .map(|name| {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                || name.starts_with('.')
            {
                return Err(invalid(format!("unsafe extension name {name:?}")));
            }
            let found = listed
                .iter()
                .find(|listed| listed.name == name)
                .ok_or_else(|| invalid(format!("merged extension {name} is not listed")))?;
            let kind = match found.kind.as_str() {
                "raw" => ExtensionKind::Image,
                "directory" | "subvolume" => ExtensionKind::Directory,
                other => return Err(invalid(format!("extension {name} is a {other}"))),
            };
            Ok(Extension {
                name,
                kind,
                path: found.path.clone(),
            })
        })
        .collect()
}

fn prepare_from(
    root: &Path,
    account: &str,
    layers: &[PathBuf],
    boot_root: &Path,
) -> Result<PathBuf, DaemonError> {
    if !tilo_device_protocol::valid_account_name(account) || layers.len() > 512 {
        return Err(DaemonError::InvalidParams(
            "invalid account view or excessive layer count".into(),
        ));
    }
    directory(root)?;
    let base = base(root, boot_root)?;
    let view = root.join(format!("account-{account}"));
    directory(&view)?;
    hierarchy(&base, &view, layers, "usr")?;
    hierarchy(&base, &view, layers, "opt")?;
    Ok(view)
}

pub(crate) fn remove(root: &Path, account: &str) -> Result<(), DaemonError> {
    if !tilo_device_protocol::valid_account_name(account) {
        return Err(DaemonError::InvalidParams("invalid account view".into()));
    }
    let view = root.join(format!("account-{account}"));
    for name in ["usr", "opt"] {
        unmount(&view.join(name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
