//! Windows compatibility on demand, as the desktop sees it.
//!
//! The Windows payload (the FEX emulator and x86 guest root on ARM, Proton, the
//! Steam runtime and the virtio-win media, about 3 GB) is not part of the OS
//! image. It is the Tilo-signed system extension [`ID`], which overlays the
//! same `/usr` paths the compatibility service and the Windows VM already name,
//! and is installed by the generic extension helper ([`crate::extension`]).
//!
//! This module is the one place that decides what "Windows support" means to
//! the rest of the desktop:
//!
//! * [`payload_present`] says whether the files are there (merged), which is
//!   the only thing a launch cares about;
//! * [`SizeDisclosure`] is the figure a person is told before the first
//!   download. An install is never started without one being shown
//!   ([`Consent`] cannot be built without it);
//! * [`Readiness`] folds the files, the helper's status and the image's
//!   catalogue into the single state a UI, `tilo-control` or an error message
//!   reports: ready, needs installing, installing, waiting for the network,
//!   failed, or not offered on this system.

use std::path::Path;

use crate::extension::{
    self, ExtensionAction, ExtensionId, ExtensionState, ExtensionStatus, Progress,
};

/// The catalogue id of the Windows compatibility extension.
pub const ID: &str = "windows-compat";
/// The one sysext part of the extension.
pub const PART: &str = "tilo-windows-compat";
/// Files whose presence means the merged payload is there. Both are named by
/// the compatibility service and the Windows VM definition.
pub const PAYLOAD_MARKERS: [&str; 2] = [
    "/usr/lib/tilo/windows-runners/proton",
    "/usr/share/virtio-win/virtio-win.iso",
];

/// The Windows compatibility extension's id.
#[must_use]
pub fn id() -> ExtensionId {
    ExtensionId::known(ID)
}

/// Whether the payload is merged under `root` (`/` on a device).
#[must_use]
pub fn payload_present_under(root: &Path) -> bool {
    PAYLOAD_MARKERS
        .iter()
        .all(|marker| root.join(marker.trim_start_matches('/')).exists())
}

/// Whether the payload is merged on this system.
#[must_use]
pub fn payload_present() -> bool {
    payload_present_under(Path::new("/"))
}

/// What a person is told before the first download. Read from the image's
/// own catalogue entry, so it is the size that image's publisher declared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeDisclosure {
    pub download_bytes: u64,
    pub installed_bytes: u64,
}

impl SizeDisclosure {
    /// Parse a catalogue entry (`sysext.d/windows-compat.json`).
    #[must_use]
    pub fn parse(catalogue: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(catalogue).ok()?;
        let size = value.get("size")?;
        let bytes = |key: &str| size.get(key).and_then(serde_json::Value::as_u64);
        let disclosure = Self {
            download_bytes: bytes("download_bytes")?,
            installed_bytes: bytes("installed_bytes")?,
        };
        (disclosure.download_bytes > 0 && disclosure.installed_bytes > 0).then_some(disclosure)
    }

    /// The disclosure of this image.
    #[must_use]
    pub fn read() -> Option<Self> {
        Self::parse(&std::fs::read_to_string(id().catalogue_path()).ok()?)
    }

    /// Gigabytes with one decimal, rounded up so the figure is never an
    /// understatement.
    fn gigabytes(bytes: u64) -> String {
        let tenths = bytes.div_ceil(100_000_000);
        format!("{}.{}", tenths / 10, tenths % 10)
    }

    /// The sentence shown with the install button.
    #[must_use]
    pub fn sentence(&self) -> String {
        format!(
            "Windows support is a separate download of about {} GB that needs about {} GB of \
             disk space. It is fetched from Tilo, checked against Tilo's signature and added \
             beside the system. It is only installed because you asked for it.",
            Self::gigabytes(self.download_bytes),
            Self::gigabytes(self.installed_bytes),
        )
    }
}

/// Proof that the disclosure was put in front of the person before they chose
/// to install. [`install`] takes one, so no code path downloads three
/// gigabytes without having had the figure to show.
#[derive(Debug)]
pub struct Consent(());

impl Consent {}

/// Where Windows support stands on this system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Merged and usable.
    Ready,
    /// Not installed; installing needs the person's consent and this figure.
    NeedsInstall(SizeDisclosure),
    /// The helper is downloading or merging.
    Installing(Option<Progress>),
    /// The person opted in while offline; it installs once online.
    WaitingForNetwork,
    /// The last attempt failed; installing again is offered.
    Failed(String, SizeDisclosure),
    /// This system cannot install it (a host session, or an image whose
    /// catalogue does not carry the extension).
    NotOffered,
}

impl Readiness {
    /// Fold the facts. Pure, so every state is testable.
    #[must_use]
    pub fn from_facts(
        payload_present: bool,
        offered: Option<SizeDisclosure>,
        status: &ExtensionStatus,
    ) -> Self {
        if payload_present {
            return Self::Ready;
        }
        let Some(disclosure) = offered else {
            return Self::NotOffered;
        };
        match status.state {
            ExtensionState::Installing | ExtensionState::Removing => {
                Self::Installing(status.progress.clone())
            }
            ExtensionState::WaitingForNetwork => Self::WaitingForNetwork,
            ExtensionState::Failed if !status.last_error.is_empty() => {
                Self::Failed(status.last_error.clone(), disclosure)
            }
            ExtensionState::NotInstalled | ExtensionState::Installed | ExtensionState::Failed => {
                Self::NeedsInstall(disclosure)
            }
        }
    }

    /// The readiness on this system now.
    #[must_use]
    pub fn read() -> Self {
        let offered = extension::available(&id())
            .then(SizeDisclosure::read)
            .flatten();
        Self::from_facts(payload_present(), offered, &ExtensionStatus::read(&id()))
    }

