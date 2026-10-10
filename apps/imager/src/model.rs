//! Pure data model for the Install Media (imager) app.
//!
//! Everything here is plain data + serde so the entire app state can be
//! round-tripped through Automerge scalars. Rendering, probing and the
//! write/install seam live in sibling modules — this file knows nothing
//! about vello, taffy or `lsblk`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Source — what we are going to write or install.
// ---------------------------------------------------------------------------

/// Where the bytes to be written come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// An arbitrary local disk image picked from disk.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// Absolute path to the image file (empty for a not-yet-downloaded
    /// known release).
    pub path: String,
    /// Display name (file name or release name).
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

/// Platform file identity, portable across the helper request boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub volume: u64,
    pub file: u64,
}

impl SourceIdentity {
    /// Capture identity from the held source file, never by reopening its path.
    /// Windows preserves the serialized volume serial / 64-bit file index pair;
    /// native query failures are errors, not an absent identity.
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
            // SAFETY: `file` keeps its handle alive for this call, and `info`
            // is a valid, writable output structure with the native layout.
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

    /// Whether this is external installation media the desktop imager may
    /// safely offer. Some USB readers report `RM=0`, so the transport is also
    /// considered; system disks are unconditionally excluded.
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

/// Whether a USB target is a throwaway live stick or a real, persistent
/// install you can boot and keep using.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallMode {
    /// Boot-once installer / live environment (writes the image as-is).
    Live,
    /// Persistent install written onto the stick — survives reboots,
    /// keeps user data.
    Persistent,
    /// Full install onto an internal disk.
    DiskInstall,
}

impl InstallMode {
    pub fn key(&self) -> &'static str {
        match self {
            InstallMode::Live => "live",
            InstallMode::Persistent => "persistent",
            InstallMode::DiskInstall => "disk",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            InstallMode::Live => "Live / installer stick",
            InstallMode::Persistent => "Persistent USB install",
            InstallMode::DiskInstall => "Install to disk",
        }
    }
    pub fn from_key(k: &str) -> InstallMode {
        match k {
            "persistent" => InstallMode::Persistent,
            "disk" => InstallMode::DiskInstall,
            _ => InstallMode::Live,
        }
    }
    pub fn all() -> [InstallMode; 3] {
        [
            InstallMode::Live,
            InstallMode::Persistent,
            InstallMode::DiskInstall,
        ]
    }
}

/// Filesystem used for the installed system.
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

/// The user account created during install.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserAccount {
    pub full_name: String,
    pub username: String,
    /// Held only for the current host-side UI session. It is deliberately
    /// excluded from the installation-media profile and must be entered on
    /// the target device.
    pub password: String,
    pub is_admin: bool,
    pub autologin: bool,
}

impl Default for UserAccount {
    fn default() -> Self {
        Self {
            full_name: "Tilo User".into(),
            username: "quser".into(),
            password: String::new(),
            is_admin: true,
            autologin: false,
        }
    }
}

/// The complete host-side customization draft. [`InstallProfile::to_media_profile_json`]
/// emits only its validated, non-secret preferences; target selection,
/// destructive consent, credentials, and recovery acknowledgement stay local
/// to the device being installed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallProfile {
    pub mode: InstallMode,
    pub hostname: String,
    pub user: UserAccount,
    pub locale: String,
    pub keyboard: String,
    pub timezone: String,
    pub filesystem: Filesystem,
    pub encrypt: bool,
    /// Host-session draft value only; never serialized into installation media.
    pub encryption_passphrase: String,
    /// Persistence overlay size in MB, for `InstallMode::Persistent`.
    pub persist_mb: u64,
    /// Wi-Fi network the new machine should join. The passphrase stays in the
    /// write request, never in this collaborative profile.
    #[serde(default)]
    pub wifi_ssid: String,
    /// Name of the device identity created for the target.
    #[serde(default)]
    pub device_label: String,
    /// Certify a new device under the setup machine's person and place it on
    /// the target. The human root secret stays on the setup machine.
    #[serde(default)]
    pub enroll_local_identity: bool,
}

/// 1–63 letters, digits or hyphens, without a hyphen at either end: one DNS
/// label, which is also what `<hostname>.local` needs.
pub(crate) fn hostname_is_valid(hostname: &str) -> bool {
    !hostname.is_empty()
        && hostname.len() <= 63
        && hostname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !hostname.starts_with('-')
        && !hostname.ends_with('-')
}

/// The hostname a profile starts with. A profile that still carries it has not
/// chosen one, so a written disk keeps the image's own name.
pub const DEFAULT_HOSTNAME: &str = "tilo";

impl Default for InstallProfile {
    fn default() -> Self {
        Self {
            mode: InstallMode::Live,
            hostname: DEFAULT_HOSTNAME.into(),
            user: UserAccount::default(),
            locale: "en_US.UTF-8".into(),
            keyboard: "us".into(),
            timezone: "UTC".into(),
            filesystem: Filesystem::Btrfs,
            encrypt: false,
            encryption_passphrase: String::new(),
            persist_mb: 4096,
            wifi_ssid: String::new(),
            device_label: String::new(),
            enroll_local_identity: false,
        }
    }
}

