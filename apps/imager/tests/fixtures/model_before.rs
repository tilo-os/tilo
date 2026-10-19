//! Pure data model for the Install Media (imager) app.
//!
//! Everything here is plain data + serde so the entire app state can be
//! round-tripped through Automerge scalars. Rendering, probing and the
//! write/install seam live in sibling modules — this file knows nothing
//! about vello, taffy or `lsblk`.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Model};

// ---------------------------------------------------------------------------
// Source — what we are going to write or install.
// ---------------------------------------------------------------------------

/// Where the bytes to be written come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub enum SourceKind {
    /// An arbitrary local disk image picked from disk.
    #[default]
    IsoFile,
    /// A Tilo installer/live image authenticated by the release catalog.
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

/// A selected image source with the metadata we can show without reading
/// the whole file (name, on-disk size, volume label / image type).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub struct Source {
    pub kind: SourceKind,
    /// Absolute path to the image file (empty for a not-yet-downloaded
    /// known release).
    pub path: String,
    /// Display name (file name or release name).
    #[model(label)]
    pub name: String,
    /// Size in bytes (0 when not yet known / not downloaded).
    pub size: u64,
    /// Volume label or image variant ("Tilo 1.0 Live", "Fedora-Workstation"…).
    pub label: String,
    /// Whether this source can drive a full Tilo install (vs. raw ISO write
    /// only).
    pub installable: bool,
    /// Stable identity of the file selected and reviewed by the user. The
    /// privileged writer rejects a path which now resolves to another file.
    #[serde(default)]
    pub identity: Option<SourceIdentity>,
    /// Digest bound into a signed release catalog. Local arbitrary images do
    /// not have an external trust anchor, so they rely on identity + exact size.
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

impl Source {
    /// Stable identity of a reviewed source. Prefer immutable digest, then
    /// platform identity, then the canonical path/name fallback.
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
        // FNV-1a is a stable identifier, not a security boundary. The writer
        // independently rechecks identity/digest at the privileged boundary.
        let hash = material
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |acc, byte| {
                (acc ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        format!("imager.source:{hash:016x}")
    }
}

/// An image reviewed before the current one, stored under its source's id
/// so two people choosing at once keep both records.
#[derive(Clone, Debug, PartialEq, Eq, Model)]
pub struct RecentSource {
    /// The source's [`Source::id`].
    pub id: Id,
    /// Where it sorts: the most recent has the lowest rank; equal ranks
    /// (concurrent choices) sort by id.
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

/// Platform file identity, portable across the helper request boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Model, JsonSchema)]
pub struct SourceIdentity {
    pub volume: u64,
    pub file: u64,
}

impl SourceIdentity {
    /// Capture identity from the held source file, never by reopening its path.
    /// Windows preserves the serialized volume serial / 64-bit file index pair;
    /// native query failures are errors, not an absent identity.
    pub fn from_file(file: &std::fs::File) -> std::io::Result<Option<Self>> {
        imager_backend::media_model::SourceIdentity::from_file(file).map(|identity| {
            identity.map(|identity| Self {
                volume: identity.volume,
                file: identity.file,
            })
        })
    }
}

fn generic_hardware_profile() -> String {
    "generic".into()
}

/// Authenticated release identity includes both CPU architecture and board.
/// Matching the host CPU alone does not establish device compatibility.
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
    /// Local path once available; empty means "needs download".
    pub local_path: String,
    /// Immutable release asset URL. Empty only in screenshot/demo fixtures.
    #[serde(default)]
    pub url: String,
    /// SHA-256 of the downloaded `.img.zst` release asset.
    #[serde(default)]
    pub archive_sha256: String,
    /// Exact compressed transport size, used to bound an untrusted response.
    #[serde(default)]
    pub archive_size: u64,
    /// SHA-256 of the expanded image that is written to the device.
    #[serde(default)]
    pub image_sha256: String,
    /// Exact target consumer proof authenticated with the media catalog's image digest.
    #[serde(default)]
    pub account_custody: Option<crate::setup::account_admission::ConsumerCapability>,
    /// Home archive consumers authenticated with the selected image digest.
    #[serde(default)]
    pub home_archive: Option<crate::setup::home_admission::ConsumerCapability>,
    /// Selected app-state consumers authenticated with the exact image digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applications: Option<crate::setup::applications_admission::ConsumerCapability>,
    /// Original-source consumer proof authenticated with the selected image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_source: Option<crate::setup::original_admission::ConsumerCapability>,
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

