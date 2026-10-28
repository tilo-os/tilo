//! Whether this machine could still boot an image whose sign-in checks the
//! old password hash, and so needs it kept.
//!
//! An image from before `pam_tilo` checks secrets with `pam_unix` against its
//! own `/etc/shadow`. Until the booted update is confirmed and every
//! deployment the machine could boot ships the `tilo-auth` PAM service,
//! a migrated person's old hash stays where such an image may need it.

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};
use tilo_rollback::{confirm, generation};

/// The PAM service every sign-in surface uses once `pam_tilo` is in the
/// image, relative to a deployment's root.
const PAM_TILO_SERVICE: &str = "etc/pam.d/tilo-auth";

/// Where to read the deployments and the confirmation from.
#[derive(Clone, Debug)]
pub struct RollbackSet {
    /// The physical root: ostree's `ostree/deploy/<stateroot>/deploy/*` or
    /// the composefs backend's `state/deploy/*` hold one directory per
    /// deployment, each with its own `/etc`.
    pub sysroot: PathBuf,
    /// tilo-rollback's bookkeeping, with the last confirmed generation.
    pub rollback_state: PathBuf,
    /// The kernel command line, with the booted generation.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    struct Machine {
        dir: tempfile::TempDir,
    }

    impl Machine {
        fn new(cmdline: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("cmdline"), cmdline).unwrap();
            Self { dir }
        }

        fn set(&self) -> RollbackSet {
            RollbackSet {
                sysroot: self.dir.path().join("sysroot"),
                rollback_state: self.dir.path().join("rollback/state.json"),
                cmdline: self.dir.path().join("cmdline"),
            }
        }

        fn deployment(&self, at: &str, pam_tilo: bool) {
            let etc = self.dir.path().join("sysroot").join(at).join("etc/pam.d");
            std::fs::create_dir_all(&etc).unwrap();
            if pam_tilo {
                std::fs::write(etc.join("tilo-auth"), "auth sufficient pam_tilo.so\n").unwrap();
            }
        }

        fn remove(&self, at: &str) {
            std::fs::remove_dir_all(self.dir.path().join("sysroot").join(at)).unwrap();
        }

        fn confirmed(&self, generation: Option<u64>) {
            let state = confirm::State {
                nv_index: 0x0150_0016,
                nv_base: 0,
                last_confirmed: generation,
                foreign_keyslots: Vec::new(),
            };
            confirm::save_state(&self.set().rollback_state, &state).unwrap();
        }
    }

    const OLD: &str = "ostree/deploy/default/deploy/aaaa.0";
    const NEW: &str = "ostree/deploy/default/deploy/bbbb.0";

    #[test]
    fn a_machine_that_is_not_image_based_needs_no_old_passwords() {
        let m = Machine::new("root=/dev/vda3 quiet");
        assert_eq!(m.set().needs_old_passwords(), None);
    }

    #[test]
    fn an_older_deployment_without_pam_tilo_keeps_them_until_it_is_gone() {
        let m = Machine::new("root=/dev/vda3");
        m.deployment(OLD, false);
        m.deployment(NEW, true);
        let why = m.set().needs_old_passwords().unwrap();
        assert!(why.contains("aaaa.0"), "{why}");
        m.remove(OLD);
        assert_eq!(m.set().needs_old_passwords(), None);
    }

    #[test]
    fn composefs_deployments_count_too() {
        let m = Machine::new("root=/dev/vda3");
        m.deployment("state/deploy/1111", true);
        m.deployment("state/deploy/2222", false);
        assert!(m.set().needs_old_passwords().unwrap().contains("2222"));
        m.remove("state/deploy/2222");
        assert_eq!(m.set().needs_old_passwords(), None);
    }

    #[test]
    fn an_unconfirmed_generation_keeps_them_while_rollback_protection_is_in_use() {
        let m = Machine::new("root=/dev/vda3 tilo.generation=7");
        m.deployment(NEW, true);
        // Not enrolled: nothing will ever confirm, so there is nothing to wait for.
        assert_eq!(m.set().needs_old_passwords(), None);
        m.confirmed(Some(6));
        assert!(
            m.set()
                .needs_old_passwords()
                .unwrap()
                .contains("generation 7")
        );
        m.confirmed(None);
        assert!(m.set().needs_old_passwords().is_some());
        m.confirmed(Some(7));
        assert_eq!(m.set().needs_old_passwords(), None);
    }

    #[test]
    fn what_cannot_be_read_keeps_them() {
        let m = Machine::new("tilo.generation=x");
        assert!(m.set().needs_old_passwords().is_some());
        let m = Machine::new("root=/dev/vda3");
        std::fs::create_dir_all(m.dir.path().join("sysroot/ostree/deploy/default/deploy")).unwrap();
        assert!(
            m.set().needs_old_passwords().is_some(),
            "no deployment found"
        );
    }
}
