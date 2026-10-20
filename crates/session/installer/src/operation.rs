//! The installer's operations: the one payload type every door produces.
//!
//! A wizard button, a keyboard shortcut, the Ctrl+K palette, a screen
//! reader, `tilo-control actions invoke`, a headless scenario and the
//! `tilo-vm scenario install` driver all end in the same place: an
//! [`Operation`] encoded under [`OPERATION_TYPE`] by the `ActionSpec` the
//! installer publishes, decoded and applied by `InstallerApp`. There is no
//! second implementation of "Next" or "Begin installation" behind any of
//! them.
//!
//! These are effect operations, not document actions: installer state is
//! local to the privileged live session, never replicated, and several
//! operations (install, reboot) act on the machine rather than on a CRDT.

use serde::{Deserialize, Serialize};

/// `provider` in `tilo-control` terms: the name the console socket answers
/// under.
pub const PROVIDER: &str = "installer";

/// The single protocol type every installer operation is encoded under.
pub const OPERATION_TYPE: &str = "installer.operation";

/// Handler table for the host's publication check.
pub const HANDLERS: &[&str] = &[OPERATION_TYPE, ONLINE_ACCOUNT_OPERATION_TYPE];

/// Fixed online-account controls; private form fields never enter this payload.
pub const ONLINE_ACCOUNT_OPERATION_TYPE: &str = "installer.online-accounts-control";

/// Resource ids. They name wizard concerns, not screen positions, so an
/// address stays valid across layouts, themes and text scales.
pub const SESSION: &str = "installer";
pub const KEYBOARD: &str = "installer.keyboard";
pub const NETWORK: &str = "installer.network";
pub const STORAGE: &str = "installer.storage";
pub const ACCOUNT: &str = "installer.account";
pub const OPTIONS: &str = "installer.options";
pub const MEDIA: &str = "installer.media";
pub const REVIEW: &str = "installer.review";
pub const RECOVERY: &str = "installer.recovery";
pub const POWER: &str = "installer.power";

/// One discovered disk: `installer.disk:/dev/vdb`.
pub fn disk_resource(device: &str) -> String {
    format!("installer.disk:{device}")
}

/// One unallocated region of the selected disk, when more than one could
/// hold an alongside install.
pub fn region_resource(index: usize) -> String {
    format!("installer.region:{index}")
}

/// Everything the installer can be asked to do.
///
/// Not `Debug`: password, disk passphrase and network actions carry secrets, and a
/// derived `Debug` would print them into any log that formats an operation.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Report the wizard's state (never a secret).
    Status,
    Next,
    Back,
    SetKeyboardLayout {
        layout: String,
    },
    RefreshNetworks,
    SelectNetwork {
        ssid: String,
    },
    ConnectNetwork {
        #[serde(default)]
        password: Option<String>,
    },
    SetInstallMethod {
        method: String,
    },
    RefreshDisks,
    SelectDisk {
        device: String,
    },
    SelectFreeRegion {
        index: usize,
    },
    SetAlongsideSize {
        gib: i64,
        disk: Box<crate::disks::Disk>,
        extent: crate::disks::GuidedAlongsidePlan,
    },
    SetIdentity {
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        full_name: Option<String>,
    },
    SetPassphrase {
        passphrase: String,
        confirmation: String,
    },
    SetDiskPassphrase {
        passphrase: String,
        confirmation: String,
        /// Empty keeps the interactive policy: a short TPM PIN only when the
        /// reviewed target proves the measured-boot boundary, otherwise a
        /// strong passphrase. `tpm2` requires that boundary; `passphrase`
        /// forces the independent strong passphrase even on a TPM2 machine.
        #[serde(default)]
        mode: String,
    },
    /// Set an account password that is already a SHA-512 crypt hash, without
    /// keeping or transmitting the plaintext. Only the unattended recipe path
    /// publishes this action.
    SetPasswordHash {
        hash: String,
    },
    SetFilesystem {
        filesystem: String,
    },
    SetSecurityKey {
        enabled: bool,
    },
    SetMediaCodecs {
        enabled: bool,
    },
    SetHostname {
        hostname: String,
    },
    SetLocale {
        locale: String,
    },
    SetTimezone {
        timezone: String,
    },
    SetRecoveryPromise {
        acknowledged: bool,
    },
    /// Start the install onto the reviewed target. `target` is the exact
    /// object the published confirmation phrase named; an install whose
    /// target has since changed is refused rather than redirected.
    BeginInstall {
        target: String,
        /// The exact append-only geometry displayed when this action was published.
        #[serde(default)]
        allocation: Option<crate::disks::GuidedAlongsidePlan>,
    },
    CopyRecoveryKey,
    ConfirmRecoverySaved {
        saved: bool,
    },
    Reboot,
    PowerOff,
    /// Leave a staged (USB-free) installer and restart into the operating
    /// system it was staged from. Firmware already consumed the one-shot
    /// `BootNext`, so a plain restart lands there.
    ReturnToHost,
}