    pub fn target_label(&self) -> String {
        if self.hardware_profile != "generic" {
            return format!("{} only", self.hardware_profile);
        }
        match self.architecture.as_str() {
            "aarch64" => "Generic ARM64",
            "x86_64" => "Generic x86-64",
            _ => "Generic media",
        }
        .into()
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
                self.target_label()
            ),
            installable: authenticated,
            identity: None,
            expected_sha256: authenticated.then(|| self.image_sha256.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Target — the drive we write to.
// ---------------------------------------------------------------------------

/// Broad class of a block device (drives the icon + warning emphasis).
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
    /// The device class is removable; this alone grants no write eligibility.
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
            // "sata" / "ata" and anything unrecognised (including no
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

/// A partition currently on a target drive — shown so the user can see
/// exactly what will be erased.
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

/// A candidate write target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetDrive {
    pub name: String,
    pub model: String,
    /// Platform-provided hardware/media identity (WWN/serial/UniqueId) used
    /// to detect unplug/replug target substitution before a destructive open.
    #[serde(default)]
    pub stable_id: Option<String>,
    pub size: u64,
    pub kind: TargetKind,
    pub tran: Option<String>,
    pub removable: bool,
    /// `true` for the root/system disk. The media creator never accepts it;
    /// internal installation belongs to the booted Tilo installer.
    pub is_system: bool,
    pub partitions: Vec<TargetPartition>,
}

impl TargetDrive {
    /// The glyph of the drive: a USB stick, a memory card, or a drive.
    pub fn icon(&self) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self.tran.as_deref() {
            Some("usb") => Icon::Usb,
            Some("mmc" | "sd") => Icon::SdCard,
            _ if self.name.starts_with("mmcblk") => Icon::SdCard,
            _ => Icon::HardDrive,
        }
    }

    /// External-media eligibility is shared with native grant admission.
    pub fn is_media_target(&self) -> bool {
        imager_backend::media_model::TargetDrive::from(self).is_media_target()
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
            // The raw character device avoids the buffered `/dev/diskN` path
            // and is the interface Apple documents for imaging removable media.
            format!("/dev/r{}", self.name)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            format!("/dev/{}", self.name)
        }
    }
    /// Human description of what currently lives on the drive.
    pub fn contents_summary(&self) -> String {
        if self.partitions.is_empty() {
            return "no partitions".into();
        }
        let labels: Vec<String> = self
            .partitions
            .iter()
            .map(|p| {
                p.label
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or_else(|| p.fstype.clone())
                    .unwrap_or_else(|| "data".into())
            })
            .collect();
        match self.partitions.len() {
            1 => format!("1 partition ({})", labels.join(", ")),
            count => format!("{count} partitions ({})", labels.join(", ")),
        }
    }
    /// Does the image fit? (Raw writes need `image_size` ≤ drive size.)
    pub fn fits(&self, image_size: u64) -> bool {
        image_size == 0 || image_size <= self.size
    }
}

// ---------------------------------------------------------------------------
// Action — what to do with (source, target).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Raw `dd`-style copy of an image onto a drive, then verify. The plain
    /// "write this image to this drive" case (a USB stick or SD card).
    Flash,
    /// Full Tilo install onto the target, driven by an [`InstallProfile`] — the
    /// pre-configuration the Customize step collects is baked into the media.
    Install,
}

