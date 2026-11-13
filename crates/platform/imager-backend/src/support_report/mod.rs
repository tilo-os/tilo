//! Hardware compatibility for the computer that will run Tilo: the components the kernel found,
//! the driver bound and the evidence that makes a result applicable to the selected release.
//!
//! A report grants nothing: the privileged helper re-probes before any change, and imported bytes
//! are relabelled, never authenticated.

use serde::{Deserialize, Serialize};

mod assemble;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_json;

#[cfg(test)]
mod tests;

pub const REPORT_SCHEMA: &str = "tilo.hardware-report.v2";
pub const MAX_REPORT_BYTES: usize = 1 << 20;
/// Every component class plus a few variants of the same class.
pub const MAX_COMPONENTS: usize = 64;
const MAX_FIELD_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportSource {
    LocalScan,
    Imported,
    DeclaredModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationIdentity {
    pub source: ReportSource,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub board: Option<String>,
    pub sku: Option<String>,
    pub firmware: Option<String>,
}

impl DestinationIdentity {
    /// A variant, board or SKU distinguishes a destination; a family alone does not.
    #[must_use]
    pub fn is_specific(&self) -> bool {
        trimmed(self.manufacturer.as_deref()).is_some()
            && trimmed(self.model.as_deref()).is_some()
            && (trimmed(self.variant.as_deref()).is_some()
                || trimmed(self.board.as_deref()).is_some()
                || trimmed(self.sku.as_deref()).is_some())
    }

    #[must_use]
    pub fn label(&self) -> String {
        let manufacturer = trimmed(self.manufacturer.as_deref());
        let model = trimmed(self.model.as_deref());
        match (manufacturer, model) {
            (Some(manufacturer), Some(model)) => {
                let mut label = format!("{manufacturer} {model}");
                if let Some(variant) = trimmed(self.variant.as_deref()) {
                    label.push_str(" (");
                    label.push_str(variant);
                    label.push(')');
                }
                label
            }
            (Some(manufacturer), None) => manufacturer.to_string(),
            (None, Some(model)) => model.to_string(),
            (None, None) => "Unknown model".to_string(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("manufacturer", &self.manufacturer),
            ("model", &self.model),
            ("variant", &self.variant),
            ("board", &self.board),
            ("sku", &self.sku),
            ("firmware", &self.firmware),
        ] {
            if let Some(value) = value
                && (value.trim().is_empty()
                    || value.chars().count() > MAX_FIELD_CHARS
                    || value.chars().any(char::is_control))
            {
                return Err(format!("The hardware report has an invalid {name}."));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentKind {
    Graphics,
    Display,
    Wifi,
    Ethernet,
    Bluetooth,
    Audio,
    Microphone,
    Camera,
    Fingerprint,
    Power,
    Acpi,
    Boot,
    Storage,
    Input,
}

impl ComponentKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Graphics => "graphics",
            Self::Display => "display",
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Microphone => "microphone",
            Self::Camera => "camera",
            Self::Fingerprint => "fingerprint",
            Self::Power => "power",
            Self::Acpi => "acpi",
            Self::Boot => "boot",
            Self::Storage => "storage",
            Self::Input => "input",
        }
    }

    #[must_use]
    pub const fn critical(self) -> bool {
        matches!(
            self,
            Self::Graphics | Self::Display | Self::Boot | Self::Storage
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupportState {
    Supported,
    Limited,
    Unsupported,
    Unverified,
}

impl SupportState {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Limited => "limited",
            Self::Unsupported => "unsupported",
            Self::Unverified => "unverified",
        }
    }
}

/// What the facts can establish: only evidence checked against the selected release is
/// [`ReleaseVerified`], and only such a component may be [`Supported`](SupportState::Supported).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    ReleaseVerified,
    ObservationOnly,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceScope {
    Observation,
    SelectedRelease,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceBinding {
    pub device: String,
    pub driver: Option<String>,
}

impl DeviceBinding {
    fn validate(&self) -> bool {
        bounded(&self.device) && self.driver.as_deref().is_none_or(bounded)
    }
}

/// A fact the outcome rests on; the view turns each into a localized sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "kebab-case")]
pub enum EvidenceFact {
    Device {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        firmware: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vendor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        product: Option<String>,
        #[serde(default)]
        reported_problem: bool,
    },
    Display {
        name: String,
        connected: bool,
        active: bool,
    },
    Kernel {
        version: String,
    },
    Acpi {
        tables: usize,
    },
    Suspend {
        states: Vec<String>,
    },
    Architecture {
        name: String,
    },
    Virtualization {
        names: Vec<String>,
    },
    Board {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        devicetree: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub scope: EvidenceScope,
    #[serde(flatten)]
    pub fact: EvidenceFact,
}

/// The outcome of one component check; the view localizes it, so the report carries no
/// sentences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum SupportReason {
    DriverBound {
        bindings: Vec<DeviceBinding>,
    },
    HybridGraphics {
        primary: DeviceBinding,
        additional: Vec<DeviceBinding>,
    },
    DriverMissing {
        devices: Vec<String>,
    },
    VirtualAdapter {
        device: String,
    },
    OpenNvidia {
        device: String,
    },
    PlatformPipeline {
        device: String,
    },
    NotDetected,
    DisplayDisconnected,
    DisplayInactive {
        device: String,
    },
    DisplaysActive {
        internal: usize,
        external: usize,
        devices: Vec<String>,
    },
    NoPlayback {
        device: String,
    },
    NoCapture {
        device: String,
    },
    FingerprintUnverified {
        device: String,
    },
    SuspendUnavailable,
    Battery {
        device: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capacity: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        health: Option<u32>,
    },
    AcOnly,
    AcpiTables {
        tables: usize,
    },
    Uefi {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secure_boot: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tpm: Option<String>,
    },
    LegacyBoot,
    StartupUnknown,
    StartsHere {
        architecture: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        board: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        devicetree: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secure_boot: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tpm: Option<String>,
    },
    OtherArchitecture {
        architecture: String,
        running: String,
    },
    BoardWithoutDevicetree {
        board: String,
        architecture: String,
    },
    InputDevices {
        count: usize,
    },
    NoKeyboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Remedy {
    RunOnDestination,
    Connect,
    CheckDriver,
    PasswordSignIn,
    ExternalCamera,
    ExternalKeyboard,
    PowerSettings,
    DisplaySettings,
    UefiBoot,
    PowerAdapter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSupport {
    pub kind: ComponentKind,
    /// The observed hardware name, empty for `NotDetected`; the view names the class itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub present: bool,
    pub state: SupportState,
    pub applicability: Applicability,
    pub reason: SupportReason,
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<Remedy>,
}

impl ComponentSupport {
    /// Whether this needs reading without opening another view.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.state != SupportState::Supported
            && (self.present || self.state == SupportState::Unsupported)
    }

    /// A missing essential component blocks installation.
    #[must_use]
    pub fn blocks_install(&self) -> bool {
        self.kind.critical() && self.state == SupportState::Unsupported
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.chars().count() > MAX_FIELD_CHARS || self.name.chars().any(char::is_control) {
            return Err("A hardware report component has an invalid name.".into());
        }
        if self.state == SupportState::Supported
            && (self.applicability != Applicability::ReleaseVerified
                || !self
                    .evidence
                    .iter()
                    .any(|evidence| evidence.scope == EvidenceScope::SelectedRelease))
        {
            return Err(
                "A supported hardware component needs evidence checked against the selected release."
                    .into(),
            );
        }
        if self.evidence.len() > 16 {
            return Err("A hardware report component has invalid evidence.".into());
        }
        for evidence in &self.evidence {
            let valid = match &evidence.fact {
                EvidenceFact::Device {
                    name,
                    driver,
                    firmware,
                    vendor,
                    product,
                    ..
                } => {
                    bounded(name)
                        && driver.as_deref().is_none_or(bounded)
                        && firmware.as_deref().is_none_or(bounded)
                        && vendor.as_deref().is_none_or(bounded)
                        && product.as_deref().is_none_or(bounded)
                }
                EvidenceFact::Display { name, .. } | EvidenceFact::Architecture { name } => {
                    bounded(name)
                }
                EvidenceFact::Kernel { version } => bounded(version),
                EvidenceFact::Acpi { tables } => *tables <= 1_000_000,
                EvidenceFact::Suspend { states } => {
                    states.len() <= 16 && states.iter().all(|state| bounded(state))
                }
                EvidenceFact::Virtualization { names } => {
                    !names.is_empty() && names.len() <= 16 && names.iter().all(|name| bounded(name))
                }
                EvidenceFact::Board { id, devicetree } => {
                    bounded(id) && devicetree.as_deref().is_none_or(bounded)
                }
            };
            if !valid {
                return Err("A hardware report component has invalid evidence.".into());
            }
        }
        let reason_valid = match &self.reason {
            SupportReason::DriverBound { bindings } => {
                !bindings.is_empty() && bindings.iter().all(DeviceBinding::validate)
            }
            SupportReason::HybridGraphics {
                primary,
                additional,
            } => primary.validate() && additional.iter().all(DeviceBinding::validate),
            SupportReason::DriverMissing { devices }
            | SupportReason::DisplaysActive { devices, .. } => {
                !devices.is_empty() && devices.iter().all(|device| bounded(device))
            }
            SupportReason::VirtualAdapter { device }
            | SupportReason::OpenNvidia { device }
            | SupportReason::PlatformPipeline { device }
            | SupportReason::DisplayInactive { device }
            | SupportReason::NoPlayback { device }
            | SupportReason::NoCapture { device }
            | SupportReason::FingerprintUnverified { device } => bounded(device),
            SupportReason::Battery {
                device,
                status,
                health,
                capacity,
            } => {
                bounded(device)
                    && status.as_deref().is_none_or(bounded)
                    && health.is_none_or(|health| health <= 100)
                    && capacity.is_none_or(|capacity| capacity <= 100)
            }
            SupportReason::Uefi { tpm, .. } => tpm.as_deref().is_none_or(bounded),
            SupportReason::StartsHere {
                architecture,
                board,
                devicetree,
                tpm,
                ..
            } => {
                bounded(architecture)
                    && board.as_deref().is_none_or(bounded)
                    && devicetree.as_deref().is_none_or(bounded)
                    && tpm.as_deref().is_none_or(bounded)
            }
            SupportReason::OtherArchitecture {
                architecture,
                running,
            } => bounded(architecture) && bounded(running) && architecture != running,
            SupportReason::BoardWithoutDevicetree {
                board,
                architecture,
            } => bounded(board) && bounded(architecture),
            SupportReason::InputDevices { count } => *count <= 1024,
            SupportReason::NotDetected
            | SupportReason::DisplayDisconnected
            | SupportReason::SuspendUnavailable
            | SupportReason::AcOnly
            | SupportReason::AcpiTables { .. }
            | SupportReason::LegacyBoot
            | SupportReason::StartupUnknown
            | SupportReason::NoKeyboard => true,
        };
        if !reason_valid {
            return Err("A hardware report component has an invalid reason.".into());
        }
        Ok(())
    }
}

fn bounded(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_FIELD_CHARS
        && !value.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportSummary {
    pub supported: usize,
    pub limited: usize,
    pub unsupported: usize,
    pub unverified: usize,
    pub blocking: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportReport {
    pub schema: String,
    pub identity: DestinationIdentity,
    pub kernel: Option<String>,
    /// A report without it cannot tell whether a medium prepared elsewhere starts on that
    /// computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    pub components: Vec<ComponentSupport>,
}

impl SupportReport {
    /// Reads this computer without privilege or mutation.
    #[must_use]
    pub fn probe() -> Self {
        #[cfg(target_os = "linux")]
        {
            linux::probe()
        }
        #[cfg(windows)]
        {
            windows::probe()
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            RawReport::default().into_report()
        }
    }

    pub fn declared_model(manufacturer: &str, model: &str, variant: &str) -> Result<Self, String> {
        Self::declared_model_with(manufacturer, model, variant, None, None)
    }

    pub fn declared_model_with(
        manufacturer: &str,
        model: &str,
        variant: &str,
        board: Option<&str>,
        sku: Option<&str>,
    ) -> Result<Self, String> {
        // An omitted field is absent, not empty: `validate` refuses an empty string, and only a
        // typed variant, board or SKU makes the model specific.
        let identity = DestinationIdentity {
            source: ReportSource::DeclaredModel,
            manufacturer: trimmed(Some(manufacturer)).map(str::to_string),
            model: trimmed(Some(model)).map(str::to_string),
            variant: trimmed(Some(variant)).map(str::to_string),
            board: trimmed(board).map(str::to_string),
            sku: trimmed(sku).map(str::to_string),
            firmware: None,
        };
        if !identity.is_specific() {
            return Err(
                "A destination needs a manufacturer, a model and a distinguishing variant.".into(),
            );
        }
        let report = Self {
            schema: REPORT_SCHEMA.into(),
            identity,
            kernel: None,
            architecture: None,
            components: Vec::new(),
        };
        report.validate()?;
        Ok(report)
    }

    /// Imports a report this probe produced on another computer.
    /// The bytes are unsigned evidence the person supplied, not installation authority: they are
    /// relabelled as imported, validated for shape and bounds, and every mutation re-checks the
    /// destination itself.
    pub fn import_json(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err("The hardware report is too large.".into());
        }
        let mut report: Self = serde_json::from_slice(bytes)
            .map_err(|_| "The file is not a Tilo hardware report.".to_string())?;
        report.identity.source = ReportSource::Imported;
        report.validate()?;
        if report.components.is_empty() {
            return Err("The hardware report has no component results.".into());
        }
        Ok(report)
    }

    /// Serializes a report for carrying to the preparing computer.
    pub fn to_json(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec_pretty(self).map_err(|error| error.to_string())
    }

    /// Returns the first schema or field that cannot be trusted.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REPORT_SCHEMA {
            return Err("The hardware report has an unsupported schema.".into());
        }
        self.identity.validate()?;
        if self.identity.source != ReportSource::DeclaredModel && !self.identity.is_specific() {
            return Err("The hardware report does not identify a specific model.".into());
        }
        if let Some(kernel) = &self.kernel
            && (kernel.trim().is_empty()
                || kernel.chars().count() > MAX_FIELD_CHARS
                || kernel.chars().any(char::is_control))
        {
            return Err("The hardware report has an invalid kernel version.".into());
        }
        if let Some(architecture) = &self.architecture
            && (architecture.trim().is_empty()
                || architecture.chars().count() > MAX_FIELD_CHARS
                || architecture.chars().any(char::is_control))
        {
            return Err("The hardware report has an invalid architecture.".into());
        }
        if self.components.len() > MAX_COMPONENTS {
            return Err("The hardware report has too many components.".into());
        }
        for component in &self.components {
            component.validate()?;
        }
        Ok(())
    }

    /// A missing essential component prevents a usable installation.
    #[must_use]
    pub fn blocks_install(&self) -> bool {
        self.components.iter().any(ComponentSupport::blocks_install)
    }

    #[must_use]
    pub fn summary(&self) -> SupportSummary {
        let mut summary = SupportSummary::default();
        for component in &self.components {
            match component.state {
                SupportState::Supported => summary.supported += 1,
                SupportState::Limited => summary.limited += 1,
                SupportState::Unsupported => summary.unsupported += 1,
                SupportState::Unverified => summary.unverified += 1,
            }
            if component.blocks_install() {
                summary.blocking += 1;
            }
        }
        summary
    }

    /// Components whose limitation or absence belongs in the expanded view.
    pub fn attention(&self) -> impl Iterator<Item = &ComponentSupport> {
        self.components
            .iter()
            .filter(|component| component.needs_attention())
    }

    /// Components that work, shown compactly.
    pub fn working(&self) -> impl Iterator<Item = &ComponentSupport> {
        self.components
            .iter()
            .filter(|component| component.state == SupportState::Supported)
    }
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Clone, Debug, Default)]
struct RawIdentity {
    manufacturer: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    board: Option<String>,
    sku: Option<String>,
    firmware: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RawReport {
    identity: RawIdentity,
    kernel: Option<String>,
    architecture: Option<String>,
    graphics: Vec<RawGraphics>,
    displays: Vec<RawDisplay>,
    wifi: Vec<RawNetwork>,
    ethernet: Vec<RawNetwork>,
    bluetooth: Vec<RawDevice>,
    sound_cards: Vec<RawSoundCard>,
    cameras: Vec<RawDevice>,
    fingerprint: Vec<RawDevice>,
    battery: Option<RawBattery>,
    ac_online: Option<bool>,
    suspend_states: Vec<String>,
    acpi_tables: Option<usize>,
    uefi: Option<bool>,
    secure_boot: Option<bool>,
    tpm: Option<String>,
    disks: Vec<RawDisk>,
    inputs: Vec<String>,
    /// Hardware-accelerated virtualization the kernel offers.
    virtualization: Vec<String>,
    /// The board profile the running image carries.
    board: Option<RawBoard>,
}

/// The board the image was built for, from `/usr/lib/tilo/hardware-profile.json`.
#[derive(Clone, Debug)]
struct RawBoard {
    id: String,
    architecture: Option<String>,
    devicetree: Option<String>,
}

#[derive(Clone, Debug)]
struct RawGraphics {
    name: String,
    driver: Option<String>,
    vendor: Option<String>,
    device: Option<String>,
    primary: bool,
    virtual_device: bool,
}

#[derive(Clone, Debug)]
struct RawDisplay {
    name: String,
    internal: Option<bool>,
    connected: bool,
    enabled: bool,
}

#[derive(Clone, Debug)]
struct RawNetwork {
    name: String,
    driver: Option<String>,
    firmware: Option<String>,
}

#[derive(Clone, Debug)]
struct RawDevice {
    name: String,
    driver: Option<String>,
    vendor: Option<String>,
    product: Option<String>,
    healthy: Option<bool>,
}

#[derive(Clone, Debug)]
struct RawSoundCard {
    name: String,
    playback: usize,
    capture: usize,
    driver: Option<String>,
}

#[derive(Clone, Debug)]
struct RawBattery {
    name: String,
    capacity: Option<u32>,
    status: Option<String>,
    health: Option<u32>,
}

#[derive(Clone, Debug)]
struct RawDisk {
    name: String,
    model: Option<String>,
    size_bytes: Option<u64>,
    driver: Option<String>,
}

impl RawReport {
    fn into_report(self) -> SupportReport {
        assemble::assemble(self)
    }
}
