//! Shared `Command` helpers used by every per-domain module.
//!
//! Wrapping the `which`-check + `Command::output` flow once gives every
//! shell-out the same error vocabulary: `"<binary> not installed"` for a
//! missing binary, `"<binary> exited with status …: <stderr>"` for a
//! non-zero exit, `"<binary> spawn failed: …"` if `Command::output`
//! itself fails. The wrappers return `Result<_, String>` so call-sites
//! can hand the string straight to `tracing::warn!`.

use std::process::Command;

/// Probe whether `binary` is on `$PATH`. Pure `Command::new(binary)
/// .arg("--version")` would spawn the process, which is wasteful when we
/// only need a presence check — `which` is fast and returns 1 cleanly if
/// the binary is missing.
pub(crate) fn which(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run `binary args...` with no timeout, capturing stdout. Returns `Err`
/// with a message that includes the binary's name and any stderr the
/// command produced.
pub(crate) fn run_capture(binary: &str, args: &[&str]) -> Result<String, String> {
    if !which(binary) {
        return Err(format!("{binary} not installed"));
    }
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|e| format!("{binary} spawn failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "{binary} exited with status {:?}: {stderr}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{binary} stdout not utf-8: {e}"))
}

/// Run `binary args...` and discard stdout. Same error semantics as
/// [`run_capture`]. Used by setter shell-outs.
pub(crate) fn run(binary: &str, args: &[&str]) -> Result<(), String> {
    let _ = run_capture(binary, args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: setters that shell out should never panic when the
    /// underlying binary is missing — they must return a
    /// `"<binary> not installed"` error string.
    #[test]
    fn missing_binary_returns_not_installed_error() {
        let r = run("definitely-not-a-real-binary-qos-test", &["--help"]);
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("not installed"), "got: {msg}");
    }
}