impl Action {
    pub fn key(&self) -> &'static str {
        match self {
            Action::Flash => "flash",
            Action::Install => "install",
        }
    }
    /// The plain-language name of the whole operation, for titles and reviews.
    pub fn title(&self) -> &'static str {
        match self {
            Action::Flash => "Write image",
            Action::Install => "Install Tilo",
        }
    }
    /// The imperative verb for the primary button and the step caption —
    /// deliberately short and unambiguous ("Write" vs "Install").
    pub fn verb(&self) -> &'static str {
        match self {
            Action::Flash => "Write",
            Action::Install => "Install",
        }
    }
    /// Past-tense form for the Done screen ("written" / "installed").
    pub fn past(&self) -> &'static str {
        match self {
            Action::Flash => "written",
            Action::Install => "installed",
        }
    }
    pub fn from_key(k: &str) -> Action {
        match k {
            "install" => Action::Install,
            _ => Action::Flash,
        }
    }
}

// ---------------------------------------------------------------------------
// Wizard flow — the ordered steps the app walks the user through.
// ---------------------------------------------------------------------------

/// One step of the guided flow. The app is a linear wizard with a visible,
/// six-station step indicator:
///
///   Source → Drive → Customize → Confirm → Write → Done
///
/// * **Confirm** is the last screen before anything destructive happens: a
///   read-only review of every choice plus the typed device-name gate.
/// * **Write** shows live progress; the read-back verify is a phase *within*
///   it (so "writing" and "verifying" share one station).
/// * **Done** is the terminal outcome (verified / failed) with a fresh-start
///   affordance.
///
/// Customize is skipped for a plain ISO (only a Tilo image carries a profile),
/// but it always occupies a station in the indicator so the flow reads the
/// same length every time.
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
    /// Zero-based order used for the step indicator + "is this step done" logic.
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
    /// Short step-indicator caption.
    pub fn title(&self) -> &'static str {
        match self {
            Step::Source => "Source",
            Step::Drive => "Drive",
            Step::Customize => "Customize",
            Step::Confirm => "Confirm",
            Step::Write => "Write",
            Step::Done => "Done",
        }
    }
    /// The next step in the flow (saturates at the last step).
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
    /// The previous step (saturates at the first step).
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

// ---------------------------------------------------------------------------
// Install options — the host-side draft whose validated, non-secret subset
// serialises to the installation-media preconfiguration profile.
// ---------------------------------------------------------------------------

/// Filesystem used for the installed system.
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
    pub fn label(&self) -> &'static str {
        match self {
            Self::Btrfs => "Btrfs (recommended)",
            Self::Ext4 => "Ext4",
            Self::Xfs => "XFS",
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Model)]
pub struct UserAccount {
    /// Full name
    pub full_name: String,
    /// Username
    pub username: String,
    /// Held only for the current host-side UI session. It is deliberately
    /// excluded from the installation-media profile and must be entered on
    /// the target device.
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
/// Its zero default is invalid and grants no prepared setup authority.
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

/// The complete host-side customization draft. [`InstallProfile::to_media_profile_json`]
/// emits only its validated, non-secret preferences; target selection,
/// destructive consent, credentials, and recovery acknowledgement stay local
/// to the device being installed.
///
/// The document holds the secret-free choices the media profile and the
/// on-write setup carry; credentials are not part of that contract and
/// stay on this machine.
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
    /// The writer independently requires the held matching package; this
    /// descriptive value grants no identity or source access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_setup: Option<PreparedSetupExpectation>,
}

