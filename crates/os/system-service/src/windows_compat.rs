//! The `windows-compat` resource: what [`tilo_system_resources::windows_compat`] says it is.
//!
//! The layer is a signed system extension: `os_actions::windows_compat` reads how it stands and
//! runs the helper that installs or removes it. An install takes the person's agreement to the
//! disclosed size, which the reading carries, so the helper is given consent only after that size
//! has been shown.

use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::offer;
use tilo_system_resources::windows_compat::{
    WINDOWS_COMPAT, WindowsCompat, WindowsCompatAction, WindowsState,
};

use crate::i18n::name;

/// The `windows-compat` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.windows-compat", label = "Windows support")]
pub struct WindowsCompatReading;

/// The `windows-compat` resource reading `windows_compat`.
#[must_use]
pub fn resource(_windows: &WindowsCompat) -> ResourceSpec {
    offer::<WindowsCompatAction>(tilo_resource::typed_resource::<WindowsCompatReading>(
        WINDOWS_COMPAT,
        name("so.tilo.system.windows-compat", "Windows support"),
    ))
}

/// The layer as `os_actions` reads it now.
#[must_use]
pub fn reading() -> WindowsCompat {
    use os_actions::windows_compat::{Readiness, SizeDisclosure};
    let readiness = Readiness::read();
    let (state, size) = match &readiness {
        Readiness::Ready => (WindowsState::Ready, None),
        Readiness::NeedsInstall(disclosure) | Readiness::Failed(_, disclosure) => (
            if matches!(&readiness, Readiness::Failed(..)) {
                WindowsState::Failed
            } else {
                WindowsState::NeedsInstall
            },
            Some(SizeDisclosure::gigabytes(disclosure.download_bytes) + " GB"),
        ),
        Readiness::Installing(_) => (WindowsState::Installing, None),
        Readiness::WaitingForNetwork => (WindowsState::WaitingForNetwork, None),
        Readiness::NotOffered => (WindowsState::NotOffered, None),
    };
    WindowsCompat {
        available: os_actions::windows_compat::available(),
        state,
        summary: readiness.message(),
        size,
        problem: match &readiness {
            Readiness::Failed(error, _) => Some(error.clone()),
            _ => None,
        },
        ..WindowsCompat::default()
    }
}

/// Make `change` on the layer.
///
/// # Errors
/// Nothing can be installed, or the helper refused or failed.
pub fn change(change: &crate::device::WindowsChange) -> Result<(), String> {
    use crate::device::WindowsChange;
    use os_actions::windows_compat::{Consent, Readiness};
    match change {
        WindowsChange::Install => {
            let readiness = Readiness::read();
            match readiness {
                Readiness::NeedsInstall(disclosure) | Readiness::Failed(_, disclosure) => {
                    os_actions::windows_compat::install(&Consent::shown(&disclosure))
                }
                Readiness::Ready => Err("Windows support is already installed".to_owned()),
                Readiness::Installing(_) => Err("Windows support is being installed".to_owned()),
                Readiness::WaitingForNetwork => {
                    Err("Windows support installs once this device is online".to_owned())
                }
                Readiness::NotOffered => {
                    Err("this system cannot install Windows support".to_owned())
                }
            }
        }
        WindowsChange::Remove => os_actions::windows_compat::remove(),
    }
}
