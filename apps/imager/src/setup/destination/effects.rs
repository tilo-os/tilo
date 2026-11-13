//! What the installer on the live medium offers its window: one effect per choice.

use ui::tilo_resource::{self, Effect};

pub const PROVIDER: &str = "installer";

pub const REVIEW: &str = "installer.review";

pub const STORAGE: &str = "installer.storage";

pub fn disk_row(device: &str) -> String {
    format!("installer.disk:{device}")
}

pub fn region_row(index: usize) -> String {
    format!("installer.region:{index}")
}

/// Everything the installer can be asked to do.
#[derive(Clone, PartialEq, Eq, Effect)]
#[effect(operation = "imager.destination")]
pub enum DestinationEffect {
    /// Installer status.
    #[effect(returns, kind = "read")]
    Status,
    /// Next.
    Next,
    /// Back.
    Back,
    /// Return to the existing system.
    ReturnToHost,
    /// Choose keyboard layout.
    SetLayout {
        #[param("Keyboard layout", choices = imager_backend::destination::layouts::display_names())]
        layout: String,
    },
    #[effect(label = "installer-action-refresh-networks")]
    RefreshNetworks,
    /// Choose Wi-Fi network.
    SelectNetwork {
        #[param("Network name")]
        ssid: String,
    },
    /// Connect to the chosen Wi-Fi network.
    ConnectNetwork {
        #[param(secret, "Wi-Fi password (secured networks only)")]
        password: Option<String>,
    },
    /// Sign in using the private form.
    AddAccount,
    /// Retry signing in using the private form.
    RetryAccount,
    #[effect(label = "installer-accounts-cancel")]
    CancelSignIn,
    #[effect(label = "imager-installer-continue-offline")]
    ContinueOffline,
    /// Choose install method.
    SetMethod {
        #[param(
            "erase: use the whole disk; alongside: keep existing partitions",
            choices = ["erase", "alongside"]
        )]
        method: String,
    },
    #[effect(label = "installer-action-refresh-disks")]
    RefreshDisks,
    /// Use this disk.
    #[effect(row)]
    SelectDisk {
        #[param("Device path of the disk")]
        device: String,
    },
    /// Use this region.
    #[effect(row)]
    SelectRegion {
        #[param("Index of the unallocated region")]
        index: i64,
    },
    #[effect(row)]
    #[effect(label = "installer-storage-use-capacity")]
    SetSize {
        #[param("Total Tilo capacity in whole GiB (at least 35)")]
        gib: i64,
        #[param("The reviewed disk and free extent the capacity divides")]
        extent: String,
    },
    /// Set username and name.
    SetIdentity {
        #[param("Up to 32 lowercase letters, digits, _ or -; starts with a letter or _")]
        username: Option<String>,
        #[param("Full name (optional)")]
        full_name: Option<String>,
    },
    /// Set account and default startup secret.
    SetPassphrase {
        #[param(
            secret,
            "Account password and startup secret: a TPM-protected PIN of at least 6 digits, or a passphrase of at least 15 characters"
        )]
        passphrase: String,
        #[param(secret, "The same secret again")]
        confirmation: String,
    },
    /// Set a separate disk startup secret.
    SetDiskPassphrase {
        #[param(
            secret,
            "Startup secret: a TPM-protected PIN of at least 6 digits, or a passphrase of at least 15 characters"
        )]
        passphrase: String,
        #[param(secret, "The same disk secret again")]
        confirmation: String,
        #[param(
            "`tpm2` requires the measured-boot boundary; `passphrase` forces an independent strong passphrase. Omit for the reviewed target's normal policy."
        )]
        mode: Option<String>,
    },
    /// Set a pre-hashed account password.
    SetPasswordHash {
        #[param(
            secret,
            "A canonical SHA-512 crypt hash, recorded by the installed system's normal account creation"
        )]
        hash: String,
    },
    /// Choose filesystem.
    SetFilesystem {
        #[param("Root filesystem", choices = install_profile_model::FILESYSTEMS)]
        filesystem: String,
    },
    /// Enroll a security key.
    SetSecurityKey {
        #[param("Enroll a FIDO2 security key as another unlock factor")]
        enabled: bool,
    },
    /// Install media codecs.
    SetMediaCodecs {
        #[param("Install more video and audio formats once the new system is online")]
        enabled: bool,
    },
    /// Set hostname.
    SetHostname {
        #[param("Letters, digits and hyphens; no leading or trailing hyphen; at most 63")]
        hostname: String,
    },
    /// Choose language and locale.
    SetLocale {
        #[param("Language and locale", choices = install_profile_model::locales())]
        locale: String,
    },
    /// Choose time zone.
    SetTimezone {
        #[param("IANA time zone")]
        timezone: String,
    },
    #[effect(label = "installer-action-recovery-promise")]
    SetRecoveryPromise {
        #[param(
            "A recovery key is shown once during install; it is the only backup if the TPM or passphrase is lost"
        )]
        acknowledged: bool,
    },
    /// Begin installation.
    #[effect(row, destructive, phrase = "Self::target_name")]
    Begin {
        #[param("The reviewed target the installation formats")]
        target: String,
        #[param("The reviewed free-space plan, for an install beside another system")]
        allocation: String,
    },
    #[effect(own_device)]
    #[effect(label = "installer-action-copy-recovery-key")]
    CopyRecoveryKey,
    #[effect(label = "installer-action-recovery-saved")]
    ConfirmRecoverySaved {
        #[param("The recovery key is stored somewhere other than this computer")]
        saved: bool,
    },
    #[effect(label = "installer-action-reboot")]
    Reboot,
    #[effect(label = "installer-action-power-off")]
    PowerOff,
}