const INSTALLER_KEYBOARDS: &[&str] = &[
    "us", "gb", "de", "fr", "es", "it", "pt", "br", "ru", "se", "no", "dk", "ch", "pl", "dvorak",
    "colemak",
];

/// Exact locale catalog supported by both the host customizer and the
/// installed OS. Keep this in the image source so the host builds, live
/// installer and privileged backend all consume one reviewed contract.
pub(crate) const INSTALLER_LOCALE_CATALOG: &str =
    include_str!("../../../os/usr/share/tilo/installer-locales.txt");

pub(crate) fn installer_locales() -> impl Iterator<Item = &'static str> {
    INSTALLER_LOCALE_CATALOG
        .lines()
        .map(str::trim)
        .filter(|locale| !locale.is_empty())
}

impl InstallProfile {
    /// Validate the exact secret-free subset accepted by the live installer.
    ///
    /// This intentionally mirrors `installer::media_profile` locally: the
    /// imager cannot depend on the graphical installer crate without pulling a
    /// second application into every host build. Keeping the checks beside the
    /// media serializer still lets us reject a bad profile before spending time
    /// flashing it; the live installer repeats every check across the trust
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns a field-specific message when any transported value would be
    /// rejected by the live installer.
    pub fn validate_media_profile(&self) -> Result<(), String> {
        if !hostname_is_valid(self.hostname.trim()) {
            return Err(
                "Hostname must be 1–63 letters, numbers or hyphens, without a hyphen at either end."
                    .into(),
            );
        }

        let username = self.user.username.trim();
        let username_bytes = username.as_bytes();
        let reserved = matches!(
            username,
            "root"
                | "bin"
                | "daemon"
                | "adm"
                | "lp"
                | "sync"
                | "shutdown"
                | "halt"
                | "mail"
                | "operator"
                | "games"
                | "ftp"
                | "nobody"
                | "dbus"
                | "polkitd"
                | "sshd"
                | "chrony"
                | "tss"
                | "greetd"
                | "tilo"
        );
        if username_bytes.is_empty()
            || username_bytes.len() > 32
            || !username_bytes
                .first()
                .is_some_and(|c| matches!(c, b'a'..=b'z' | b'_'))
            || !username_bytes
                .iter()
                .skip(1)
                .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
            || reserved
        {
            return Err(
                "Username must be a non-reserved Linux account name of at most 32 characters."
                    .into(),
            );
        }

        let full_name = self.user.full_name.trim();
        if full_name.chars().count() > 100
            || full_name
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '\n' | '\r'))
        {
            return Err("Full name must be at most 100 characters and contain no control characters or colon."
                .into());
        }

        let locale = self.locale.trim();
        if !installer_locales().any(|supported| supported == locale) {
            return Err("Choose a language and locale supported by Tilo.".into());
        }

        if !INSTALLER_KEYBOARDS.contains(&self.keyboard.trim()) {
            return Err("Keyboard layout is not supported by the Tilo installer.".into());
        }

        let timezone = self.timezone.trim();
        if timezone.is_empty()
            || timezone.len() > 128
            || timezone.starts_with('/')
            || timezone.contains("..")
            || !timezone
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-'))
        {
            return Err("Time zone must be a safe IANA name such as Europe/Berlin.".into());
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
            hostname: &'a str,
            username: &'a str,
            full_name: &'a str,
            locale: &'a str,
            keyboard: &'a str,
            timezone: &'a str,
            filesystem: &'static str,
        }

        serde_json::to_vec(&MediaProfile {
            schema: 1,
            hostname: self.hostname.trim(),
            username: self.user.username.trim(),
            full_name: self.user.full_name.trim(),
            locale: self.locale.trim(),
            keyboard: self.keyboard.trim(),
            timezone: self.timezone.trim(),
            filesystem: self.filesystem.key(),
        })
    }

    /// A named saved-profile snapshot for the "saved install profiles" list.
    pub fn sample_workstation() -> Self {
        InstallProfile {
            mode: InstallMode::DiskInstall,
            hostname: "tilo-workstation".into(),
            user: UserAccount {
                full_name: "Quinten".into(),
                username: "quinten".into(),
                password: "•••••••••".into(),
                is_admin: true,
                autologin: false,
            },
            locale: "en_US.UTF-8".into(),
            keyboard: "us".into(),
            timezone: "Europe/Amsterdam".into(),
            filesystem: Filesystem::Btrfs,
            encrypt: true,
            encryption_passphrase: "•••••••••".into(),
            persist_mb: 0,
            wifi_ssid: String::new(),
            device_label: String::new(),
            enroll_local_identity: false,
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
        ui::format::bytes_si(bytes)
    }
}

/// Format a transfer rate as `X MB/s`.
pub fn fmt_rate(bps: u64) -> String {
    if bps == 0 {
        return "—".into();
    }
    format!("{}/s", fmt_size(bps))
}

