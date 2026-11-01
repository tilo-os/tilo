//! What the installer on the live medium offers its wizard's controls, the
//! palette, agents, its console and the unattended recipe: one typed effect
//! vocabulary, each answered by `InstallerApp::apply`.
//!
//! A wizard button, a keyboard shortcut, the palette, a screen reader,
//! `tilo-control actions invoke`, a headless scenario and the `tilo-vm
//! scenario install` driver all end in the same handler. These are effects,
//! not document actions: installer state is local to the privileged live
//! session, never replicated, and several of them (install, reboot) act on
//! the machine.
//!
//! The console names an effect `<id>@installer`; an effect bound to one item
//! is named on that item: `select-disk@installer.disk:/dev/vdb`,
//! `select-region@installer.region:1`, `set-size@installer.storage` and
//! `begin@installer.review`.

use ui::tilo_resource::{self, Effect};

/// `provider` in `tilo-control` terms, and the id its root resource is
/// published at: the name the console socket answers under.
pub const PROVIDER: &str = "installer";

/// The review the destructive `begin` is offered on, named by the target it
/// formats.
pub const REVIEW: &str = "installer.review";

/// The capacity choice of a guided install beside another system, bound to
/// the free extent it divides.
pub const STORAGE: &str = "installer.storage";

/// One discovered disk: `installer.disk:/dev/vdb`.
pub fn disk_row(device: &str) -> String {
    format!("installer.disk:{device}")
}

/// One unallocated region of the selected disk, when more than one could
/// hold an alongside install.
pub fn region_row(index: usize) -> String {
    format!("installer.region:{index}")
}

/// Everything the installer can be asked to do.
///
/// Not `Debug`: passphrase and network effects carry secrets, and a derived
/// `Debug` would print them into any log that formats one.
#[derive(Clone, PartialEq, Eq, Effect)]
#[effect(operation = "imager.destination")]
pub enum DestinationEffect {
    /// Installer status.
    ///
    /// The current step, every choice made so far, progress and what blocks
    /// the next step. Never includes a passphrase or the recovery key.
    #[effect(returns, kind = "read")]
    Status,
    /// Next.
    ///
    /// Validate this step and move to the next one, exactly as the wizard's
    /// Next button does.
    Next,
    /// Back.
    Back,
    /// Return to the existing system.
    ///
    /// Leave the installer without changing this disk and restart into the
    /// system it was staged from.
    ReturnToHost,
    /// Choose keyboard layout.
    SetLayout {
        #[param("Keyboard layout", choices = imager_backend::destination::layouts::display_names())]
        layout: String,
    },
    /// Refresh networks.
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
    /// Cancel sign-in.
    CancelSignIn,
    /// Continue offline.
    ContinueOffline,
    /// Choose install method.
    SetMethod {
        #[param(
            "erase: use the whole disk; alongside: keep existing partitions",
            choices = ["erase", "alongside"]
        )]
        method: String,
    },
    /// Refresh disks.
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
    /// Use N GiB for Tilo.
    #[effect(row)]
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
    ///
    /// Sets the account password and, unless a separate disk secret is
    /// supplied, the startup secret. Held only in installer memory; never
    /// written to state, logs or replies.
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
    ///
    /// Replaces the disk startup secret without changing the account
    /// password. Held only in installer memory; never written to state, logs
    /// or replies.
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
    ///
    /// Only an explicit unattended installation offers it.
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
    ///
    /// Downloaded to this computer from the formats' publishers once
    /// it is online. You can turn it off in Settings.
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
    /// I will save my recovery key.
    SetRecoveryPromise {
        #[param(
            "A recovery key is shown once during install; it is the only backup if the TPM or passphrase is lost"
        )]
        acknowledged: bool,
    },
    /// Begin installation.
    ///
    /// Formats the reviewed target, then installs Tilo. Offered on the
    /// review, where the caller types the target's name to confirm it.
    #[effect(row, destructive, phrase = "Self::target_name")]
    Begin {
        #[param("The reviewed target the installation formats")]
        target: String,
        #[param("The reviewed free-space plan, for an install beside another system")]
        allocation: String,
    },
    /// Copy recovery key.
    ///
    /// Copy the key to this live session's clipboard. The key itself is never
    /// returned to the caller.
    #[effect(own_device)]
    CopyRecoveryKey,
    /// I saved this key off this computer.
    ConfirmRecoverySaved {
        #[param("The recovery key is stored somewhere other than this computer")]
        saved: bool,
    },
    /// Reboot now.
    Reboot,
    /// Power off.
    PowerOff,
}

impl DestinationEffect {
    /// What the caller types to begin: the target it formats.
    fn target_name(&self) -> String {
        match self {
            Self::Begin { target, .. } if !target.trim().is_empty() => target.clone(),
            _ => "select a disk first".to_owned(),
        }
    }

    /// A short, secret-free name for logs and errors.
    pub fn name(&self) -> &'static str {
        self.id()
    }

    /// Where this effect is published: the item it is bound to, or the
    /// installer itself.
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

    /// The console's name for this effect: `<id>@<where it is published>`.
    pub fn console_id(&self) -> String {
        format!("{}@{}", self.id(), self.published_at())
    }
}

/// A reference to effect `id` of the vocabulary, for a status about it.
pub fn reference(id: &'static str) -> tilo_resource::ActionRef<DestinationEffect, ()> {
    tilo_resource::ActionRef::new(id, &[], DestinationEffect::kind())
}

/// The digest a bound effect carries of the reviewed `value` (a disk and
/// its free extent, a free-space plan): the handler refuses the effect when
/// what it was offered on has changed since.
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
