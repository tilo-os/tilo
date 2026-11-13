//! The installer's UI-free data model: plain data plus serde, round-tripped through
//! Automerge scalars.

use serde::{Deserialize, Serialize};
use tilo_resource::{Id, Identified, JsonSchema, Model};

/// Where the bytes to be written come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub enum SourceKind {
    #[default]
    IsoFile,
    /// Authenticated by the release catalog.
    TiloImage,
}

impl SourceKind {
    pub fn tag(&self) -> &'static str {
        match self {
            SourceKind::IsoFile => "LOCAL",
            SourceKind::TiloImage => "Tilo",
        }
    }
    pub fn long(&self) -> &'static str {
        match self {
            SourceKind::IsoFile => "Local disk image (unverified)",
            SourceKind::TiloImage => "Tilo system image",
        }
    }
}

/// A selected image source, with the metadata shown without reading the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub struct Source {
    pub kind: SourceKind,
    /// Absolute path to the image file (empty for a not-yet-downloaded known release).
    pub path: String,
    /// Display name (file name or release name).
    #[model(label)]
    pub name: String,
    /// Size in bytes (0 when not yet known / not downloaded).
    pub size: u64,
    /// Volume label or image variant ("Tilo 1.0 Live", "Fedora-Workstation"…).
    pub label: String,
    /// Whether this source can drive a full Tilo install (raw ISO write only otherwise).
    pub installable: bool,
    /// Stable identity of the reviewed file; the writer rejects a path that now resolves elsewhere.
    #[serde(default)]
    pub identity: Option<SourceIdentity>,
    /// Digest bound into a signed release catalog. A local arbitrary image has no such anchor, so it
    /// relies on identity and exact size.
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

impl Source {
    pub fn id(&self) -> String {
        let material = self
            .expected_sha256
            .as_deref()
            .filter(|s| !s.is_empty())
            .map_or_else(
                || {
                    self.identity.map_or_else(
                        || format!("{}\u{1f}{}\u{1f}{}", self.kind.tag(), self.path, self.name),
                        |identity| {
                            format!(
                                "{}\u{1f}{}:{}",
                                self.kind.tag(),
                                identity.volume,
                                identity.file
                            )
                        },
                    )
                },
                |digest| format!("{}\u{1f}{digest}", self.kind.tag()),
            );
        // FNV-1a names the source; the writer rechecks identity and digest at the privileged boundary.
        let hash = material
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |acc, byte| {
                (acc ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        format!("imager.source:{hash:016x}")
    }
}

/// An image reviewed earlier, keyed by source id so concurrent choices keep both records.
#[derive(Clone, Debug, PartialEq, Eq, Model)]
pub struct RecentSource {
    /// The source's [`Source::id`].
    pub id: Id,
    /// Most recent first; concurrent choices of one rank sort by id.
    pub rank: i64,
    /// Image
    pub source: Source,
}

impl Identified for RecentSource {
    fn id(&self) -> Id {
        self.id.clone()
    }

    /// Most recent first; concurrent choices of one rank keep id order.
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.rank.cmp(&other.rank)
    }
}

/// A file's platform identity, kept across the helper request boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub struct SourceIdentity {
    pub volume: u64,
    pub file: u64,
}

impl SourceIdentity {
    /// Reads identity from the open `file`, never by reopening its path. Windows pairs the volume
    /// serial with the 64-bit file index; an unavailable identity is `Ok(None)`.
    pub fn from_file(file: &std::fs::File) -> std::io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            Ok(Some(Self {
                volume: metadata.dev(),
                file: metadata.ino(),
            }))
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
            };

            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Some(Self {
                volume: u64::from(info.dwVolumeSerialNumber),
                file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            }))
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            let _ = file;
            Ok(None)
        }
    }
}

fn generic_hardware_profile() -> String {
    "generic".into()
}