    /// The one line a UI or an error shows. For the states that need the
    /// person to act it names the action.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Ready => "Windows support is installed.".into(),
            Self::NeedsInstall(disclosure) => format!(
                "Windows support is not installed. Install it to run Windows apps or create a \
                 Windows VM. {}",
                disclosure.sentence()
            ),
            Self::Installing(Some(progress)) if !progress.label.is_empty() => format!(
                "Installing Windows support: {} (step {} of {}).",
                progress.label, progress.step, progress.steps
            ),
            Self::Installing(_) => "Installing Windows support\u{2026}".into(),
            Self::WaitingForNetwork => {
                "Windows support installs by itself once this device is online.".into()
            }
            Self::Failed(error, _) => {
                format!("Windows support could not be installed: {error}. You can try again.")
            }
            Self::NotOffered => "Windows support cannot be installed on this system.".into(),
        }
    }

    /// Whether a start of [`install`] is the right next step.
    #[must_use]
    pub fn can_install(&self) -> bool {
        matches!(self, Self::NeedsInstall(_) | Self::Failed(..))
    }
}

/// Download, verify and merge the Windows support extension.
///
/// # Errors
/// As [`extension::run`].
pub fn install(_consent: &Consent) -> Result<(), String> {
    extension::run(ExtensionAction::Install, &id())
}

/// Remove the extension and everything it added; Windows capsules and VM
/// disks are not touched.
///
/// # Errors
/// As [`extension::run`].
pub fn remove() -> Result<(), String> {
    extension::run(ExtensionAction::Remove, &id())
}

/// Call `on_change` whenever the helper rewrites the extension's status.
///
/// # Errors
/// As [`extension::watch`].
pub fn watch(on_change: impl FnMut() -> bool + Send + 'static) -> Result<(), String> {
    extension::watch(&id(), on_change)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOGUE: &str = include_str!(
        "../../../../os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json"
    );

    fn disclosure() -> SizeDisclosure {
        SizeDisclosure {
            download_bytes: 1_900_000_000,
            installed_bytes: 3_100_000_000,
        }
    }

    #[test]
    fn the_shipped_catalogue_entry_carries_the_disclosure() {
        let shipped = SizeDisclosure::parse(CATALOGUE).expect("size disclosure in the catalogue");
        assert_eq!(shipped, disclosure());
        assert!(shipped.sentence().contains("about 1.9 GB"));
        assert!(shipped.sentence().contains("about 3.1 GB"));
    }

    #[test]
    fn a_catalogue_without_sizes_discloses_nothing() {
        assert_eq!(SizeDisclosure::parse(r#"{"id":"windows-compat"}"#), None);
        assert_eq!(
            SizeDisclosure::parse(r#"{"size":{"download_bytes":0,"installed_bytes":5}}"#),
            None
        );
        assert_eq!(SizeDisclosure::parse("not json"), None);
    }

    #[test]
    fn sizes_round_up_never_down() {
        assert_eq!(SizeDisclosure::gigabytes(1_900_000_001), "2.0");
        assert_eq!(SizeDisclosure::gigabytes(3_100_000_000), "3.1");
    }

    #[test]
    fn the_payload_is_present_only_when_every_marker_is() {
        let root = tempfile::tempdir().unwrap();
        assert!(!payload_present_under(root.path()));
        std::fs::create_dir_all(root.path().join("usr/lib/tilo/windows-runners/proton")).unwrap();
        assert!(
            !payload_present_under(root.path()),
            "virtio media is missing"
        );
        std::fs::create_dir_all(root.path().join("usr/share/virtio-win")).unwrap();
        std::fs::write(root.path().join("usr/share/virtio-win/virtio-win.iso"), b"").unwrap();
        assert!(payload_present_under(root.path()));
    }

    #[test]
    fn readiness_covers_every_state_and_names_the_action() {
        let offered = Some(disclosure());
        let status = |json: &str| ExtensionStatus::parse(json);
        assert_eq!(
            Readiness::from_facts(true, None, &ExtensionStatus::default()),
            Readiness::Ready,
            "the files decide, whatever the status says"
        );
        let needs = Readiness::from_facts(false, offered, &status(r#"{"state":"not-installed"}"#));
        assert_eq!(needs, Readiness::NeedsInstall(disclosure()));
        assert!(needs.can_install());
        assert!(needs.message().contains("not installed"));
        assert!(needs.message().contains("about 3.1 GB"));
        assert!(matches!(
            Readiness::from_facts(false, offered, &status(r#"{"state":"installing"}"#)),
            Readiness::Installing(None)
        ));
        assert_eq!(
            Readiness::from_facts(
                false,
                offered,
                &status(r#"{"state":"waiting-for-network"}"#)
            ),
            Readiness::WaitingForNetwork
        );
        let failed = Readiness::from_facts(
            false,
            offered,
            &status(r#"{"state":"failed","last_error":"offline"}"#),
        );
        assert!(failed.can_install(), "a failure offers installing again");
        assert!(failed.message().contains("offline"));
        assert_eq!(
            Readiness::from_facts(false, None, &status(r#"{"state":"not-installed"}"#)),
            Readiness::NotOffered
        );
        assert!(!Readiness::NotOffered.can_install());
    }

    #[test]
    fn it_uses_the_generic_extension_units_and_status() {
        assert_eq!(
            ExtensionAction::Install.unit(&id()),
            "tilo-extension@install:windows-compat.service"
        );
        assert_eq!(
            id().status_path(),
            Path::new("/var/lib/tilo/extensions/windows-compat.json")
        );
    }
}
