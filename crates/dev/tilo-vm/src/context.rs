//! Where tilo-vm is running: the git worktree, its branch, and the slot
//! name and owner derived from them.

use std::path::{Path, PathBuf};
use std::process::Command;

use tilo_vm_protocol::{DEVICE_SLOT, Owner, VM_SLOT, valid_slot_name};

#[derive(Debug, Clone, Default)]
pub struct Worktree {
    pub root: PathBuf,
    pub branch: Option<String>,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    git_said(dir, args).ok()
}

/// `git -C dir args`: its trimmed, non-empty stdout, or what went wrong (git's
/// own stderr), for checks that must say why they could not observe a fact.
fn git_said(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && !stdout.is_empty() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(match stderr.trim() {
        "" => format!("git {} {}", args.join(" "), output.status),
        said => said.to_owned(),
    })
}

impl Worktree {
    pub fn detect(dir: &Path) -> Option<Self> {
        let root = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
        let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        Some(Self { root, branch })
    }

    /// The default slot: the branch, else the worktree directory's name.
    pub fn default_slot(&self) -> Option<String> {
        let base = self.branch.clone().or_else(|| {
            self.root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })?;
        slot_name_from(&base)
    }

    pub fn owner(&self) -> Owner {
        Owner {
            host: hostname(),
            worktree: Some(self.root.display().to_string()),
            branch: self.branch.clone(),
        }
    }

    pub fn head(&self) -> Option<String> {
        git(&self.root, &["rev-parse", "HEAD"])
    }

    /// The commit this checkout's HEAD names, read the way build provenance
    /// (`head`) and the acceptance harness read it. Resolving HEAD to its id
    /// does not open the commit object, so a worktree whose shared object
    /// store this user cannot read (an unprivileged user's worktree of a
    /// root-owned CI clone) still answers. A checkout's HEAD always names a
    /// commit, and that id is the identity: peeling it adds no guarantee.
    ///
    /// # Errors
    ///
    /// What git said when it could not resolve HEAD here.
    pub fn checkout_commit(&self) -> Result<String, String> {
        git_said(&self.root, &["rev-parse", "--verify", "HEAD"])
    }

    /// The full commit a revision names, if this repository has it.
    pub fn rev_parse(&self, rev: &str) -> Option<String> {
        git(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
    }

    /// Everything that differs from HEAD, for provenance: the tracked diff
    /// plus untracked paths. `None` for a clean tree.
    pub fn dirty_state(&self) -> Option<String> {
        let status = git(&self.root, &["status", "--porcelain"])?;
        let diff = git(&self.root, &["diff", "HEAD"]).unwrap_or_default();
        Some(format!("{status}\n{diff}"))
    }
}

pub fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            let output = Command::new("hostname").output().ok()?;
            Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .filter(|h| !h.is_empty())
}

/// Branch or directory name to slot name: lowercase, runs of anything else
/// become one dash, a leading `worktree-`/`agent-` style prefix is kept,
/// trimmed to 28 bytes. `None` when nothing usable remains.
pub fn slot_name_from(text: &str) -> Option<String> {
    let text = text.rsplit('/').next().unwrap_or(text);
    let mut name = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            name.push(c);
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
    let mut name: String = name.chars().take(28).collect();
    while name.ends_with('-') {
        name.pop();
    }
    if name == VM_SLOT || name == DEVICE_SLOT {
        name.push_str("-slot");
    }
    valid_slot_name(&name).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_from_branches() {
        assert_eq!(
            slot_name_from("worktree-vm-dev-loop").as_deref(),
            Some("worktree-vm-dev-loop")
        );
        assert_eq!(
            slot_name_from("codex/Type_Action--Authority").as_deref(),
            Some("type-action-authority")
        );
        assert_eq!(slot_name_from("feature/42-fix").as_deref(), Some("fix"));
        assert_eq!(slot_name_from("vm").as_deref(), Some("vm-slot"));
        assert_eq!(slot_name_from("device").as_deref(), Some("device-slot"));
        assert_eq!(
            slot_name_from("a-very-long-branch-name-that-keeps-going").map(|n| n.len()),
            Some(28)
        );
        assert!(
            slot_name_from("a-very-long-branch-name-that-keeps-going")
                .is_some_and(|n| !n.ends_with('-'))
        );
        assert_eq!(slot_name_from("___"), None);
    }
}
