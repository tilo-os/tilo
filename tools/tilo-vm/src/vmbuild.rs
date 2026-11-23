//! The in-VM builder's host half: which files of the worktree go to the VM, and the name of its
//! directory there. The files go as content blobs and a `tree-sync` step; the build runs as the
//! device's `build` step, its output shown live on this terminal.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::context::{Worktree, account_name_from};
use crate::error::CliError;

/// The VM's directory name for this host's worktree: readable, and unique
/// per host and path so two machines' `main` checkouts never share a build dir.
pub fn worktree_id(worktree: &Worktree, host: Option<&str>) -> String {
    let name = worktree
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .and_then(|n| account_name_from(&n))
        .unwrap_or_else(|| "worktree".into());
    let digest = Sha256::digest(
        format!("{}\0{}", host.unwrap_or_default(), worktree.root.display()).as_bytes(),
    );
    let short: String = name.chars().take(40).collect();
    format!(
        "{}-{}",
        short.trim_end_matches('-'),
        hex::encode(digest.get(..4).unwrap_or_default())
    )
}

pub fn sh_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The files of `root` a build in the VM needs: what git tracks and the untracked files it does
/// not ignore, so `target/`, `.git` and other ignored output never go.
pub fn worktree_files(root: &Path) -> Result<Vec<PathBuf>, CliError> {
    let listed = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(CliError::io("run git ls-files"))?;
    if !listed.status.success() {
        return Err(CliError::CommandFailed {
            command: format!("git -C {} ls-files", root.display()),
            status: listed.status.to_string(),
            stderr: String::from_utf8_lossy(&listed.stderr).trim().to_owned(),
        });
    }
    let mut files: Vec<PathBuf> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty() && *name != b".tilo-vm")
        .map(|name| PathBuf::from(String::from_utf8_lossy(name).into_owned()))
        // A deleted file git still tracks is not in the tree.
        .filter(|path| root.join(path).symlink_metadata().is_ok())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_worktree_has_its_own_build_directory_in_the_vm() {
        {
            let wt = Worktree {
                root: PathBuf::from("/home/a/tilo/.claude/worktrees/agent-A74"),
                branch: None,
            };
            let a = worktree_id(&wt, Some("laptop"));
            assert!(a.starts_with("agent-a74-"), "{a}");
            assert_eq!(a.len(), "agent-a74-".len() + 8);
            assert_ne!(a, worktree_id(&wt, Some("desktop")));
            assert_eq!(a, worktree_id(&wt, Some("laptop")));
        }
    }
}
