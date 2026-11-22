//! The secret-free installation-media profile: the choices the Imager writes
//! onto a medium and the live installer reads back, as plain serde data.
//!
//! The app's resource document derives its model from these with the `model`
//! feature; the standalone media tools take the crate without it, so their
//! build resolves no document stack.

use serde::{Deserialize, Serialize};

/// Filesystem used for the installed system.
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
    #[default]
    Btrfs,
    Ext4,
    Xfs,
}

impl Filesystem {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Btrfs => "btrfs",
            Self::Ext4 => "ext4",
            Self::Xfs => "xfs",
        }
    }
    pub fn from_key(k: &str) -> Option<Self> {
        match k {
            "btrfs" => Some(Self::Btrfs),
            "ext4" => Some(Self::Ext4),
            "xfs" => Some(Self::Xfs),
            _ => None,
        }
    }
    pub fn all() -> [Self; 3] {
        [Self::Btrfs, Self::Ext4, Self::Xfs]
    }
}

/// The user account created during install. Only the names are authored
/// in the document; the password is entered on the target.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserAccount {
    /// Full name
    pub full_name: String,
    /// Username
    pub username: String,
    /// Held only for the current host-side UI session. It is deliberately
    /// excluded from the installation-media profile and must be entered on
    /// the target device.
    #[cfg_attr(feature = "model", model(skip))]
    pub password: String,
}

impl Default for UserAccount {
    fn default() -> Self {
        Self {
            full_name: "Tilo User".into(),
            username: "quser".into(),
            password: String::new(),
        }
    }
}

/// Public expectation for an opaque, password-encrypted preparation package.
/// Its zero default is invalid and grants no prepared setup authority.
#[cfg_attr(
    feature = "model",
    derive(tilo_resource::Model, tilo_resource::JsonSchema)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPreparedSetup {
    pub schema: u32,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: String,
}
impl MediaPreparedSetup {
    pub fn valid(&self) -> bool {
        crate::PreparedSetupExpectation {
            schema: self.schema,
            sealed_bytes: self.sealed_bytes,
            ciphertext_sha256: self.ciphertext_sha256.clone(),
        }
        .valid()
    }
}

/// The complete host-side customization draft. [`InstallProfile::to_media_profile_json`]
/// emits only its validated, non-secret preferences; target selection,
/// destructive consent, credentials, and recovery acknowledgement stay local
/// to the device being installed.
///
/// The document holds the secret-free choices the media profile and the
/// on-write setup carry; credentials are not part of that contract and
/// stay on this machine.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstallProfile {
    /// Same OS, with ordinary computer or always-available server defaults.
    #[serde(default = "crate::default_purpose")]
    pub purpose: String,
    /// Hostname
    pub hostname: String,
    pub user: UserAccount,
    /// Locale
    pub locale: String,
    /// Keyboard layout
    pub keyboard: String,
    /// Time zone
    pub timezone: String,
    /// Filesystem
    pub filesystem: Filesystem,
    /// Host-session draft value only; never serialized into installation media.
    #[cfg_attr(feature = "model", model(skip))]
    pub encryption_passphrase: String,
    /// Wi-Fi network the new machine should join. The passphrase stays in the
    /// write request, never in this collaborative profile.
    #[serde(default)]
    pub wifi_ssid: String,
    /// Name of the device identity created for the target.
    #[serde(default)]
    pub device_label: String,
    /// Enroll as a new device: certify a new device under the setup
    /// machine's person and place it on the target. The human root secret
    /// stays on the setup machine.
    #[serde(default)]
    pub enroll_local_identity: bool,
    /// Public ciphertext binding carried by the reviewed setup profile.
    /// The writer independently requires the held matching package; this
    /// descriptive value grants no identity or source access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_setup: Option<MediaPreparedSetup>,
}

/// The hostname a profile starts with. A profile that still carries it has not
/// chosen one, so a written disk keeps the image's own name.
pub const DEFAULT_HOSTNAME: &str = "tilo";

impl Default for InstallProfile {
    fn default() -> Self {
        Self {
            purpose: crate::default_purpose(),
            hostname: DEFAULT_HOSTNAME.into(),
            user: UserAccount::default(),
            locale: "en_US.UTF-8".into(),
            keyboard: "us".into(),
            timezone: "UTC".into(),
            filesystem: Filesystem::Btrfs,
            encryption_passphrase: String::new(),
            wifi_ssid: String::new(),
            device_label: String::new(),
            enroll_local_identity: false,
            prepared_setup: None,
        }
    }
}