#[cfg(test)]
mod tests {
    /// Source identity across handles, renames and equal bytes.
    #[test]
    fn source_identity_follows_the_file_across_handles_and_renames() {
        // Phase `source_identity_tracks_the_file_across_handles_and_renames`.
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

        // Phase `held_source_identity_survives_rename_and_distinguishes_same_bytes`.
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

    /// Sample data, release digests, capacity, sizes and target kinds.
    #[test]
    fn samples_releases_and_targets_are_classified_truthfully() {
        // Phase `sample_has_usb_and_nvme_targets`.
        {
            let t = sample_targets();
            assert_eq!(t.len(), 2);
            assert_eq!(t[0].kind, TargetKind::Usb);
            assert!(t[0].removable);
            assert_eq!(t[1].kind, TargetKind::Nvme);
            assert!(!t[1].removable);
        }

        // Phase `sample_source_is_installable_tilo`.
        {
            let s = sample_source();
            assert_eq!(s.kind, SourceKind::TiloImage);
            assert!(s.installable);
            assert!(s.size > 0);
        }

        // Phase `release_without_authenticated_image_digest_fails_closed`.
        {
            let mut release = sample_releases().remove(0);
            release.image_sha256.clear();
            let source = release.to_source();
            assert_eq!(source.kind, SourceKind::IsoFile);
            assert!(!source.installable);
            assert!(source.expected_sha256.is_none());
        }

        // Phase `fits_respects_capacity`.
        {
            let usb = &sample_targets()[0];
            assert!(usb.fits(2_000_000_000));
            assert!(!usb.fits(40_000_000_000));
            assert!(usb.fits(0)); // unknown size always "fits" for the check
        }

        // Phase `fmt_size_picks_sane_units`.
        {
            assert_eq!(fmt_size(0), "—");
            assert_eq!(fmt_size(512), "512 B");
            assert_eq!(fmt_size(1_000_000_000_000), "1.0 TB");
            assert!(fmt_size(2_415_919_104).ends_with("GB"));
        }

        // Phase `target_kind_classifies_from_lsblk`.
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

    /// Embedded profile contract, filesystems, migration, validation and JSON.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_profile_serializes_migrates_and_validates_the_installer_contract() {
        // Phase `embedded_profile_contains_only_applied_non_secret_fields`.
        {
            let prof = InstallProfile::sample_workstation();
            let src = sample_source();
            let tgt = sample_targets()[1].clone();
            let cfg = prof.to_headless_config(Some(&src), Some(&tgt));
            assert!(cfg.contains("\"schema\": 1"));
            assert!(cfg.contains("\"hostname\": \"tilo-workstation\""));
            assert!(cfg.contains("\"username\": \"quinten\""));
            assert!(!cfg.contains(&src.path));
            assert!(!cfg.contains(&tgt.dev_path()));
            assert!(!cfg.contains("password"));
            assert!(!cfg.contains("passphrase"));
            assert!(!cfg.contains(&prof.user.password) || prof.user.password.contains('•'));
        }

        // Phase `every_filesystem_serializes_to_the_installer_contract`.
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

        // Phase `media_profile_validation_rejects_each_consumer_invalid_field`.
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

        // Phase `round_trips_through_json`.
        {
            let prof = InstallProfile::sample_workstation();
            let json = serde_json::to_string(&prof).unwrap();
            let back: InstallProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(back.hostname, prof.hostname);
            assert_eq!(back.encrypt, prof.encrypt);
            assert_eq!(back.filesystem, prof.filesystem);
        }
    }

    /// Step flow, progress, ETA and supervision recovery.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_and_progress_state_advance_and_fail_closed_on_lost_supervision() {
        // Phase `step_flow_advances_and_retreats_and_saturates`.
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
            // Every station round-trips through its stable document key.
            for s in Step::all() {
                assert_eq!(Step::from_key(s.key()), s);
            }
        }

        // Phase `progress_overall_and_eta_track_phase`.
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
            // Half of a 1000-byte image left at 100 B/s → ~5s.
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

        // Phase `restored_active_phases_fail_closed_when_supervision_is_lost`.
        {
            for phase in [JobPhase::Writing, JobPhase::Verifying] {
                let mut progress = Progress {
                    phase,
                    write_frac: 0.7,
                    verify_frac: 0.2,
                    rate_bps: 42_000_000,
                    error: None,
                };

                assert!(progress.fail_if_supervision_lost());
                assert_eq!(progress.phase, JobPhase::Failed);
                assert_eq!(progress.rate_bps, 0);
                assert!(progress.error.as_deref().is_some_and(|message| {
                    message.contains("outcome is unknown") && message.contains("incomplete")
                }));
                assert_ne!(progress.phase, JobPhase::Success);
            }
        }

        // Phase `supervision_recovery_does_not_rewrite_terminal_or_idle_phases`.
        {
            for phase in [JobPhase::Idle, JobPhase::Success, JobPhase::Failed] {
                let mut progress = Progress {
                    phase,
                    ..Progress::default()
                };
                assert!(!progress.fail_if_supervision_lost());
                assert_eq!(progress.phase, phase);
            }
        }
    }
}