/// The hostname a profile starts with. A profile that still carries it has not
/// chosen one, so a written disk keeps the image's own name.
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
    /// Validate the exact secret-free subset accepted by the live installer,
    /// with the rules it applies, so a bad profile is refused before
    /// flashing. The live installer repeats every check across the trust
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns a field-specific message when any transported value would be
    /// rejected by the live installer.
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

    /// A named saved-profile snapshot for the "saved install profiles" list.
    pub fn sample_workstation() -> Self {
        InstallProfile {
            purpose: install_profile_model::default_purpose(),
            hostname: "tilo-workstation".into(),
            user: UserAccount {
                full_name: "Quinten".into(),
                username: "quinten".into(),
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

// ---------------------------------------------------------------------------
// Progress / job state.
// ---------------------------------------------------------------------------

/// The phase of a write/install job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobPhase {
    Idle,
    Writing,
    Verifying,
    Success,
    Failed,
}

/// A persisted active phase cannot prove that a restarted UI still supervises
/// the privileged writer which produced it. Treat that state as unknown and
/// potentially incomplete until the user performs a fresh review.
pub const WRITE_SUPERVISION_LOST: &str = "Tilo Imager restarted before the previous write reported completion. Its outcome is unknown and the drive may be incomplete. Keep it connected until any writer activity stops, then rescan and review it before retrying.";

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

/// Live progress of the current job (0..1 each phase).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub phase: JobPhase,
    pub write_frac: f64,
    pub verify_frac: f64,
    /// Bytes per second of the active phase, for the ETA line.
    pub rate_bps: u64,
    /// Actionable terminal failure detail from the writer/helper. Kept on the
    /// progress event so the privileged boundary cannot collapse a useful
    /// device, authorization, or verification error into a generic failure.
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
    /// Record that no live supervisor can establish the job's outcome.
    pub fn mark_supervision_lost(&mut self) {
        self.phase = JobPhase::Failed;
        self.rate_bps = 0;
        self.error = Some(WRITE_SUPERVISION_LOST.into());
    }

    /// Fail closed when an active progress snapshot has outlived its local
    /// worker/supervisor. Returns whether a recovery transition was applied.
    pub fn fail_if_supervision_lost(&mut self) -> bool {
        if !self.phase.is_active() {
            return false;
        }
        self.mark_supervision_lost();
        true
    }

    /// Overall fraction across both phases (write counts for the first 85%,
    /// verify the last 15% — the read-back is quicker than the write).
    pub fn overall(&self) -> f64 {
        match self.phase {
            JobPhase::Idle => 0.0,
            JobPhase::Writing | JobPhase::Failed => self.write_frac * 0.85,
            JobPhase::Verifying => 0.85 + self.verify_frac * 0.15,
            JobPhase::Success => 1.0,
        }
    }

    /// Rough ETA in seconds for the active phase given `total` image bytes and
    /// the reported rate. `None` when idle, finished, or the rate is unknown.
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

/// Format a duration in seconds as a short `m:ss` / `Ns` string for the ETA.
pub fn fmt_eta(secs: u64) -> String {
    if secs >= 60 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

// ---------------------------------------------------------------------------
// Sample data — fully populated, used by `--shots` and as the instant-on
// seed so the screenshot is deterministic offline.
// ---------------------------------------------------------------------------

/// Two target drives: a USB stick (the flash target) and an internal `NVMe`
/// (the install target), exactly the spec the screenshot calls for.
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

/// Known Tilo releases offered without a file browse.
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

/// The source pre-selected for `--shots`: a Tilo 1.0 stable image.
pub fn sample_source() -> Source {
    // `sample_releases()` always returns at least one entry; the fallback
    // below only matters if that ever changes.
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

/// Recent-source history shown in the source picker.
pub fn sample_recent_sources() -> Vec<Source> {
    vec![
        sample_source(),
        Source {
            kind: SourceKind::IsoFile,
            path: "/home/quinten/Downloads/Fedora-Workstation-Live-42.iso".into(),
            name: "Fedora-Workstation-Live-42.iso".into(),
            size: 2_113_929_216,
            label: "Fedora-WS-Live-42-1-1".into(),
            installable: false,
            identity: None,
            expected_sha256: None,
        },
        Source {
            kind: SourceKind::IsoFile,
            path: "/home/quinten/Downloads/archlinux-2026.06.01-x86_64.iso".into(),
            name: "archlinux-2026.06.01-x86_64.iso".into(),
            size: 1_181_116_006,
            label: "ARCH_202606".into(),
            installable: false,
            identity: None,
            expected_sha256: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Formatting helpers.
// ---------------------------------------------------------------------------

/// Format a byte count with SI decimal units, the way drive and image
/// vendors label capacities (`931.5 GB`). Zero means "unknown" and reads `—`.
pub fn fmt_size(bytes: u64) -> String {
    if bytes == 0 {
        "—".to_string()
    } else {
        ui::format::size(bytes)
    }
}

/// Format a transfer rate as `X MB/s`.
pub fn fmt_rate(bps: u64) -> String {
    if bps == 0 {
        return "—".into();
    }
    format!("{}/s", fmt_size(bps))
}
