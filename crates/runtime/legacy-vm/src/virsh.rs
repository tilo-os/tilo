//! `virsh` against the user's own `qemu:///session`.

use std::ffi::OsString;
use std::io::Write as _;
use std::process::{Command, Stdio};

use libvirt_domain::{Connection, DomainState};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VirshError {
    #[error("could not run virsh: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("virsh {operation} failed: {stderr}")]
    Failed { operation: String, stderr: String },
}

/// Commands for one session domain. Every argument is a separate argv entry.
#[derive(Clone, Debug)]
pub struct Virsh {
    program: OsString,
    domain: String,
}

impl Virsh {
    #[must_use]
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            program: OsString::from("virsh"),
            domain: domain.into(),
        }
    }

    /// The argv `virsh` runs with for `args`.
    #[must_use]
    pub fn argv(&self, args: &[&str]) -> Vec<OsString> {
        Connection::Session.virsh_args(args.iter().copied())
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, VirshError> {
        let mut command = Command::new(&self.program);
        command
            .args(self.argv(args))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input)?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(VirshError::Failed {
                operation: args.first().copied().unwrap_or_default().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    pub fn state(&self) -> Result<DomainState, VirshError> {
        let names = self.run(&["list", "--all", "--name"], None)?;
        if !DomainState::listed(&names, &self.domain) {
            return Ok(DomainState::Absent);
        }
        Ok(DomainState::from_domstate(
            &self.run(&["domstate", &self.domain], None)?,
        ))
    }

    pub fn dumpxml(&self) -> Result<String, VirshError> {
        self.run(&["dumpxml", &self.domain], None)
    }

    pub fn define(&self, xml: &str) -> Result<(), VirshError> {
        self.run(&["define", "/dev/stdin"], Some(xml.as_bytes()))
            .map(drop)
    }

    pub fn has_managed_save(&self) -> Result<bool, VirshError> {
        let info = self.run(&["dominfo", &self.domain], None)?;
        Ok(managed_save_in_dominfo(&info))
    }

    /// Boots, or restores the managed save when there is one.
    pub fn start(&self) -> Result<(), VirshError> {
        self.run(&["start", &self.domain], None).map(drop)
    }

    pub fn managed_save(&self) -> Result<(), VirshError> {
        self.run(&["managedsave", &self.domain], None).map(drop)
    }

    pub fn shutdown(&self) -> Result<(), VirshError> {
        self.run(&["shutdown", &self.domain], None).map(drop)
    }

    pub fn destroy(&self) -> Result<(), VirshError> {
        self.run(&["destroy", &self.domain], None).map(drop)
    }

    pub fn set_memory_mib(&self, mib: u64) -> Result<(), VirshError> {
        let kib = mib.saturating_mul(1024).to_string();
        self.run(&["setmem", &self.domain, "--size", &kib, "--live"], None)
            .map(drop)
    }

    pub fn memory_stats(&self) -> Result<String, VirshError> {
        self.run(&["dommemstat", &self.domain], None)
    }

    pub fn attach_live(&self, device_xml: &str) -> Result<(), VirshError> {
        self.run(
            &["attach-device", &self.domain, "/dev/stdin", "--live"],
            Some(device_xml.as_bytes()),
        )
        .map(drop)
    }

    pub fn detach_live(&self, device_xml: &str) -> Result<(), VirshError> {
        self.run(
            &["detach-device", &self.domain, "/dev/stdin", "--live"],
            Some(device_xml.as_bytes()),
        )
        .map(drop)
    }
}

/// `virsh dominfo` says `Managed save:   yes` when a snapshot waits.
#[must_use]
pub fn managed_save_in_dominfo(dominfo: &str) -> bool {
    dominfo.lines().any(|line| {
        line.split_once(':').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("managed save") && value.trim() == "yes"
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virsh_talks_to_the_session_daemon_only() {
        let virsh = Virsh::new("tilo-legacy-linux");
        assert_eq!(
            virsh.argv(&["start", "tilo-legacy-linux"]),
            ["--connect", "qemu:///session", "start", "tilo-legacy-linux"].map(OsString::from)
        );
        assert!(managed_save_in_dominfo("Name: x\nManaged save:   yes\n"));
        assert!(!managed_save_in_dominfo("Managed save:   no\n"));
    }
}
