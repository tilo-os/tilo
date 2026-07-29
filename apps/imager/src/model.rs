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
    /// An arbitrary `.iso` file picked from disk.
    IsoFile,
    /// A qOS installer/live image (local file or a known release).
    QosImage,
}

impl SourceKind {
    pub fn tag(&self) -> &'static str {
        match self {
            SourceKind::IsoFile => "ISO",
            SourceKind::QosImage => "qOS",
        }
    }
    pub fn long(&self) -> &'static str {
        match self {
            SourceKind::IsoFile => "Disk image file",
            SourceKind::QosImage => "qOS system image",
        }
    }
}

/// A selected image source with the metadata we can show without reading
/// the whole file (name, on-disk size, volume label / image type).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// Absolute path to the image file (empty for a not-yet-downloaded
    /// known release).
    pub path: String,
    /// Display name (file name or release name).
    pub name: String,
    /// Size in bytes (0 when not yet known / not downloaded).
    pub size: u64,
    /// Volume label or image variant ("qOS 1.0 Live", "Fedora-Workstation"…).
    pub label: String,
    /// Whether this source can drive a full qOS install (vs. raw ISO write
    /// only).
    pub installable: bool,
}

impl Source {
    /// One-line summary used under the source header.
    pub fn summary(&self) -> String {
        let where_from = if self.path.is_empty() {
            "not downloaded".to_string()
        } else {
            self.path.clone()
        };
        format!("{}  ·  {}  ·  {}", self.label, fmt_size(self.size), where_from)
    }
}

/// A locally-known qOS release the user can pick without browsing for a
/// file. The `path` is filled in once it is fetched / located on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QosRelease {
    pub name: String,
    pub version: String,
    pub size: u64,
    pub channel: String,
    /// Local path once available; empty means "needs download".
    pub local_path: String,
}