/// An authenticated release: compatibility needs both CPU architecture and board.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TiloRelease {
    pub name: String,
    pub version: String,
    pub size: u64,
    pub channel: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(default = "generic_hardware_profile")]
    pub hardware_profile: String,
    /// Empty until downloaded.
    pub local_path: String,
    /// Empty only in screenshot fixtures.
    #[serde(default)]
    pub url: String,
    /// SHA-256 of the downloaded `.img.zst` release asset.
    #[serde(default)]
    pub archive_sha256: String,
    /// Bounds an untrusted response.
    #[serde(default)]
    pub archive_size: u64,
    #[serde(default)]
    pub image_sha256: String,
    /// Authenticated with the media catalog's image digest.
    #[serde(default)]
    pub account_custody: Option<crate::account_admission::ConsumerCapability>,
    /// Authenticated with the same digest.
    #[serde(default)]
    pub home_archive: Option<crate::home_admission::ConsumerCapability>,
    /// Authenticated with the same digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applications: Option<crate::applications_admission::ConsumerCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_source: Option<crate::original_admission::ConsumerCapability>,
}

impl TiloRelease {
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.name, self.version);
        if self.hardware_profile == "generic" {
            name
        } else {
            format!("{name} ({} only)", self.hardware_profile)
        }
    }

    pub fn target_name(&self) -> TargetName {
        if self.hardware_profile != "generic" {
            return TargetName::Profile(self.hardware_profile.clone());
        }
        match self.architecture.as_str() {
            "aarch64" => TargetName::Arm64,
            "x86_64" => TargetName::X86_64,
            _ => TargetName::Media,
        }
    }

    fn target_key(&self) -> String {
        match self.target_name() {
            TargetName::Arm64 => "aarch64".to_owned(),
            TargetName::X86_64 => "x86-64".to_owned(),
            TargetName::Media => "media".to_owned(),
            TargetName::Profile(profile) => profile,
        }
    }

    pub fn selection_id(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.version, self.architecture, self.hardware_profile, self.image_sha256
        )
    }

    pub fn matches_source(&self, source: &Source) -> bool {
        source.kind == SourceKind::TiloImage
            && source.name == self.display_name()
            && (self.image_sha256.is_empty()
                || source.expected_sha256.as_deref() == Some(self.image_sha256.as_str()))
    }

    pub fn to_source(&self) -> Source {
        let authenticated = self.image_sha256.len() == 64
            && self
                .image_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        Source {
            kind: if authenticated {
                SourceKind::TiloImage
            } else {
                SourceKind::IsoFile
            },
            path: self.local_path.clone(),
            name: self.display_name(),
            size: self.size,
            label: format!(
                "Tilo {} ({}), {}",
                self.version,
                self.channel,
                self.target_key()
            ),
            installable: authenticated,
            identity: None,
            expected_sha256: authenticated.then(|| self.image_sha256.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    Usb,
    Nvme,
    Ssd,
    Hdd,
    SdCard,
    Other,
}

impl TargetKind {
    /// This alone grants no write eligibility.
    pub fn is_removable_class(&self) -> bool {
        matches!(self, Self::Usb | Self::SdCard)
    }

    pub fn tag(&self) -> &'static str {
        match self {
            TargetKind::Usb => "USB",
            TargetKind::Nvme => "NVMe",
            TargetKind::Ssd => "SSD",
            TargetKind::Hdd => "HDD",
            TargetKind::SdCard => "SD",
            TargetKind::Other => "DISK",
        }
    }
    pub fn long(&self) -> &'static str {
        match self {
            TargetKind::Usb => "Removable USB drive",
            TargetKind::Nvme => "NVMe solid-state drive",
            TargetKind::Ssd => "Solid-state drive",
            TargetKind::Hdd => "Hard disk drive",
            TargetKind::SdCard => "SD / memory card",
            TargetKind::Other => "Block device",
        }
    }
    pub fn from_lsblk(rota: bool, tran: Option<&str>, removable: bool) -> Self {
        match tran {
            Some("usb") => TargetKind::Usb,
            Some("nvme") => TargetKind::Nvme,
            Some("mmc") => TargetKind::SdCard,
            // "sata", "ata" and anything unrecognised classify the same way.
            // transport at all) share the same removable/rotational
            // classification.
            _ => {
                if removable {
                    TargetKind::Usb
                } else if rota {
                    TargetKind::Hdd
                } else {
                    TargetKind::Ssd
                }
            }
        }
    }
}

