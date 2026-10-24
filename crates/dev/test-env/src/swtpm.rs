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

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .is_ok_and(|s| s.success())
}

fn missing_tools() -> Vec<&'static str> {
    ["swtpm", "tpm2_startup", "cryptsetup"]
        .into_iter()
        .filter(|t| !have(t))
        .collect()
}

/// A free TCP port `n` for which `n + 1` is also free.
///
/// Binding then releasing is racy: two tests can grab the same pair, so the loser's `swtpm`
/// fails with "address already in use" and its test talks to the wrong TPM. Retry with a
/// fresh pair on failure.
fn free_port_pair() -> (u16, u16) {
    use std::sync::atomic::{AtomicU16, Ordering};
    static SKEW: AtomicU16 = AtomicU16::new(0);
    let skew = SKEW.fetch_add(2, Ordering::Relaxed);
    for attempt in 0..64u16 {
        let base = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().unwrap().port()
        };
        // Walk away from the kernel's suggestion by a per-caller amount so two
        // threads handed adjacent ephemeral ports do not converge on one pair.
        let base = base
            .checked_add(skew)
            .and_then(|b| b.checked_add(attempt.saturating_mul(2)))
            .unwrap_or(base);
        if base >= u16::MAX - 1 {
            continue;
        }
        let a = std::net::TcpListener::bind(("127.0.0.1", base));
        let b = std::net::TcpListener::bind(("127.0.0.1", base.saturating_add(1)));
        if a.is_ok() && b.is_ok() {
            return (base, base.saturating_add(1));
        }
    }
    panic!("could not find an adjacent free TCP port pair for swtpm");
}

/// A running `swtpm`, killed on drop.
pub struct Swtpm {
    _state: tempfile::TempDir,
    port: u16,
    child: Child,
    /// swtpm's log of every command and response, when it keeps one.
    wire: Option<PathBuf>,
}

impl Swtpm {
    /// A fresh emulated TPM on its own port pair. `Err` names the tools this
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
        // Retry on a fresh port pair: a lost race shows up as swtpm exiting immediately.
        let mut last = String::new();
        for _ in 0..8 {
            match Self::try_start(record) {
                Ok(tpm) => return Ok(tpm),
                Err(why) => last = why,
            }
        }
        panic!("could not start swtpm after 8 attempts; last failure: {last}");
    }

    fn try_start(record: bool) -> Result<Self, String> {
        let state = tempfile::tempdir().expect("tempdir");
        // tpm2-tools' `swtpm:` TCTI assumes the control channel is at `port + 1`; two unrelated
        // free ports produce a data channel that answers and a control channel that looks broken.
        let (port, ctrl) = free_port_pair();
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
            .status();
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
            .arg(format!("--server=type=tcp,port={port}"))
            .arg(format!("--ctrl=type=tcp,port={ctrl}"))
            .arg("--flags=not-need-init,startup-clear")
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&log).expect("swtpm log")))
            .stderr(Stdio::from(
                std::fs::File::options()
                    .append(true)
                    .open(&log)
                    .expect("swtpm log"),
            ))
            .spawn()
            .expect("spawning swtpm");
        let mut tpm = Swtpm {
            _state: state,
            port,
            child,
            wire,
        };
        let mut last = String::new();
        // Readiness usually takes a few probes; the bound only catches a hang.
        let deadline = crate::hang_deadline();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Already-exited swtpm means the port pair was taken; retry with a new one.
            if let Ok(Some(status)) = tpm.child.try_wait() {
                return Err(format!(
                    "swtpm on port {port} exited ({status}): {}",
                    std::fs::read_to_string(&log).unwrap_or_default().trim()
                ));
            }
            match Command::new("tpm2_getcap")
                .arg("properties-fixed")
                .env("TPM2TOOLS_TCTI", tpm.tcti())
                .output()
            {
                Ok(o) if o.status.success() => return Ok(tpm),
                Ok(o) => last = String::from_utf8_lossy(&o.stderr).into_owned(),
                Err(e) => last = format!("could not run tpm2_getcap: {e}"),
            }
        }
        Err(format!(
            "swtpm on port {port} never answered.\n--- swtpm log ---\n{}\n\
             --- swtpm_setup log ---\n{}\n--- last probe error ---\n{last}",
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
        format!("swtpm:host=127.0.0.1,port={}", self.port)
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