impl QosRelease {
    pub fn to_source(&self) -> Source {
        Source {
            kind: SourceKind::QosImage,
            path: self.local_path.clone(),
            name: format!("{} {}", self.name, self.version),
            size: self.size,
            label: format!("qOS {} ({})", self.version, self.channel),
            installable: true,
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
    /// Removable media is the "safe" flash target; fixed internal disks are
    /// the dangerous install target and get a louder warning.
    pub fn is_removable_class(&self) -> bool {
        matches!(self, TargetKind::Usb | TargetKind::SdCard)
    }
    pub fn from_lsblk(rota: bool, tran: Option<&str>, removable: bool) -> Self {
        match tran {
            Some("usb") => TargetKind::Usb,
            Some("nvme") => TargetKind::Nvme,
            Some("sata") | Some("ata") => {
                if removable {
                    TargetKind::Usb
                } else if rota {
                    TargetKind::Hdd
                } else {
                    TargetKind::Ssd
                }
            }
            Some("mmc") => TargetKind::SdCard,
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
    pub size: u64,
    pub kind: TargetKind,
    pub tran: Option<String>,
    pub removable: bool,
    /// `true` for the root/system disk — writing here destroys the running
    /// OS, so it is hidden unless the user explicitly opts in.
    pub is_system: bool,
    pub partitions: Vec<TargetPartition>,
}

impl TargetDrive {
    pub fn dev_path(&self) -> String {
        format!("/dev/{}", self.name)
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
        format!("{} partition(s): {}", self.partitions.len(), labels.join(", "))
    }
    /// Does the image fit? (Raw writes need image_size ≤ drive size.)
    pub fn fits(&self, image_size: u64) -> bool {
        image_size == 0 || image_size <= self.size
    }
}

// ---------------------------------------------------------------------------
// Action — what to do with (source, target).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Raw `dd`-style copy of an ISO onto a removable drive, then verify.
    Flash,
    /// Full qOS install (live stick, persistent stick, or onto a disk),
    /// driven by an [`InstallProfile`].
    Install,
}

impl Action {
    pub fn key(&self) -> &'static str {
        match self {
            Action::Flash => "flash",
            Action::Install => "install",
        }
    }
    pub fn title(&self) -> &'static str {
        match self {
            Action::Flash => "Write ISO",
            Action::Install => "Install qOS",
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

/// One step of the guided flow. The app is a linear wizard: pick a source
/// image, pick a target drive, optionally pre-configure the image, then write
/// (which streams and then verifies). Verify is a phase *within* the Write
/// step, not a separate destination, so the breadcrumb stays short.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Source,
    Drive,
    Customize,
    Write,
}

impl Step {
    pub fn all() -> [Step; 4] {
        [Step::Source, Step::Drive, Step::Customize, Step::Write]
    }
    pub fn key(&self) -> &'static str {
        match self {
            Step::Source => "source",
            Step::Drive => "drive",
            Step::Customize => "customize",
            Step::Write => "write",
        }
    }
    pub fn from_key(k: &str) -> Step {
        match k {
            "drive" => Step::Drive,
            "customize" => Step::Customize,
            "write" => Step::Write,
            _ => Step::Source,
        }
    }
    /// Zero-based order used for the breadcrumb + "is this step done" logic.
    pub fn index(&self) -> usize {
        match self {
            Step::Source => 0,
            Step::Drive => 1,
            Step::Customize => 2,
            Step::Write => 3,
        }
    }
    /// Short breadcrumb title.
    pub fn title(&self) -> &'static str {
        match self {
            Step::Source => "Source",
            Step::Drive => "Drive",
            Step::Customize => "Customize",
            Step::Write => "Write",
        }
    }
    /// The next step in the flow (saturates at the last step).
    pub fn next(&self) -> Step {
        match self {
            Step::Source => Step::Drive,
            Step::Drive => Step::Customize,
            Step::Customize => Step::Write,
            Step::Write => Step::Write,
        }
    }
    /// The previous step (saturates at the first step).
    pub fn prev(&self) -> Step {
        match self {
            Step::Source => Step::Source,
            Step::Drive => Step::Source,
            Step::Customize => Step::Drive,
            Step::Write => Step::Customize,
        }
    }
}

// ---------------------------------------------------------------------------
// Install options — the structured config that ALSO serialises to a
// headless / unattended install file.
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
        [InstallMode::Live, InstallMode::Persistent, InstallMode::DiskInstall]
    }
}

/// Disk layout strategy for an install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskLayout {
    /// Single root partition filling the device.
    WholeDisk,
    /// Separate /home for data that survives reinstalls.
    SeparateHome,
    /// Btrfs subvolumes (qOS default for snapshots).
    BtrfsSubvols,
}

impl DiskLayout {
    pub fn key(&self) -> &'static str {
        match self {
            DiskLayout::WholeDisk => "whole",
            DiskLayout::SeparateHome => "home",
            DiskLayout::BtrfsSubvols => "btrfs",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            DiskLayout::WholeDisk => "Whole disk (single root)",
            DiskLayout::SeparateHome => "Separate /home",
            DiskLayout::BtrfsSubvols => "Btrfs subvolumes",
        }
    }
    pub fn from_key(k: &str) -> DiskLayout {
        match k {
            "home" => DiskLayout::SeparateHome,
            "btrfs" => DiskLayout::BtrfsSubvols,
            _ => DiskLayout::WholeDisk,
        }
    }
    pub fn all() -> [DiskLayout; 3] {
        [DiskLayout::WholeDisk, DiskLayout::SeparateHome, DiskLayout::BtrfsSubvols]
    }
}

/// The user account created during install.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserAccount {
    pub full_name: String,
    pub username: String,
    /// Held in the model only so the headless config can carry it; the UI
    /// shows it masked and the real writer would hash it.
    pub password: String,
    pub is_admin: bool,
    pub autologin: bool,
}

impl Default for UserAccount {
    fn default() -> Self {
        Self {
            full_name: "qOS User".into(),
            username: "quser".into(),
            password: "quickdesktop".into(),
            is_admin: true,
            autologin: false,
        }
    }
}