/// A partition on a target drive, shown so the erasure is visible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetPartition {
    pub name: String,
    pub size: u64,
    pub fstype: Option<String>,
    pub label: Option<String>,
}

impl TargetPartition {
    pub fn display(&self) -> String {
        let label = self
            .label
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.fstype.clone())
            .unwrap_or_else(|| "—".into());
        format!("{}  {}", label, fmt_size(self.size))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetDrive {
    pub name: String,
    pub model: String,
    /// Hardware identity (WWN, serial, UniqueId), checked for substitution before a destructive open.
    #[serde(default)]
    pub stable_id: Option<String>,
    pub size: u64,
    pub kind: TargetKind,
    pub tran: Option<String>,
    pub removable: bool,
    /// The root disk: media creation never accepts it, and internal install belongs to the booted
    /// installer.
    pub is_system: bool,
    pub partitions: Vec<TargetPartition>,
}

impl From<&TargetDrive> for TargetDrive {
    fn from(value: &TargetDrive) -> Self {
        value.clone()
    }
}

impl TargetDrive {
    /// Whether the desktop imager may offer this drive. Some USB readers report `RM=0`, so the
    /// transport counts too, and system disks are always excluded.
    pub fn is_media_target(&self) -> bool {
        !self.is_system
            && (self.removable
                || self.tran.as_deref().is_some_and(|transport| {
                    matches!(
                        transport.to_ascii_lowercase().as_str(),
                        "usb" | "sd" | "mmc"
                    )
                }))
    }

    pub fn dev_path(&self) -> String {
        if self.name.starts_with('/') || self.name.starts_with(r"\\.\") {
            return self.name.clone();
        }
        #[cfg(target_os = "windows")]
        {
            return format!(r"\\.\{}", self.name);
        }
        #[cfg(target_os = "macos")]
        {
            // The raw character device bypasses the buffer cache; Apple documents it for imaging.
            format!("/dev/r{}", self.name)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            format!("/dev/{}", self.name)
        }
    }
    /// The name of each partition: its label, else its filesystem; a partition with neither adds
    /// no name.
    pub fn partition_names(&self) -> Vec<&str> {
        self.partitions
            .iter()
            .filter_map(|p| {
                p.label
                    .as_deref()
                    .filter(|label| !label.is_empty())
                    .or_else(|| p.fstype.as_deref().filter(|kind| !kind.is_empty()))
            })
            .collect()
    }
    pub fn fits(&self, image_size: u64) -> bool {
        image_size == 0 || image_size <= self.size
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Flash,
    Install,
}

impl Action {
    pub fn key(&self) -> &'static str {
        match self {
            Action::Flash => "flash",
            Action::Install => "install",
        }
    }
    pub fn from_key(k: &str) -> Action {
        match k {
            "install" => Action::Install,
            _ => Action::Flash,
        }
    }
}

/// One step of the guided flow: Source → Drive → Customize → Confirm → Write → Done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Source,
    Drive,
    Customize,
    Confirm,
    Write,
    Done,
}

impl Step {
    pub fn all() -> [Step; 6] {
        [
            Step::Source,
            Step::Drive,
            Step::Customize,
            Step::Confirm,
            Step::Write,
            Step::Done,
        ]
    }
    pub fn key(&self) -> &'static str {
        match self {
            Step::Source => "source",
            Step::Drive => "drive",
            Step::Customize => "customize",
            Step::Confirm => "confirm",
            Step::Write => "write",
            Step::Done => "done",
        }
    }
    pub fn from_key(k: &str) -> Step {
        match k {
            "drive" => Step::Drive,
            "customize" => Step::Customize,
            "confirm" => Step::Confirm,
            "write" => Step::Write,
            "done" => Step::Done,
            _ => Step::Source,
        }
    }
    pub fn index(&self) -> usize {
        match self {
            Step::Source => 0,
            Step::Drive => 1,
            Step::Customize => 2,
            Step::Confirm => 3,
            Step::Write => 4,
            Step::Done => 5,
        }
    }
    #[must_use]
    pub fn next(&self) -> Step {
        match self {
            Step::Source => Step::Drive,
            Step::Drive => Step::Customize,
            Step::Customize => Step::Confirm,
            Step::Confirm => Step::Write,
            Step::Write | Step::Done => Step::Done,
        }
    }
    #[must_use]
    pub fn prev(&self) -> Step {
        match self {
            Step::Source | Step::Drive => Step::Source,
            Step::Customize => Step::Drive,
            Step::Confirm => Step::Customize,
            Step::Write => Step::Confirm,
            Step::Done => Step::Write,
        }
    }
}

