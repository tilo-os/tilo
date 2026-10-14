//! Private enrollment input on the existing authenticated administrator transport.
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// The unchanged console semantic operations carried by the authenticated admin route.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceActions {
    pub line: String,
}

impl std::fmt::Debug for DeviceActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceActions { line: [private] }")
    }
}

impl Drop for DeviceActions {
    fn drop(&mut self) {
        self.line.zeroize();
    }
}

/// Enroll only the machine's current encrypted root for sibling consent.
/// Paths, account ids, boot keys and key files are never accepted from a caller.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceUnlockEnrol {
    pub passphrase: String,
    #[serde(default)]
    pub networks: Vec<String>,
    /// Explicitly acknowledge readable device-factor/network material when no TPM exists.
    #[serde(default)]
    pub allow_unsealed: bool,
}
impl std::fmt::Debug for DeviceUnlockEnrol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceUnlockEnrol")
            .field("passphrase", &"[private]")
            .field("networks", &self.networks)
            .field("allow_unsealed", &self.allow_unsealed)
            .finish()
    }
}
impl Drop for DeviceUnlockEnrol {
    fn drop(&mut self) {
        self.passphrase.zeroize();
    }
}
impl DeviceUnlockEnrol {
    /// Validate before any privileged side effect, with content-free refusals.
    ///
    /// # Errors
    /// Refuses empty, oversized or invalid private input without reflecting it.
    pub fn validate(&self) -> Result<(), String> {
        if self.passphrase.is_empty()
            || self.passphrase.len() > 4096
            || self.passphrase.contains('\0')
        {
            return Err("A bounded existing disk passphrase is required".into());
        }
        if self.networks.len() > 8
            || self.networks.iter().any(|name| {
                name.is_empty() || name.len() > 64 || name.chars().any(char::is_control)
            })
        {
            return Err("Choose at most eight saved network names".into());
        }
        Ok(())
    }
}
/// Public enrollment facts; no factor, certificate private key or credential readback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceUnlockEnrolResult {
    pub volume_uuid: String,
    pub keyslot: u32,
    pub sealing: String,
    pub networks: Vec<String>,
    pub siblings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_input_is_private_bounded_and_cannot_select_another_volume_or_owner() {
        let request = DeviceUnlockEnrol {
            passphrase: "private-fixture-answer".into(),
            networks: vec![],
            allow_unsealed: false,
        };
        assert!(request.validate().is_ok());
        assert!(!format!("{request:?}").contains("private-fixture-answer"));
        for field in ["device", "session_uid", "key_file"] {
            let mut value = serde_json::to_value(&request).unwrap();
            value[field] = serde_json::json!("attacker");
            assert!(serde_json::from_value::<DeviceUnlockEnrol>(value).is_err());
        }
        let mut invalid = request;
        invalid.passphrase = "x".repeat(4097);
        assert!(invalid.validate().is_err());
        invalid.passphrase = "private-fixture-answer".into();
        invalid.networks = vec!["private-fixture-answer\n".into()];
        assert!(
            !invalid
                .validate()
                .unwrap_err()
                .contains("private-fixture-answer")
        );
    }
}