impl DestinationEffect {
    fn target_name(&self) -> String {
        match self {
            Self::Begin { target, .. } if !target.trim().is_empty() => target.clone(),
            _ => "select a disk first".to_owned(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.id()
    }

    pub fn published_at(&self) -> String {
        match self {
            Self::SelectDisk { device } => disk_row(device),
            Self::SelectRegion { index } => {
                region_row(usize::try_from(*index).unwrap_or(usize::MAX))
            }
            Self::SetSize { .. } => STORAGE.to_owned(),
            Self::Begin { .. } => REVIEW.to_owned(),
            _ => PROVIDER.to_owned(),
        }
    }

    pub fn console_id(&self) -> String {
        format!("{}@{}", self.id(), self.published_at())
    }
}

pub fn reference(id: &'static str) -> tilo_resource::ActionRef<DestinationEffect, ()> {
    tilo_resource::ActionRef::new(id, &[], DestinationEffect::kind())
}

pub fn reviewed_digest<T: serde::Serialize>(value: &T) -> String {
    use sha2::Digest as _;
    serde_json::to_vec(value).map_or_else(
        |_| String::new(),
        |bytes| hex::encode(sha2::Sha256::digest(bytes)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_round_trip_and_refusals_never_echo_secrets() {
        for effect in [
            DestinationEffect::Next,
            DestinationEffect::SelectDisk {
                device: "/dev/vdb".into(),
            },
            DestinationEffect::SetIdentity {
                username: Some("ada".into()),
                full_name: None,
            },
            DestinationEffect::Begin {
                target: "/dev/vdb".into(),
                allocation: String::new(),
            },
        ] {
            let decoded = DestinationEffect::decode(effect.id(), &effect.arguments()).ok();
            assert!(decoded == Some(effect));
        }
        assert!(DestinationEffect::decode("format-everything", &serde_json::Map::new()).is_err());
        let refused = DestinationEffect::decode(
            "set-passphrase",
            serde_json::json!({ "passphrase": "hunter2-correct-horse", "confirmation": 5 })
                .as_object()
                .unwrap(),
        )
        .err()
        .unwrap();
        assert!(!format!("{refused:?}").contains("hunter2"), "{refused}");
    }

    #[test]
    fn begin_asks_for_the_target_it_formats() {
        let begin = DestinationEffect::Begin {
            target: "/dev/vdb".into(),
            allocation: String::new(),
        }
        .bound();
        assert_eq!(
            begin.contract.confirmation,
            ui::tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "/dev/vdb".into()
            }
        );
    }
}