/// The machines a release starts on, as a surface names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetName {
    Arm64,
    X86_64,
    Media,
    Profile(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
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

/// The account created during install; only its names are stored, the password is typed on the
/// target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct UserAccount {
    /// Full name
    pub full_name: String,
    /// Username
    pub username: String,
    #[model(skip)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedSetupExpectation {
    pub schema: u32,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: String,
}
impl PreparedSetupExpectation {
    pub fn valid(&self) -> bool {
        install_profile_model::PreparedSetupExpectation {
            schema: self.schema,
            sealed_bytes: self.sealed_bytes,
            ciphertext_sha256: self.ciphertext_sha256.clone(),
        }
        .valid()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct InstallProfile {
    /// Same OS, with ordinary computer or always-available server defaults.
    #[serde(default = "install_profile_model::default_purpose")]
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
    #[model(skip)]
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
    /// The writer requires the matching held package; this value grants no access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_setup: Option<PreparedSetupExpectation>,
}

/// A profile that still carries this has chosen no hostname, so a written disk keeps the image's name.
pub const DEFAULT_HOSTNAME: &str = "tilo";

impl Default for InstallProfile {
    fn default() -> Self {
        Self {
            purpose: install_profile_model::default_purpose(),
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
    /// Checks the secret-free subset the live installer accepts, so a bad profile is refused
    /// before flashing; the installer repeats the checks.
    pub fn validate_media_profile(&self) -> Result<(), String> {
        use install_profile_model as rules;
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

    /// The secret-free configuration embedded into Tilo installation media. Destructive choices
    /// and credentials never cross the host/target boundary.
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
            prepared_setup: &'a Option<PreparedSetupExpectation>,
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

    pub fn sample_workstation() -> Self {
        InstallProfile {
            purpose: install_profile_model::default_purpose(),
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

    pub fn to_headless_config(
        &self,
        source: Option<&Source>,
        target: Option<&TargetDrive>,
    ) -> String {
        let _ = (source, target);
        self.to_media_profile_json()
            .ok()
            .and_then(|json| serde_json::from_slice::<serde_json::Value>(&json).ok())
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| "{\"error\":\"profile could not be encoded\"}".to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobPhase {
    Idle,
    Writing,
    Verifying,
    Success,
    Failed,
}

impl JobPhase {
    pub fn key(&self) -> &'static str {
        match self {
            JobPhase::Idle => "idle",
            JobPhase::Writing => "writing",
            JobPhase::Verifying => "verifying",
            JobPhase::Success => "success",
            JobPhase::Failed => "failed",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            JobPhase::Idle => "Ready",
            JobPhase::Writing => "Writing…",
            JobPhase::Verifying => "Verifying…",
            JobPhase::Success => "Complete",
            JobPhase::Failed => "Failed",
        }
    }
    pub fn from_key(k: &str) -> JobPhase {
        match k {
            "writing" => JobPhase::Writing,
            "verifying" => JobPhase::Verifying,
            "success" => JobPhase::Success,
            "failed" => JobPhase::Failed,
            _ => JobPhase::Idle,
        }
    }
    pub fn is_active(&self) -> bool {
        matches!(self, JobPhase::Writing | JobPhase::Verifying)
    }
}

/// Progress of the current job, 0..1 per phase.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub phase: JobPhase,
    pub write_frac: f64,
    pub verify_frac: f64,
    pub rate_bps: u64,
    /// The writer's own failure text, kept so a device or verification error does not collapse
    /// into a generic one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            phase: JobPhase::Idle,
            write_frac: 0.0,
            verify_frac: 0.0,
            rate_bps: 0,
            error: None,
        }
    }
}

impl Progress {
    /// Overall fraction: write is the first 85%, verify the rest.
    pub fn overall(&self) -> f64 {
        match self.phase {
            JobPhase::Idle => 0.0,
            JobPhase::Writing | JobPhase::Failed => self.write_frac * 0.85,
            JobPhase::Verifying => 0.85 + self.verify_frac * 0.15,
            JobPhase::Success => 1.0,
        }
    }

    pub fn eta_secs(&self, total: u64) -> Option<u64> {
        if self.rate_bps == 0 || !self.phase.is_active() {
            return None;
        }
        let frac = if self.phase == JobPhase::Verifying {
            self.verify_frac
        } else {
            self.write_frac
        };
        let remaining = (total as f64 * (1.0 - frac)).max(0.0);
        Some((remaining / self.rate_bps as f64).round() as u64)
    }
}

/// `m:ss`, or `Ns` under a minute.
pub fn fmt_eta(secs: u64) -> String {
    if secs >= 60 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// A USB stick as flash target and an internal `NVMe` as install target.
pub fn sample_targets() -> Vec<TargetDrive> {
    vec![
        TargetDrive {
            name: "sdb".into(),
            model: "SanDisk Ultra USB 3.0".into(),
            stable_id: Some("demo-sandisk-ultra".into()),
            size: 30_752_000_000,
            kind: TargetKind::Usb,
            tran: Some("usb".into()),
            removable: true,
            is_system: false,
            partitions: vec![TargetPartition {
                name: "sdb1".into(),
                size: 30_745_000_000,
                fstype: Some("exfat".into()),
                label: Some("ULTRA".into()),
            }],
        },
        TargetDrive {
            name: "nvme0n1".into(),
            model: "Samsung SSD 990 PRO 1TB".into(),
            stable_id: Some("demo-samsung-990".into()),
            size: 1_000_204_886_016,
            kind: TargetKind::Nvme,
            tran: Some("nvme".into()),
            removable: false,
            is_system: false,
            partitions: vec![
                TargetPartition {
                    name: "nvme0n1p1".into(),
                    size: 536_870_912,
                    fstype: Some("vfat".into()),
                    label: Some("EFI".into()),
                },
                TargetPartition {
                    name: "nvme0n1p2".into(),
                    size: 999_650_000_000,
                    fstype: Some("btrfs".into()),
                    label: Some("fedora".into()),
                },
            ],
        },
    ]
}

pub fn sample_releases() -> Vec<TiloRelease> {
    vec![
        TiloRelease {
            name: "Tilo".into(),
            version: "1.0".into(),
            size: 2_415_919_104,
            channel: "stable".into(),
            architecture: "x86_64".into(),
            hardware_profile: "generic".into(),
            local_path: "/var/lib/tilo/images/tilo-1.0-x86_64.iso".into(),
            url: String::new(),
            archive_sha256: String::new(),
            archive_size: 0,
            image_sha256: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            account_custody: None,
            home_archive: None,
            applications: None,
            original_source: None,
        },
        TiloRelease {
            name: "Tilo".into(),
            version: "1.1-rc2".into(),
            size: 2_499_805_184,
            channel: "testing".into(),
            architecture: "x86_64".into(),
            hardware_profile: "generic".into(),
            local_path: String::new(),
            url: String::new(),
            archive_sha256: String::new(),
            archive_size: 0,
            image_sha256: "2222222222222222222222222222222222222222222222222222222222222222".into(),
            account_custody: None,
            home_archive: None,
            applications: None,
            original_source: None,
        },
    ]
}

pub fn sample_source() -> Source {
    sample_releases().first().map_or_else(
        || Source {
            kind: SourceKind::TiloImage,
            path: String::new(),
            name: String::new(),
            size: 0,
            label: String::new(),
            installable: false,
            identity: None,
            expected_sha256: None,
        },
        TiloRelease::to_source,
    )
}

pub fn sample_recent_sources() -> Vec<Source> {
    vec![
        sample_source(),
        Source {
            kind: SourceKind::IsoFile,
            path: "/home/user/Downloads/Fedora-Workstation-Live-42.iso".into(),
            name: "Fedora-Workstation-Live-42.iso".into(),
            size: 2_113_929_216,
            label: "Fedora-WS-Live-42-1-1".into(),
            installable: false,
            identity: None,
            expected_sha256: None,
        },
        Source {
            kind: SourceKind::IsoFile,
            path: "/home/user/Downloads/archlinux-2026.06.01-x86_64.iso".into(),
            name: "archlinux-2026.06.01-x86_64.iso".into(),
            size: 1_181_116_006,
            label: "ARCH_202606".into(),
            installable: false,
            identity: None,
            expected_sha256: None,
        },
    ]
}

/// SI decimal units, as drive vendors label capacities; zero reads `—`.
pub fn fmt_size(bytes: u64) -> String {
    if bytes == 0 {
        "—".to_string()
    } else {
        bytes_si(bytes)
    }
}

pub fn fmt_rate(bps: u64) -> String {
    if bps == 0 {
        return "—".into();
    }
    format!("{}/s", fmt_size(bps))
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_identity_follows_the_file_across_handles_and_renames() {
        #[cfg(any(unix, windows))]
        {
            use super::SourceIdentity;
            let directory = tempfile::tempdir().unwrap();
            let original = directory.path().join("original.img");
            let renamed = directory.path().join("renamed.img");
            std::fs::write(&original, b"same bytes").unwrap();
            let file = std::fs::File::open(&original).unwrap();
            let identity = SourceIdentity::from_file(&file)
                .expect("read native file identity")
                .expect("native file identity");
            assert_eq!(
                SourceIdentity::from_file(&file.try_clone().unwrap())
                    .expect("read cloned file identity"),
                Some(identity)
            );
            std::fs::rename(&original, &renamed).unwrap();
            std::fs::write(&original, b"same bytes").unwrap();
            assert_eq!(
                SourceIdentity::from_file(&file).expect("read file identity"),
                Some(identity)
            );
            assert_eq!(
                SourceIdentity::from_file(&std::fs::File::open(&renamed).unwrap())
                    .expect("read renamed file identity"),
                Some(identity)
            );
            let replacement = SourceIdentity::from_file(&std::fs::File::open(&original).unwrap())
                .expect("read replacement file identity")
                .expect("replacement file identity");
            assert_ne!(replacement, identity);
        }

        #[cfg(any(unix, target_os = "windows"))]
        {
            let root = tempfile::tempdir().unwrap();
            let selected = root.path().join("selected.img");
            let renamed = root.path().join("renamed.img");
            std::fs::write(&selected, b"same image bytes").unwrap();
            let held = std::fs::File::open(&selected).unwrap();
            let identity = SourceIdentity::from_file(&held)
                .unwrap()
                .expect("platform exposes file identity");

            std::fs::rename(&selected, &renamed).unwrap();
            std::fs::write(&selected, b"same image bytes").unwrap();
            let replacement = std::fs::File::open(&selected).unwrap();
            assert_eq!(
                held.metadata().unwrap().len(),
                replacement.metadata().unwrap().len()
            );
            assert_eq!(SourceIdentity::from_file(&held).unwrap(), Some(identity));
            assert_eq!(
                SourceIdentity::from_file(&std::fs::File::open(&renamed).unwrap()).unwrap(),
                Some(identity)
            );
            assert_ne!(
                SourceIdentity::from_file(&replacement).unwrap(),
                Some(identity)
            );
        }
    }

    use super::*;

    #[test]
    fn samples_releases_and_targets_are_classified_truthfully() {
        {
            let t = sample_targets();
            assert_eq!(t.len(), 2);
            assert_eq!(t[0].kind, TargetKind::Usb);
            assert!(t[0].removable);
            assert_eq!(t[1].kind, TargetKind::Nvme);
            assert!(!t[1].removable);
        }

        {
            let s = sample_source();
            assert_eq!(s.kind, SourceKind::TiloImage);
            assert!(s.installable);
            assert!(s.size > 0);
        }

        {
            let mut release = sample_releases().remove(0);
            release.image_sha256.clear();
            let source = release.to_source();
            assert_eq!(source.kind, SourceKind::IsoFile);
            assert!(!source.installable);
            assert!(source.expected_sha256.is_none());
        }

        {
            let usb = &sample_targets()[0];
            assert!(usb.fits(2_000_000_000));
            assert!(!usb.fits(40_000_000_000));
            assert!(usb.fits(0)); // unknown size always "fits" for the check
        }

        {
            assert_eq!(fmt_size(0), "—");
            assert_eq!(fmt_size(512), "512 B");
            assert_eq!(fmt_size(1_000_000_000_000), "1 TB");
            assert!(fmt_size(2_415_919_104).ends_with("GB"));
        }

        {
            assert_eq!(
                TargetKind::from_lsblk(false, Some("usb"), true),
                TargetKind::Usb
            );
            assert_eq!(
                TargetKind::from_lsblk(false, Some("nvme"), false),
                TargetKind::Nvme
            );
            assert_eq!(
                TargetKind::from_lsblk(true, Some("sata"), false),
                TargetKind::Hdd
            );
            assert_eq!(
                TargetKind::from_lsblk(false, Some("sata"), false),
                TargetKind::Ssd
            );
            assert_eq!(
                TargetKind::from_lsblk(false, Some("mmc"), true),
                TargetKind::SdCard
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_profile_serializes_migrates_and_validates_the_installer_contract() {
        {
            let prof = InstallProfile::sample_workstation();
            let src = sample_source();
            let tgt = sample_targets()[1].clone();
            let cfg = prof.to_headless_config(Some(&src), Some(&tgt));
            assert!(cfg.contains("\"schema\": 1"));
            assert!(cfg.contains("\"hostname\": \"tilo-workstation\""));
            assert!(cfg.contains("\"username\": \"ada\""));
            assert!(!cfg.contains(&src.path));
            assert!(!cfg.contains(&tgt.dev_path()));
            assert!(!cfg.contains("password"));
            assert!(!cfg.contains("passphrase"));
            assert!(!cfg.contains(&prof.user.password) || prof.user.password.contains('•'));
        }

        {
            for filesystem in Filesystem::all() {
                let profile = InstallProfile {
                    filesystem,
                    ..InstallProfile::default()
                };
                let value: serde_json::Value =
                    serde_json::from_slice(&profile.to_media_profile_json().unwrap()).unwrap();
                assert_eq!(value["filesystem"], filesystem.key());
            }
        }

        {
            let good = InstallProfile::default();
            good.validate_media_profile().unwrap();

            let mut cases = Vec::new();
            let mut profile = good.clone();
            profile.hostname = "bad host".into();
            cases.push(("hostname", profile));
            let mut profile = good.clone();
            profile.user.username = "root".into();
            cases.push(("username", profile));
            let mut profile = good.clone();
            profile.user.full_name = "Bad:Name".into();
            cases.push(("full name", profile));
            let mut profile = good.clone();
            profile.locale = "zz_ZZ.UTF-8".into();
            cases.push(("locale", profile));
            let mut profile = good.clone();
            profile.keyboard = "not-a-layout".into();
            cases.push(("keyboard", profile));
            let mut profile = good;
            profile.timezone = "../../etc".into();
            cases.push(("time zone", profile));

            for (field, profile) in cases {
                let message = profile
                    .validate_media_profile()
                    .expect_err("invalid producer field must be rejected before writing");
                assert!(
                    message.to_ascii_lowercase().contains(field),
                    "{field} produced an unhelpful error: {message}"
                );
            }
        }

        {
            let prof = InstallProfile::sample_workstation();
            let json = serde_json::to_string(&prof).unwrap();
            let back: InstallProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(back.hostname, prof.hostname);
            assert_eq!(back.user.username, prof.user.username);
            assert_eq!(back.filesystem, prof.filesystem);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_and_progress_state_advance() {
        {
            assert_eq!(Step::Source.next(), Step::Drive);
            assert_eq!(Step::Drive.next(), Step::Customize);
            assert_eq!(Step::Customize.next(), Step::Confirm);
            assert_eq!(Step::Confirm.next(), Step::Write);
            assert_eq!(Step::Write.next(), Step::Done);
            assert_eq!(Step::Done.next(), Step::Done, "last step saturates");
            assert_eq!(Step::Done.prev(), Step::Write);
            assert_eq!(Step::Write.prev(), Step::Confirm);
            assert_eq!(Step::Confirm.prev(), Step::Customize);

            assert_eq!(Step::Source.prev(), Step::Source, "first step saturates");
            assert_eq!(Step::all().len(), 6);
            for s in Step::all() {
                assert_eq!(Step::from_key(s.key()), s);
            }
        }

        {
            let idle = Progress::default();
            assert_eq!(idle.overall(), 0.0);
            assert_eq!(idle.eta_secs(1_000_000), None);
            let writing = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.5,
                verify_frac: 0.0,
                rate_bps: 100,
                error: None,
            };
            assert!((writing.overall() - 0.425).abs() < 1e-9);
            assert_eq!(writing.eta_secs(1000), Some(5));
            let done = Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            };
            assert_eq!(done.overall(), 1.0);
        }
    }
}

#[cfg(test)]
mod prepared_setup_binding_tests {
    use super::*;

    #[test]
    fn media_profile_carries_only_valid_ciphertext_binding_and_public_device_name() {
        let mut profile = InstallProfile::default();
        let legacy: serde_json::Value =
            serde_json::from_slice(&profile.to_media_profile_json().unwrap()).unwrap();
        assert!(legacy.get("prepared_setup").is_none());
        profile.device_label = "Travel laptop".into();
        profile.user.password = "private startup secret".into();
        profile.encryption_passphrase = "private disk secret".into();
        let expectation = PreparedSetupExpectation {
            schema: 1,
            sealed_bytes: 4096,
            ciphertext_sha256: "a".repeat(64),
        };
        profile.prepared_setup = Some(expectation.clone());
        profile.validate_media_profile().unwrap();
        let bytes = profile.to_media_profile_json().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["device_label"], "Travel laptop");
        assert_eq!(
            serde_json::from_value::<PreparedSetupExpectation>(value["prepared_setup"].clone())
                .unwrap(),
            expectation
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("private startup secret"));
        assert!(!text.contains("private disk secret"));
        profile.prepared_setup.as_mut().unwrap().ciphertext_sha256 = "A".repeat(64);
        assert!(profile.validate_media_profile().is_err());
        profile.prepared_setup.as_mut().unwrap().ciphertext_sha256 = "a".repeat(64);
        profile.prepared_setup.as_mut().unwrap().sealed_bytes = 40 * 1024 * 1024 + 1;
        assert!(profile.validate_media_profile().is_err());
    }
}

impl From<Progress> for crate::write::helper::HelperProgress {
    fn from(progress: Progress) -> Self {
        use crate::write::helper::HelperPhase;
        Self {
            phase: match progress.phase {
                JobPhase::Idle => HelperPhase::Idle,
                JobPhase::Writing => HelperPhase::Writing,
                JobPhase::Verifying => HelperPhase::Verifying,
                JobPhase::Success => HelperPhase::Success,
                JobPhase::Failed => HelperPhase::Failed,
            },
            write_frac: progress.write_frac,
            verify_frac: progress.verify_frac,
            rate_bps: progress.rate_bps,
            error: progress.error,
        }
    }
}

impl From<crate::write::helper::HelperProgress> for Progress {
    fn from(progress: crate::write::helper::HelperProgress) -> Self {
        use crate::write::helper::HelperPhase;
        Self {
            phase: match progress.phase {
                HelperPhase::Idle => JobPhase::Idle,
                HelperPhase::Writing => JobPhase::Writing,
                HelperPhase::Verifying => JobPhase::Verifying,
                HelperPhase::Success => JobPhase::Success,
                HelperPhase::Failed => JobPhase::Failed,
            },
            write_frac: progress.write_frac,
            verify_frac: progress.verify_frac,
            rate_bps: progress.rate_bps,
            error: progress.error,
        }
    }
}

fn bytes_si(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    #[allow(
        clippy::cast_precision_loss,
        reason = "display value rounded to one decimal place"
    )]
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len().saturating_sub(1) {
        value /= 1000.0;
        unit = unit.saturating_add(1);
    }
    let name = UNITS.get(unit).unwrap_or(&"");
    if unit == 0 {
        format!("{n} {name}")
    } else {
        // One decimal, dropped when zero (`1 kB`).
        let text = format!("{value:.1}");
        format!("{} {name}", text.strip_suffix(".0").unwrap_or(&text))
    }
}
