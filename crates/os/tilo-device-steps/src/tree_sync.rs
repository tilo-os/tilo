//! The `tree-sync` step: lay a worktree out under the build root from a manifest whose files are
//! all in the content store. Only what differs is written; whatever the manifest no longer names
//! is deleted, except `target/`, where the builds keep their output.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path, PathBuf};

use tilo_device_protocol::{TreeManifest, TreeSync, TreeSyncResult};

use crate::build_user::BuildUser;
use crate::content;
use crate::daemon::Daemon;
use crate::error::DaemonError;
use crate::targets::valid_worktree_id;

/// The top-level directory a sync never touches.
const KEPT: &str = "target";

/// A manifest path as a relative path of plain components outside `target/`.
fn relative(path: &str) -> Result<PathBuf, DaemonError> {
    let parsed = Path::new(path);
    let plain = !path.is_empty()
        && parsed
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    let kept = parsed.components().next() == Some(Component::Normal(KEPT.as_ref()));
    if plain && !kept {
        Ok(parsed.to_owned())
    } else {
        Err(DaemonError::InvalidParams(format!(
            "'{path}' is not a path inside the worktree"
        )))
    }
}

fn blake3_of(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read)?);
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// Remove whatever is at `path` (a file, a symlink or a directory), if anything.
fn clear(path: &Path) -> Result<(), DaemonError> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(DaemonError::io(format!("remove {}", path.display())))
}

struct Layout<'a> {
    root: PathBuf,
    user: Option<&'a BuildUser>,
}

impl Layout<'_> {
    /// Create the directories leading to `path`, owned by the build user.
    fn parents(&self, path: &Path) -> Result<(), DaemonError> {
        let mut dir = self.root.clone();
        for component in path.parent().into_iter().flat_map(Path::components) {
            dir.push(component);
            if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
                clear(&dir)?;
                fs::create_dir(&dir)
                    .map_err(DaemonError::io(format!("create {}", dir.display())))?;
                self.own(&dir)?;
            }
        }
        Ok(())
    }

    fn own(&self, path: &Path) -> Result<(), DaemonError> {
        self.user.map_or(Ok(()), |user| user.own(path))
    }
}

/// Run a [`TreeSync`].
///
/// # Errors
/// A worktree id or manifest that is not valid, content the store lacks, or a write that failed.
pub fn sync(daemon: &Daemon, request: &TreeSync) -> Result<TreeSyncResult, DaemonError> {
    if !valid_worktree_id(&request.worktree) {
        return Err(DaemonError::InvalidParams(format!(
            "'{}' is not a valid worktree id",
            request.worktree
        )));
    }
    let manifest_path = content::blob(&daemon.content, &request.manifest)?;
    let manifest: TreeManifest = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(DaemonError::io("read the tree manifest"))?,
    )
    .map_err(|e| DaemonError::InvalidParams(format!("the tree manifest is not one: {e}")))?;
    let _build = daemon.locks.src(&request.worktree)?;
    fs::create_dir_all(daemon.targets.dir()).map_err(DaemonError::io("create the build root"))?;
    let user = BuildUser::for_build_root(daemon.targets.dir())?;
    let layout = Layout {
        root: daemon.targets.src(&request.worktree),
        user: user.as_ref(),
    };
    if !layout.root.is_dir() {
        fs::create_dir_all(&layout.root)
            .map_err(DaemonError::io(format!("create {}", layout.root.display())))?;
        layout.own(&layout.root)?;
    }

    let mut wanted = BTreeSet::new();
    let mut written = 0_u64;
    for file in &manifest.entries {
        let path = relative(&file.path)?;
        let source = content::blob(&daemon.content, &file.hash)?;
        let dest = layout.root.join(&path);
        let mode = file.mode & 0o7777;
        let same = fs::symlink_metadata(&dest)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o7777 == mode)
            && blake3_of(&dest).as_deref() == Some(file.hash.as_str());
        if !same {
            layout.parents(&path)?;
            clear(&dest)?;
            let staged = dest.with_file_name(format!(
                ".{}.tree-sync",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
            ));
            fs::copy(&source, &staged)
                .and_then(|_| fs::set_permissions(&staged, fs::Permissions::from_mode(mode)))
                .and_then(|()| fs::rename(&staged, &dest))
                .map_err(DaemonError::io(format!("write {}", dest.display())))?;
            layout.own(&dest)?;
            written = written.saturating_add(1);
        }
        wanted.insert(path);
    }
    for link in &manifest.symlinks {
        let path = relative(&link.path)?;
        let dest = layout.root.join(&path);
        if fs::read_link(&dest).ok().as_deref() != Some(Path::new(&link.target)) {
            layout.parents(&path)?;
            clear(&dest)?;
            std::os::unix::fs::symlink(&link.target, &dest)
                .map_err(DaemonError::io(format!("link {}", dest.display())))?;
            layout.own(&dest)?;
            written = written.saturating_add(1);
        }
        wanted.insert(path);
    }

    let dirs: BTreeSet<PathBuf> = wanted
        .iter()
        .flat_map(|path| path.ancestors().skip(1))
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(Path::to_owned)
        .collect();
    let mut removed = 0_u64;
    let mut pending = vec![PathBuf::new()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(layout.root.join(&dir))
            .map_err(DaemonError::io(format!("read {}", dir.display())))?;
        for entry in entries.flatten() {
            let path = dir.join(entry.file_name());
            if path.as_os_str() == KEPT || wanted.contains(&path) {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if is_dir && dirs.contains(&path) {
                pending.push(path);
            } else {
                clear(&layout.root.join(&path))?;
                removed = removed.saturating_add(1);
            }
        }
    }
    Ok(TreeSyncResult { written, removed })
}
