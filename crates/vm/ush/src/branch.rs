//! A workspace's environments, branched into another workspace, as the person's
//! `workspace.branch` asks.
//!
//! Each environment of the source becomes one of the branch under the same key: its
//! record forked with its history ([`Records::branch_into`]), its home and grants moved
//! to the same places in the branch's tree, and what it keeps outside its record (its
//! legacy home, its guest's `/var`) copied there. Its id is the branch's
//! (`<branch>/<key>`), so its sessions and MCP server are the branch's. The source is
//! read and never changed, and images are shared already.

use std::path::{Path, PathBuf};

use tilo_files::native::tree::{Strategy, clone_tree};

use crate::records::Records;
use crate::store::Store;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Branched {
    pub environments: u64,
    pub files: u64,
    pub bytes: u64,
}

/// Branch the environments of the workspace `from` is of into the one `to` is of
/// ([`Store::for_workspace`]).
///
/// # Errors
/// The two are one workspace, the branch has an environment of the same key already, or a record
/// or a file could not be read or written.
pub fn branch_environments(from: &Store, to: &Store) -> Result<Branched, String> {
    if from.workspace_id() == to.workspace_id() {
        return Err("a workspace is branched into another".to_owned());
    }
    let source = Records::of(from);
    let target = Records::of(to);
    let trees = (
        from.workspace().map(Path::to_path_buf),
        to.workspace().map(Path::to_path_buf),
    );
    let rehome = |path: &str| moved(path, &trees);
    let mut branched = Branched::default();
    for key in source.keys().map_err(|error| error.to_string())? {
        if !source
            .branch_into(&key, &target, &rehome)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        branched.environments = branched.environments.saturating_add(1);
        let kept = from.environment_dir(&key);
        if kept.is_dir() {
            let copied = clone_tree(
                &kept,
                &to.environment_dir(&key),
                Strategy::Clone,
                &mut |path| Some(path.to_path_buf()),
                &mut |_| {},
            )
            .map_err(|error| format!("environment {key}'s own files: {error}"))?;
            branched.files = branched.files.saturating_add(copied.files);
            branched.bytes = branched.bytes.saturating_add(copied.bytes);
        }
    }
    Ok(branched)
}

fn moved(path: &str, (from, to): &(Option<PathBuf>, Option<PathBuf>)) -> Option<String> {
    let (from, to) = (from.as_deref()?, to.as_deref()?);
    let rest = Path::new(path).strip_prefix(from).ok()?;
    let moved = if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    };
    Some(moved.to_string_lossy().into_owned())
}
