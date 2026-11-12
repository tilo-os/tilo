//! An emulated TPM for mechanism tests: one `swtpm` per caller, so tests that
//! drive dictionary-attack lockout cannot leak it into each other.
//!
//! Needs `swtpm`, `tpm2-tools` and `cryptsetup`; probe with
//! [`require_capability!`](crate::require_capability)`(Swtpm, Swtpm::start(), ..)`.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic_in_result_fn,
    reason = "test support: failing to set up the fixture is the test's answer"
)]

use crate::SpawnUnderEnvLock as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status_locked()
        .is_ok_and(|s| s.success())
}

fn missing_tools() -> Vec<&'static str> {
    ["swtpm", "tpm2_startup", "cryptsetup"]
        .into_iter()
        .filter(|t| !have(t))
        .collect()
}

/// A running `swtpm`, killed on drop.
pub struct Swtpm {
    _state: tempfile::TempDir,
    /// The data channel's socket under the state directory, its own and no
    /// one else's: no port is picked, so none can be taken.
    socket: PathBuf,
    child: Child,
    /// swtpm's log of every command and response, when it keeps one.
    wire: Option<PathBuf>,
}

impl Swtpm {
    /// A fresh emulated TPM on its own socket. `Err` names the tools this
    /// host lacks; a TPM that will not start once they are there panics.
    pub fn start() -> Result<Self, String> {
        Self::start_with(false)
    }

    /// Like [`Swtpm::start`], and the TPM keeps every command and response it
    /// exchanges, for tests of what crosses the bus: [`Swtpm::wire`].
    pub fn start_recording() -> Result<Self, String> {
        Self::start_with(true)
    }

    fn start_with(record: bool) -> Result<Self, String> {
        let missing = missing_tools();
        if !missing.is_empty() {
            return Err(format!(
                "{missing:?} not installed (install swtpm, tpm2-tools and cryptsetup)"
            ));
        }
        Ok(Self::start_on(record).unwrap_or_else(|why| panic!("could not start swtpm: {why}")))
    }

    fn start_on(record: bool) -> Result<Self, String> {
        let state = tempfile::tempdir().expect("tempdir");
        // A data channel and the control channel tpm2-tools' `swtpm:` TCTI looks
        // for beside it, both sockets under this TPM's own state directory.
        let socket = state.path().join("tpm.sock");
        let ctrl = state.path().join("tpm.sock.ctrl");
        // No `--create-ek-cert`: minting one needs swtpm_localca, which fails in a test sandbox
        // and is not needed here.
        let setup_log = state.path().join("setup.log");
        let _ = Command::new("swtpm_setup")
            .args(["--tpm2", "--tpmstate"])
            .arg(state.path())
            .stdout(Stdio::from(
                std::fs::File::create(&setup_log).expect("setup log"),
            ))
            .stderr(Stdio::from(
                std::fs::File::options()
                    .append(true)
                    .open(&setup_log)
                    .expect("setup log"),
            ))
            .status_locked();
        let log = state.path().join("swtpm.log");
        let wire = record.then(|| state.path().join("wire.log"));
        let mut swtpm = Command::new("swtpm");
        swtpm.arg("socket");
        if let Some(wire) = &wire {
            // Level 20 logs each command and response in full, as hex.
            swtpm.arg(format!("--log=file={},level=20", wire.display()));
        }
        let child = swtpm
            .arg("--tpm2")
            .arg(format!("--tpmstate=dir={}", state.path().display()))
            .arg(format!("--server=type=unixio,path={}", socket.display()))
            .arg(format!("--ctrl=type=unixio,path={}", ctrl.display()))
            .arg("--flags=not-need-init,startup-clear")
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&log).expect("swtpm log")))
            .stderr(Stdio::from(
                std::fs::File::options()
                    .append(true)
                    .open(&log)
                    .expect("swtpm log"),
            ))
            .spawn_locked()
            .expect("spawning swtpm");
        let mut tpm = Swtpm {
            _state: state,
            socket,
            child,
            wire,
        };
        let mut last = String::new();
        // Readiness usually takes a few probes; the bound only catches a hang.
        let deadline = crate::hang_deadline();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(Some(status)) = tpm.child.try_wait() {
                return Err(format!(
                    "swtpm on {} exited ({status}): {}",
                    tpm.socket.display(),
                    std::fs::read_to_string(&log).unwrap_or_default().trim()
                ));
            }
            match Command::new("tpm2_getcap")
                .arg("properties-fixed")
                .env("TPM2TOOLS_TCTI", tpm.tcti())
                .output_locked()
            {
                Ok(o) if o.status.success() => return Ok(tpm),
                Ok(o) => last = String::from_utf8_lossy(&o.stderr).into_owned(),
                Err(e) => last = format!("could not run tpm2_getcap: {e}"),
            }
        }
        Err(format!(
            "swtpm on {} never answered.\n--- swtpm log ---\n{}\n\
             --- swtpm_setup log ---\n{}\n--- last probe error ---\n{last}",
            tpm.socket.display(),
            std::fs::read_to_string(&log).unwrap_or_default(),
            std::fs::read_to_string(&setup_log).unwrap_or_default(),
        ))
    }

    /// Every command and response this TPM exchanged so far, in order, as
    /// one byte string. Only a TPM from [`Swtpm::start_recording`] keeps them.
    #[must_use]
    pub fn wire(&self) -> Vec<u8> {
        let path = self
            .wire
            .as_deref()
            .expect("only a TPM from Swtpm::start_recording records its traffic");
        parse_wire(&std::fs::read_to_string(path).expect("swtpm's wire log"))
    }

    /// The `TPM2TOOLS_TCTI` value that reaches this TPM.
    #[must_use]
    pub fn tcti(&self) -> String {
        format!("swtpm:path={}", self.socket.display())
    }
}

/// The bytes of swtpm's `SWTPM_IO_Read` and `SWTPM_IO_Write` entries: a line
/// `SWTPM_IO_Read: length N`, then N bytes as hex, sixteen to a line.
fn parse_wire(log: &str) -> Vec<u8> {
    let mut wire = Vec::new();
    let mut left = 0_usize;
    for line in log.lines().map(str::trim) {
        let length = line
            .strip_prefix("SWTPM_IO_Read: length ")
            .or_else(|| line.strip_prefix("SWTPM_IO_Write: length "));
        if let Some(length) = length {
            left = length.parse().unwrap_or(0);
            continue;
        }
        if left == 0 {
            continue;
        }
        let bytes: Option<Vec<u8>> = line
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).ok())
            .collect();
        match bytes {
            Some(bytes) if !bytes.is_empty() => {
                left = left.saturating_sub(bytes.len());
                wire.extend(bytes);
            }
            _ => left = 0,
        }
    }
    wire
}

/// Whether `needle` occurs in `haystack`.
#[must_use]
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