/// The complete, structured set of install options. Producing the same
/// struct from the UI or from a file means an interactive install and a
/// HEADLESS / unattended install share one source of truth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallProfile {
    pub mode: InstallMode,
    pub hostname: String,
    pub user: UserAccount,
    pub locale: String,
    pub keyboard: String,
    pub timezone: String,
    pub layout: DiskLayout,
    pub encrypt: bool,
    /// Passphrase for full-disk encryption (only meaningful when `encrypt`).
    pub encryption_passphrase: String,
    /// Persistence overlay size in MB, for `InstallMode::Persistent`.
    pub persist_mb: u64,
}

impl Default for InstallProfile {
    fn default() -> Self {
        Self {
            mode: InstallMode::Live,
            hostname: "qos".into(),
            user: UserAccount::default(),
            locale: "en_US.UTF-8".into(),
            keyboard: "us".into(),
            timezone: "UTC".into(),
            layout: DiskLayout::BtrfsSubvols,
            encrypt: false,
            encryption_passphrase: String::new(),
            persist_mb: 4096,
        }
    }
}

impl InstallProfile {
    /// A named saved-profile snapshot for the "saved install profiles" list.
    pub fn sample_workstation() -> Self {
        InstallProfile {
            mode: InstallMode::DiskInstall,
            hostname: "qos-workstation".into(),
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
            layout: DiskLayout::BtrfsSubvols,
            encrypt: true,
            encryption_passphrase: "•••••••••".into(),
            persist_mb: 0,
        }
    }

    /// Render the options as a HEADLESS / unattended install config. This
    /// is the artefact the "Export config" button shows — the same options
    /// the interactive flow uses, serialised so a machine can replay them
    /// with no operator present.
    ///
    /// The format is a small, self-describing TOML-ish document; the real
    /// installer ([`crate::write`]) consumes exactly these keys.
    pub fn to_headless_config(&self, source: Option<&Source>, target: Option<&TargetDrive>) -> String {
        let mut s = String::new();
        s.push_str("# qOS unattended install configuration\n");
        s.push_str("# Generated by the Install Media app — replay with:\n");
        s.push_str("#   qos-install --unattended ./qos-install.conf\n\n");

        s.push_str("[image]\n");
        if let Some(src) = source {
            s.push_str(&format!("source = \"{}\"\n", src.name));
            s.push_str(&format!("path = \"{}\"\n", src.path));
            s.push_str(&format!("size_bytes = {}\n", src.size));
        } else {
            s.push_str("source = \"\"\n");
        }
        s.push('\n');

        s.push_str("[target]\n");
        if let Some(t) = target {
            s.push_str(&format!("device = \"{}\"\n", t.dev_path()));
            s.push_str(&format!("model = \"{}\"\n", t.model));
            s.push_str(&format!("size_bytes = {}\n", t.size));
            s.push_str(&format!("removable = {}\n", t.removable));
        } else {
            s.push_str("device = \"\"\n");
        }
        s.push('\n');

        s.push_str("[install]\n");
        s.push_str(&format!("mode = \"{}\"\n", self.mode.key()));
        s.push_str(&format!("hostname = \"{}\"\n", self.hostname));
        s.push_str(&format!("locale = \"{}\"\n", self.locale));
        s.push_str(&format!("keyboard = \"{}\"\n", self.keyboard));
        s.push_str(&format!("timezone = \"{}\"\n", self.timezone));
        if matches!(self.mode, InstallMode::Persistent) {
            s.push_str(&format!("persist_mb = {}\n", self.persist_mb));
        }
        s.push('\n');

        s.push_str("[disk]\n");
        s.push_str(&format!("layout = \"{}\"\n", self.layout.key()));
        s.push_str(&format!("encrypt = {}\n", self.encrypt));
        if self.encrypt {
            // Never emit the real passphrase into an exported file — the
            // headless run prompts for it (or reads a secrets file).
            s.push_str("encryption_passphrase = \"$QOS_LUKS_PASSPHRASE\"\n");
        }
        s.push('\n');

        s.push_str("[user]\n");
        s.push_str(&format!("full_name = \"{}\"\n", self.user.full_name));
        s.push_str(&format!("username = \"{}\"\n", self.user.username));
        s.push_str("password = \"$QOS_USER_PASSWORD\"\n");
        s.push_str(&format!("admin = {}\n", self.user.is_admin));
        s.push_str(&format!("autologin = {}\n", self.user.autologin));

        s
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub phase: JobPhase,
    pub write_frac: f64,
    pub verify_frac: f64,
    /// Bytes per second of the active phase, for the ETA line.
    pub rate_bps: u64,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            phase: JobPhase::Idle,
            write_frac: 0.0,
            verify_frac: 0.0,
            rate_bps: 0,
        }
    }
}