impl InstallProfile {
    /// Validate the exact secret-free subset accepted by the live installer,
    /// with the rules it applies, so a bad profile is refused before
    /// flashing. The live installer repeats every check across the trust
    /// boundary.
    pub fn validate_media_profile(&self) -> Result<(), String> {
        use crate as rules;
        if !rules::valid_purpose(&self.purpose) {
            return Err("Choose computer or server purpose.".into());
        }
        if !rules::valid_device_label(&self.device_label) {
            return Err("Device name must be at most 64 bytes without control characters.".into());
        }
        if !rules::valid_hostname(&self.hostname) {
            return Err(
                "Hostname must be 1–63 letters, numbers or hyphens, without a hyphen at either end."
                    .into(),
            );
        }
        if !rules::valid_username(self.user.username.trim()) {
            return Err(
                "Username must be a non-reserved Linux account name of at most 32 characters."
                    .into(),
            );
        }
        if !rules::valid_full_name(self.user.full_name.trim()) {
            return Err("Full name must be at most 100 characters and contain no control characters or colon."
                .into());
        }
        if !rules::valid_locale(self.locale.trim()) {
            return Err("Choose a language and locale supported by Tilo.".into());
        }
        if !rules::KEYBOARDS.contains(&self.keyboard.trim()) {
            return Err("Keyboard layout is not supported by the Tilo installer.".into());
        }
        if !rules::valid_timezone_name(self.timezone.trim()) {
            return Err("Time zone must be a safe IANA name such as Europe/Berlin.".into());
        }
        if self
            .prepared_setup
            .as_ref()
            .is_some_and(|receipt| !receipt.valid())
        {
            return Err("Prepared setup expectation is invalid.".into());
        }
        Ok(())
    }

    /// Secret-free configuration embedded into Tilo installation media.
    ///
    /// Destructive choices and credentials deliberately do not cross the
    /// host/target boundary. The target installer consumes these validated
    /// preferences, then asks only for the target disk, install method,
    /// account/startup secret, and recovery-key acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the bounded media profile cannot be
    /// encoded as JSON.
    pub fn to_media_profile_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        #[derive(Serialize)]
        struct MediaProfile<'a> {
            schema: u8,
            purpose: &'a str,
            hostname: &'a str,
            username: &'a str,
            full_name: &'a str,
            #[serde(skip_serializing_if = "str::is_empty")]
            device_label: &'a str,
            locale: &'a str,
            keyboard: &'a str,
            timezone: &'a str,
            filesystem: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            prepared_setup: &'a Option<MediaPreparedSetup>,
        }

        serde_json::to_vec(&MediaProfile {
            schema: 1,
            purpose: &self.purpose,
            hostname: self.hostname.trim(),
            username: self.user.username.trim(),
            full_name: self.user.full_name.trim(),
            device_label: &self.device_label,
            locale: self.locale.trim(),
            keyboard: self.keyboard.trim(),
            timezone: self.timezone.trim(),
            filesystem: self.filesystem.key(),
            prepared_setup: &self.prepared_setup,
        })
    }

    /// A named saved-profile snapshot for the "saved install profiles" list.
    pub fn sample_workstation() -> Self {
        InstallProfile {
            purpose: crate::default_purpose(),
            hostname: "tilo-workstation".into(),
            user: UserAccount {
                full_name: "Ada".into(),
                username: "ada".into(),
                password: "•••••••••".into(),
            },
            locale: "en_US.UTF-8".into(),
            keyboard: "us".into(),
            timezone: "Europe/Amsterdam".into(),
            filesystem: Filesystem::Btrfs,
            encryption_passphrase: "•••••••••".into(),
            wifi_ssid: String::new(),
            device_label: String::new(),
            enroll_local_identity: false,
            prepared_setup: None,
        }
    }

    /// Render the exact secret-free JSON embedded into Tilo media. The preview
    /// excludes host paths, target devices, passwords, disk selection, and
    /// destructive confirmation.
    pub fn to_headless_config(&self) -> String {
        self.to_media_profile_json()
            .ok()
            .and_then(|json| serde_json::from_slice::<serde_json::Value>(&json).ok())
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| "{\"error\":\"profile could not be encoded\"}".to_string())
    }
}