impl Operation {
    /// A short, secret-free name for logs and errors.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Next => "next",
            Self::Back => "back",
            Self::SetKeyboardLayout { .. } => "set-layout",
            Self::RefreshNetworks => "refresh-networks",
            Self::SelectNetwork { .. } => "select-network",
            Self::ConnectNetwork { .. } => "connect-network",
            Self::SetInstallMethod { .. } => "set-method",
            Self::RefreshDisks => "refresh-disks",
            Self::SelectDisk { .. } => "select-disk",
            Self::SelectFreeRegion { .. } => "select-region",
            Self::SetAlongsideSize { .. } => "set-size",
            Self::SetIdentity { .. } => "set-identity",
            Self::SetPassphrase { .. } => "set-passphrase",
            Self::SetDiskPassphrase { .. } => "set-disk-passphrase",
            Self::SetPasswordHash { .. } => "set-password-hash",
            Self::SetFilesystem { .. } => "set-filesystem",
            Self::SetSecurityKey { .. } => "set-security-key",
            Self::SetMediaCodecs { .. } => "set-media-codecs",
            Self::SetHostname { .. } => "set-hostname",
            Self::SetLocale { .. } => "set-locale",
            Self::SetTimezone { .. } => "set-timezone",
            Self::SetRecoveryPromise { .. } => "set-recovery-promise",
            Self::BeginInstall { .. } => "begin",
            Self::CopyRecoveryKey => "copy-recovery-key",
            Self::ConfirmRecoverySaved { .. } => "confirm-recovery-saved",
            Self::Reboot => "reboot",
            Self::PowerOff => "power-off",
            Self::ReturnToHost => "return-to-host",
        }
    }

    /// Decode a payload produced by one of the installer's own specs.
    ///
    /// # Errors
    ///
    /// Returns a secret-free message when the payload is not an installer
    /// operation.
    pub fn decode(payload: &[u8]) -> Result<Self, String> {
        // Report the category only: serde's message can quote the offending
        // input, and that input may be a passphrase.
        serde_json::from_slice(payload).map_err(|error| {
            let category = match error.classify() {
                serde_json::error::Category::Io => "unreadable payload",
                serde_json::error::Category::Syntax => "malformed payload",
                serde_json::error::Category::Data => "unexpected fields or values",
                serde_json::error::Category::Eof => "truncated payload",
            };
            format!("not an installer operation: {category}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_round_trip_and_refusals_never_echo_secrets() {
        // Phase `operations_round_trip_and_unknown_fields_are_refused`.
        {
            for operation in [
                Operation::Next,
                Operation::SelectDisk {
                    device: "/dev/vdb".into(),
                },
                Operation::SetIdentity {
                    username: Some("ada".into()),
                    full_name: None,
                },
                Operation::BeginInstall {
                    target: "/dev/vdb".into(),
                    allocation: None,
                },
            ] {
                let bytes = serde_json::to_vec(&operation).unwrap();
                assert!(Operation::decode(&bytes).unwrap() == operation);
            }
            assert!(
                Operation::decode(br#"{"op":"select_disk","device":"/dev/vdb","extra":1}"#)
                    .is_err()
            );
            assert!(Operation::decode(br#"{"op":"format_everything"}"#).is_err());
        }
        // Phase `a_refused_payload_never_echoes_its_secret`.
        {
            let error = Operation::decode(
                br#"{"op":"set_passphrase","passphrase":"hunter2-correct-horse","confirmation":5}"#,
            )
            .err()
            .unwrap();
            assert!(!error.contains("hunter2"), "{error}");
        }
    }
}
