//! `virsh` for one domain, as typed argument vectors and never a shell line.

use std::ffi::OsString;
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

use crate::{Connection, DomainState};

/// A hard deadline for every `virsh` call: a wedged daemon must not hang the
/// caller forever.
const TIMEOUT: Duration = Duration::from_secs(300);
/// Domain XML and `virsh` diagnostics are far below this.
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// What one `virsh` run returned.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VirshOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl VirshOutput {
    #[must_use]
    pub fn successful(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            success: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }
}

/// Runs `virsh` with exactly `args`; `stdin` may carry private domain XML.
pub trait VirshRunner: Send + Sync {
    fn run(&self, args: &[OsString], stdin: &[u8]) -> Result<VirshOutput, VirshError>;
}

/// The real `virsh`, with bounded output and time.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoundedVirsh;

impl VirshRunner for BoundedVirsh {
    fn run(&self, args: &[OsString], stdin: &[u8]) -> Result<VirshOutput, VirshError> {
        let mut command = Command::new("virsh");
        command.args(args);
        let input = (!stdin.is_empty()).then_some(stdin);
        let output = bounded_command::run_prepared(command, input, OUTPUT_LIMIT, TIMEOUT)?;
        Ok(VirshOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Debug, Error)]
pub enum VirshError {
    #[error("could not run virsh: {0}")]
    Run(#[from] bounded_command::Error),
    #[error("virsh {operation} failed with code {code:?}: {stderr}")]
    Failed {
        operation: String,
        code: Option<i32>,
        stderr: String,
    },
}

/// `virsh` commands for one domain on one connection.
#[derive(Clone, Debug)]
pub struct Virsh<R = BoundedVirsh> {
    runner: R,
    connection: Connection,
    domain: String,
}

impl Virsh {
    #[must_use]
    pub fn new(connection: Connection, domain: impl Into<String>) -> Self {
        Self::with_runner(BoundedVirsh, connection, domain)
    }
}

impl<R> Virsh<R> {
    #[must_use]
    pub fn with_runner(runner: R, connection: Connection, domain: impl Into<String>) -> Self {
        Self {
            runner,
            connection,
            domain: domain.into(),
        }
    }

    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    #[must_use]
    pub const fn runner(&self) -> &R {
        &self.runner
    }
}

impl<R: VirshRunner> Virsh<R> {
    /// Runs `virsh <args…>` on this connection and returns its stdout, or its
    /// stderr as [`VirshError::Failed`].
    pub fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String, VirshError> {
        let output = self.runner.run(
            &self.connection.virsh_args(args.iter().copied()),
            stdin.unwrap_or_default(),
        )?;
        if output.success {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(VirshError::Failed {
                operation: args.first().copied().unwrap_or_default().to_string(),
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    pub fn exists(&self) -> Result<bool, VirshError> {
        let names = self.run(&["list", "--all", "--name"], None)?;
        Ok(DomainState::listed(&names, &self.domain))
    }

    pub fn state(&self) -> Result<DomainState, VirshError> {
        if !self.exists()? {
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

    /// Boots, or restores the managed save when there is one.
    pub fn start(&self) -> Result<(), VirshError> {
        self.on_domain("start", &[])
    }

    /// Graceful ACPI shutdown.
    pub fn shutdown(&self) -> Result<(), VirshError> {
        self.on_domain("shutdown", &[])
    }

    pub fn destroy(&self) -> Result<(), VirshError> {
        self.on_domain("destroy", &[])
    }

    pub fn has_managed_save(&self) -> Result<bool, VirshError> {
        let info = self.run(&["dominfo", &self.domain], None)?;
        Ok(managed_save_in_dominfo(&info))
    }

    pub fn managed_save(&self) -> Result<(), VirshError> {
        self.on_domain("managedsave", &[])
    }

    pub fn set_memory_mib(&self, mib: u64) -> Result<(), VirshError> {
        let kib = mib.saturating_mul(1024).to_string();
        self.on_domain("setmem", &["--size", &kib, "--live"])
    }

    pub fn memory_stats(&self) -> Result<String, VirshError> {
        self.run(&["dommemstat", &self.domain], None)
    }

    /// Attaches a device; `scope` is `--live`, `--config` or both.
    pub fn attach_device(&self, device_xml: &str, scope: &[&str]) -> Result<(), VirshError> {
        self.device("attach-device", device_xml, scope)
    }

    /// Detaches a device; `scope` is `--live`, `--config` or both.
    pub fn detach_device(&self, device_xml: &str, scope: &[&str]) -> Result<(), VirshError> {
        self.device("detach-device", device_xml, scope)
    }

    /// `virsh <command> <domain> <args…>`.
    pub fn on_domain(&self, command: &str, args: &[&str]) -> Result<(), VirshError> {
        let mut full = vec![command, &self.domain];
        full.extend_from_slice(args);
        self.run(&full, None).map(drop)
    }

    fn device(&self, command: &str, device_xml: &str, scope: &[&str]) -> Result<(), VirshError> {
        let mut full = vec![command, &self.domain, "/dev/stdin"];
        full.extend_from_slice(scope);
        self.run(&full, Some(device_xml.as_bytes())).map(drop)
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
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(Vec<OsString>, Vec<u8>)>>);

    impl VirshRunner for Recorder {
        fn run(&self, args: &[OsString], stdin: &[u8]) -> Result<VirshOutput, VirshError> {
            self.0.lock().unwrap().push((args.to_vec(), stdin.to_vec()));
            Ok(VirshOutput::successful("x\n"))
        }
    }

    #[test]
    fn virsh_targets_one_domain_on_one_connection() {
        let virsh = Virsh::with_runner(Recorder::default(), Connection::Session, "x");
        virsh.start().unwrap();
        virsh.attach_device("<filesystem/>", &["--live"]).unwrap();
        assert_eq!(virsh.state().unwrap(), DomainState::Other);
        let calls = virsh.runner().0.lock().unwrap();
        assert_eq!(
            calls[0].0,
            ["--connect", "qemu:///session", "start", "x"].map(OsString::from)
        );
        assert_eq!(
            calls[1],
            (
                [
                    "--connect",
                    "qemu:///session",
                    "attach-device",
                    "x",
                    "/dev/stdin",
                    "--live"
                ]
                .map(OsString::from)
                .to_vec(),
                b"<filesystem/>".to_vec()
            )
        );
        drop(calls);
        assert!(managed_save_in_dominfo("Name: x\nManaged save:   yes\n"));
        assert!(!managed_save_in_dominfo("Managed save:   no\n"));
    }
}