impl Progress {
    /// Overall fraction across both phases (write counts for the first 85%,
    /// verify the last 15% — the read-back is quicker than the write).
    pub fn overall(&self) -> f64 {
        match self.phase {
            JobPhase::Idle => 0.0,
            JobPhase::Writing => self.write_frac * 0.85,
            JobPhase::Verifying => 0.85 + self.verify_frac * 0.15,
            JobPhase::Success => 1.0,
            JobPhase::Failed => self.write_frac * 0.85,
        }
    }

    /// Rough ETA in seconds for the active phase given `total` image bytes and
    /// the reported rate. `None` when idle, finished, or the rate is unknown.
    pub fn eta_secs(&self, total: u64) -> Option<u64> {
        if self.rate_bps == 0 || !self.phase.is_active() {
            return None;
        }
        let frac = if self.phase == JobPhase::Verifying { self.verify_frac } else { self.write_frac };
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

/// Two target drives: a USB stick (the flash target) and an internal NVMe
/// (the install target), exactly the spec the screenshot calls for.
pub fn sample_targets() -> Vec<TargetDrive> {
    vec![
        TargetDrive {
            name: "sdb".into(),
            model: "SanDisk Ultra USB 3.0".into(),
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

/// Known qOS releases offered without a file browse.
pub fn sample_releases() -> Vec<QosRelease> {
    vec![
        QosRelease {
            name: "qOS".into(),
            version: "1.0".into(),
            size: 2_415_919_104,
            channel: "stable".into(),
            local_path: "/var/lib/qos/images/qos-1.0-x86_64.iso".into(),
        },
        QosRelease {
            name: "qOS".into(),
            version: "1.1-rc2".into(),
            size: 2_499_805_184,
            channel: "testing".into(),
            local_path: String::new(),
        },
    ]
}

/// The source pre-selected for `--shots`: a qOS 1.0 stable image.
pub fn sample_source() -> Source {
    sample_releases()[0].to_source()
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
        },
        Source {
            kind: SourceKind::IsoFile,
            path: "/home/quinten/Downloads/archlinux-2026.06.01-x86_64.iso".into(),
            name: "archlinux-2026.06.01-x86_64.iso".into(),
            size: 1_181_116_006,
            label: "ARCH_202606".into(),
            installable: false,
        },
    ]
}

/// Saved install profiles shown in the options panel.
pub fn sample_profiles() -> Vec<(String, InstallProfile)> {
    vec![
        ("Default live stick".into(), InstallProfile::default()),
        ("Encrypted workstation".into(), InstallProfile::sample_workstation()),
    ]
}

// ---------------------------------------------------------------------------
// Formatting helpers.
// ---------------------------------------------------------------------------

/// Format a byte count as a short decimal-SI string (vendors use powers of
/// 1000), e.g. `931.5 GB`, `512 MB`, `16.0 GB`.
pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;
    if bytes == 0 {
        "—".to_string()
    } else if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} kB", b / KB)
    } else {
        format!("{bytes} B")
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
    use super::*;

    #[test]
    fn sample_has_usb_and_nvme_targets() {
        let t = sample_targets();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].kind, TargetKind::Usb);
        assert!(t[0].removable);
        assert_eq!(t[1].kind, TargetKind::Nvme);
        assert!(!t[1].removable);
    }

    #[test]
    fn sample_source_is_installable_qos() {
        let s = sample_source();
        assert_eq!(s.kind, SourceKind::QosImage);
        assert!(s.installable);
        assert!(s.size > 0);
    }

    #[test]
    fn headless_config_has_all_sections_and_masks_secrets() {
        let prof = InstallProfile::sample_workstation();
        let src = sample_source();
        let tgt = sample_targets()[1].clone();
        let cfg = prof.to_headless_config(Some(&src), Some(&tgt));
        for section in ["[image]", "[target]", "[install]", "[disk]", "[user]"] {
            assert!(cfg.contains(section), "missing {section}");
        }
        // The real passphrase / password must NOT leak into the exported file.
        assert!(cfg.contains("$QOS_LUKS_PASSPHRASE"));
        assert!(cfg.contains("$QOS_USER_PASSWORD"));
        assert!(!cfg.contains(&prof.user.password) || prof.user.password.contains('•'));
    }

    #[test]
    fn fits_respects_capacity() {
        let usb = &sample_targets()[0];
        assert!(usb.fits(2_000_000_000));
        assert!(!usb.fits(40_000_000_000));
        assert!(usb.fits(0)); // unknown size always "fits" for the check
    }

    #[test]
    fn fmt_size_picks_sane_units() {
        assert_eq!(fmt_size(0), "—");
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1_000_000_000_000), "1.00 TB");
        assert!(fmt_size(2_415_919_104).ends_with("GB"));
    }

    #[test]
    fn round_trips_through_json() {
        let prof = InstallProfile::sample_workstation();
        let json = serde_json::to_string(&prof).unwrap();
        let back: InstallProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hostname, prof.hostname);
        assert_eq!(back.encrypt, prof.encrypt);
        assert_eq!(back.layout, prof.layout);
    }

    #[test]
    fn step_flow_advances_and_retreats_and_saturates() {
        assert_eq!(Step::Source.next(), Step::Drive);
        assert_eq!(Step::Drive.next(), Step::Customize);
        assert_eq!(Step::Customize.next(), Step::Write);
        assert_eq!(Step::Write.next(), Step::Write, "last step saturates");
        assert_eq!(Step::Write.prev(), Step::Customize);
        assert_eq!(Step::Source.prev(), Step::Source, "first step saturates");
        assert_eq!(Step::all().len(), 4);
        assert_eq!(Step::from_key(Step::Customize.key()), Step::Customize);
    }

    #[test]
    fn progress_overall_and_eta_track_phase() {
        let idle = Progress::default();
        assert_eq!(idle.overall(), 0.0);
        assert_eq!(idle.eta_secs(1_000_000), None);
        let writing = Progress { phase: JobPhase::Writing, write_frac: 0.5, verify_frac: 0.0, rate_bps: 100 };
        assert!((writing.overall() - 0.425).abs() < 1e-9);
        // Half of a 1000-byte image left at 100 B/s → ~5s.
        assert_eq!(writing.eta_secs(1000), Some(5));
        let done = Progress { phase: JobPhase::Success, write_frac: 1.0, verify_frac: 1.0, rate_bps: 0 };
        assert_eq!(done.overall(), 1.0);
    }

    #[test]
    fn target_kind_classifies_from_lsblk() {
        assert_eq!(TargetKind::from_lsblk(false, Some("usb"), true), TargetKind::Usb);
        assert_eq!(TargetKind::from_lsblk(false, Some("nvme"), false), TargetKind::Nvme);
        assert_eq!(TargetKind::from_lsblk(true, Some("sata"), false), TargetKind::Hdd);
        assert_eq!(TargetKind::from_lsblk(false, Some("sata"), false), TargetKind::Ssd);
        assert_eq!(TargetKind::from_lsblk(false, Some("mmc"), true), TargetKind::SdCard);
    }
}
