//! What the installer on the live medium offers its window: one effect per choice.

use tilo_resource::{self, Effect};

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
    #[effect(returns, kind = "read", on = Session)]
    Status,
    /// Next.
    #[effect(kind = "outside", on = Session)]
    Next,
    /// Back.
    #[effect(kind = "outside", on = Session)]
    Back,
    /// Return to the existing system.
    #[effect(kind = "outside", on = Session)]
    ReturnToHost,
    /// Choose keyboard layout.
    #[effect(kind = "outside", on = Session)]
    SetLayout {
        #[param("Keyboard layout", choices = tilo_keyboard_map::display_names())]
        layout: String,
    },
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-refresh-networks", on = Session)]
    RefreshNetworks,
    /// Choose Wi-Fi network.
    #[effect(kind = "outside", on = Network)]
    SelectNetwork {
        #[param("Network name")]
        ssid: String,
    },
    /// Connect to the chosen Wi-Fi network.
    #[effect(kind = "outside", on = Session)]
    ConnectNetwork {
        #[param(secret, "Wi-Fi password (secured networks only)")]
        password: Option<String>,
    },
    /// Choose install method.
    #[effect(kind = "outside", on = Session)]
    SetMethod {
        #[param(
            "erase: use the whole disk; alongside: keep existing partitions",
            choices = ["erase", "alongside"]
        )]
        method: String,
    },
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-refresh-disks", on = Session)]
    RefreshDisks,
    /// Use this disk.
    #[effect(kind = "outside")]
    #[effect(row, on = Disk)]
    SelectDisk {
        #[param("Device path of the disk")]
        device: String,
    },
    /// Use this region.
    #[effect(kind = "outside")]
    #[effect(row, on = Region)]
    SelectRegion {
        #[param("Index of the unallocated region")]
        index: i64,
    },
    #[effect(kind = "outside")]
    #[effect(row)]
    #[effect(label = "installer-storage-use-capacity", on = Storage)]
    SetSize {
        #[param("Total Tilo capacity in whole GiB, at least the least room Tilo takes")]
        gib: i64,
        #[param("The reviewed disk and free extent the capacity divides")]
        extent: String,
    },
    /// Set username and name.
    #[effect(kind = "outside", on = Session)]
    SetIdentity {
        #[param("Up to 32 lowercase letters, digits, _ or -; starts with a letter or _")]
        username: Option<String>,
        #[param("Full name (optional)")]
        full_name: Option<String>,
    },
    /// Set account and default startup secret.
    #[effect(kind = "outside", on = Session)]
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
    #[effect(kind = "outside", on = Session)]
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
    #[effect(kind = "outside", on = Session)]
    SetPasswordHash {
        #[param(
            secret,
            "A canonical SHA-512 crypt hash, recorded by the installed system's normal account creation"
        )]
        hash: String,
    },
    /// Choose filesystem.
    #[effect(kind = "outside", on = Session)]
    SetFilesystem {
        #[param("Root filesystem", choices = install_profile_model::FILESYSTEMS)]
        filesystem: String,
    },
    /// Enroll a security key.
    #[effect(kind = "outside", on = Session)]
    SetSecurityKey {
        #[param("Enroll a FIDO2 security key as another unlock factor")]
        enabled: bool,
    },
    /// Install media codecs.
    #[effect(kind = "outside", on = Session)]
    SetMediaCodecs {
        #[param("Install more video and audio formats once the new system is online")]
        enabled: bool,
    },
    /// Set hostname.
    #[effect(kind = "outside", on = Session)]
    SetHostname {
        #[param("Letters, digits and hyphens; no leading or trailing hyphen; at most 63")]
        hostname: String,
    },
    /// Choose language and locale.
    #[effect(kind = "outside", on = Session)]
    SetLocale {
        #[param("Language and locale", choices = install_profile_model::locales())]
        locale: String,
    },
    /// Choose time zone.
    #[effect(kind = "outside", on = Session)]
    SetTimezone {
        #[param("IANA time zone")]
        timezone: String,
    },
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-recovery-promise", on = Session)]
    SetRecoveryPromise {
        #[param(
            "A recovery key is shown once during install; it is the only backup if the TPM or passphrase is lost"
        )]
        acknowledged: bool,
    },
    /// Begin installation.
    #[effect(kind = "outside")]
    #[effect(row, destructive, phrase = "Self::target_name", on = Review)]
    Begin {
        #[param("The reviewed target the installation formats")]
        target: String,
        #[param("The reviewed free-space plan, for an install beside another system")]
        allocation: String,
    },
    #[effect(kind = "outside")]
    #[effect(own_device)]
    #[effect(label = "installer-action-copy-recovery-key", on = Session)]
    CopyRecoveryKey,
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-recovery-saved", on = Session)]
    ConfirmRecoverySaved {
        #[param("The recovery key is stored somewhere other than this computer")]
        saved: bool,
    },
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-reboot", on = Session)]
    Reboot,
    #[effect(kind = "outside")]
    #[effect(label = "installer-action-power-off", on = Session)]
    PowerOff,
}

/// The installer on the live medium, as it publishes itself: every command
/// it offers while it is open.
#[derive(tilo_resource::Row)]
#[row(
    kind = "installer.session",
    label = "Install",
    effects = DestinationEffect
)]
pub struct Session;

/// One Wi-Fi network the installer can join.
#[derive(tilo_resource::Row)]
#[row(kind = "installer.network", label = "Network", effects = DestinationEffect)]
pub struct Network;

/// One disk the installer may write.
#[derive(tilo_resource::Row)]
#[row(kind = "installer.disk", label = "Disk", effects = DestinationEffect)]
pub struct Disk;

/// One unallocated region inside a disk.
#[derive(tilo_resource::Row)]
#[row(kind = "installer.region", label = "Install region", effects = DestinationEffect)]
pub struct Region;

/// The capacity a guided installation takes.
#[derive(tilo_resource::Row)]
#[row(kind = "installer.storage", label = "Storage", effects = DestinationEffect)]
pub struct Storage;

/// The review before the install begins.
#[derive(tilo_resource::Row)]
#[row(kind = "installer.review", label = "Review", effects = DestinationEffect)]
pub struct Review;

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

    tilo_test_env::scenario! {
        fn destination_effects_round_trip_and_name_their_target;

        phase effects_round_trip_and_refusals_never_echo_secrets;
        {
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

        phase begin_asks_for_the_target_it_formats;
        {
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
}
