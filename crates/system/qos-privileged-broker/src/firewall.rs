//! The firewall applier — the trusted side of the `firewall-admin`
//! capability.
//!
//! This is the *only* place `firewall-cmd` is executed. The sandboxed app
//! computes the plan and asks the broker to apply it; the broker, having
//! passed the request through all three gates, hands the plan here. Because
//! the applier is a trait, the real `firewall-cmd` exec
//! ([`SystemFirewall`]) can be swapped for a recorder ([`RecordingFirewall`])
//! in a test — so the authorization path is exercised for real while the
//! system-mutating boundary is mocked.

use std::process::Command;

/// Applies a validated firewall plan. Implementors run the plan against the
/// real system or record it.
///
/// The plan is already shape-validated by [`crate::FirewallOp::validate_shape`]:
/// every line is a `firewall-cmd` invocation. An implementor may still fail
/// part-way (a command exits non-zero); it returns `Err` with the underlying
/// message and the system is left as the tool left it.
pub trait FirewallApplier {
    /// Apply every command in `plan` in order. Returns the number of commands
    /// run on success.
    fn apply(&mut self, plan: &[String]) -> Result<usize, String>;
}

/// Runs the plan against the host with `firewall-cmd`. The production
/// applier — used inside the compositor process, which is outside every app
/// sandbox.
#[derive(Debug, Default)]
pub struct SystemFirewall;

impl FirewallApplier for SystemFirewall {
    fn apply(&mut self, plan: &[String]) -> Result<usize, String> {
        let mut ran = 0;
        for line in plan {
            let mut parts = line.split_whitespace();
            // The shape gate guarantees the first token is `firewall-cmd`;
            // re-check defensively so this function is safe even if called
            // without the gate.
            let bin = parts.next().unwrap_or("");
            if bin != "firewall-cmd" {
                return Err(format!("refusing to run non-firewall-cmd line: {line:?}"));
            }
            let args: Vec<&str> = parts.collect();
            let status = Command::new(bin)
                .args(&args)
                .status()
                .map_err(|e| format!("spawn firewall-cmd: {e}"))?;
            if !status.success() {
                return Err(format!("`{line}` exited with {status}"));
            }
            ran += 1;
        }
        Ok(ran)
    }
}

/// Records the plans it was asked to apply instead of running anything. The
/// mock for the `nft`/`firewall-cmd` boundary in tests: the broker's gates
/// run for real, and this proves the applier was (or was not) reached.
#[derive(Debug, Default)]
pub struct RecordingFirewall {
    /// One entry per [`FirewallApplier::apply`] call, holding that call's
    /// plan. Empty means the applier was never reached — the assertion every
    /// deny-path test makes.
    pub applied: Vec<Vec<String>>,
    /// If set, [`FirewallApplier::apply`] returns this error instead of
    /// recording — to exercise the [`crate::BrokerResponse::Failed`] path.
    pub fail_with: Option<String>,
}

impl FirewallApplier for RecordingFirewall {
    fn apply(&mut self, plan: &[String]) -> Result<usize, String> {
        if let Some(err) = &self.fail_with {
            return Err(err.clone());
        }
        self.applied.push(plan.to_vec());
        Ok(plan.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_applier_counts_and_stores() {
        let mut fw = RecordingFirewall::default();
        let plan = vec![
            "firewall-cmd --reload".to_string(),
            "firewall-cmd --panic-off".to_string(),
        ];
        assert_eq!(fw.apply(&plan), Ok(2));
        assert_eq!(fw.applied, vec![plan]);
    }

    #[test]
    fn recording_applier_can_simulate_failure() {
        let mut fw = RecordingFirewall {
            fail_with: Some("firewalld not running".to_string()),
            ..Default::default()
        };
        assert_eq!(
            fw.apply(&["firewall-cmd --reload".to_string()]),
            Err("firewalld not running".to_string())
        );
        assert!(fw.applied.is_empty());
    }
}
