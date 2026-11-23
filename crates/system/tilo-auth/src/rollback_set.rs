//! Whether this machine could still boot an image whose sign-in checks the
//! old password hash, and so needs it kept.

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};
use tilo_rollback::{confirm, generation};

/// The PAM service every sign-in surface uses once `pam_tilo` is in the
/// image, relative to a deployment's root.
const PAM_TILO_SERVICE: &str = "etc/pam.d/tilo-auth";

#[derive(Clone, Debug)]
pub struct RollbackSet {
    /// The physical root: ostree's `ostree/deploy/<stateroot>/deploy/*` or
    /// the composefs backend's `state/deploy/*` hold one directory per
    /// deployment, each with its own `/etc`.
    pub sysroot: PathBuf,
    pub rollback_state: PathBuf,
    pub cmdline: PathBuf,
}

impl Default for RollbackSet {
    fn default() -> Self {
        Self {
            sysroot: PathBuf::from("/sysroot"),
            rollback_state: confirm::state_path(),
            cmdline: PathBuf::from("/proc/cmdline"),
        }
    }
}

impl RollbackSet {
    /// Why old password hashes are still needed, or `None` once nothing this
    /// machine could boot would check them. When it cannot tell, they are.
    pub fn needs_old_passwords(&self) -> Option<String> {
        self.why_needed()
            .unwrap_or_else(|e| Some(format!("could not tell whether they are needed: {e:#}")))
    }

    fn why_needed(&self) -> Result<Option<String>> {
        if let Some(generation) = self.unconfirmed()? {
            return Ok(Some(format!(
                "the booted generation {generation} is not confirmed yet"
            )));
        }
        let Some(deployments) = self.deployments()? else {
            return Ok(None);
        };
        if deployments.is_empty() {
            return Ok(Some(format!(
                "found no deployment under {}",
                self.sysroot.display()
            )));
        }
        Ok(deployments
            .iter()
            .find(|d| !d.join(PAM_TILO_SERVICE).is_file())
            .map(|d| {
                format!(
                    "the deployment {} predates pam_tilo and can still boot",
                    d.display()
                )
            }))
    }

    /// The booted generation, while rollback protection is in use and has not
    /// confirmed it. A machine without it (no generation on the command line,
    /// or never enrolled) has no confirmation to wait for.
    fn unconfirmed(&self) -> Result<Option<u64>> {
        let cmdline = std::fs::read_to_string(&self.cmdline)
            .with_context(|| format!("reading {}", self.cmdline.display()))?;
        let Some(booted) = generation::from_cmdline(&cmdline)? else {
            return Ok(None);
        };
        let Some(state) = confirm::load_state(&self.rollback_state)? else {
            return Ok(None);
        };
        Ok((state.last_confirmed < Some(booted)).then_some(booted))
    }

    /// Every deployment's root, or `None` on a machine that is not image
    /// based.
    fn deployments(&self) -> Result<Option<Vec<PathBuf>>> {
        let ostree = self.sysroot.join("ostree/deploy");
        let composefs = self.sysroot.join("state/deploy");
        let mut found = None;
        if let Some(stateroots) = subdirectories(&ostree)? {
            let all: &mut Vec<PathBuf> = found.get_or_insert_with(Vec::new);
            for stateroot in stateroots {
                all.extend(subdirectories(&stateroot.join("deploy"))?.unwrap_or_default());
            }
        }
        if let Some(deployments) = subdirectories(&composefs)? {
            found.get_or_insert_with(Vec::new).extend(deployments);
        }
        Ok(found)
    }
}

/// The directories in `dir`, sorted; `None` when it does not exist.
fn subdirectories(dir: &Path) -> Result<Option<Vec<PathBuf>>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("listing {}", dir.display()))?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(Some(dirs))
}
