//! The `windows-compat` resource: what [`tilo_system_resources::windows_compat`] says it is.
//!
//! The layer is a signed system extension: `os_actions::windows_compat` reads how it stands and
//! runs the helper that installs or removes it. An install takes the person's agreement to the
//! disclosed size, which the reading carries, so the helper is given consent only after that size
//! has been shown.

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::windows_compat::{
    WINDOWS_PROGRAMS, WindowsOffer, WindowsPrograms, WindowsProgramsAction, WindowsState,
    WindowsStep,
};

use crate::i18n::name;

/// The `windows-compat` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.windows-compat", label = "Windows support")]
pub struct WindowsProgramsReading;

/// The `windows-compat` resource reading `windows_compat`.
#[must_use]
pub fn resource(_windows: &WindowsPrograms) -> ResourceSpec {
    offer::<WindowsProgramsAction>(tilo_resource::typed_resource::<WindowsProgramsReading>(
        WINDOWS_PROGRAMS,
        name("so.tilo.system.windows-compat", "Windows support"),
    ))
}

/// The layer as `os_actions` reads it now.
#[must_use]
pub fn reading() -> WindowsPrograms {
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
    let step = readiness.progress_label().map(|(label, number, count)| {
        let (megabytes_done, megabytes_total) = readiness
            .progress_megabytes()
            .map_or((None, None), |(done, total)| (Some(done), Some(total)));
        WindowsStep {
            label,
            number,
            count,
            megabytes_done,
            megabytes_total,
        }
    });
    WindowsPrograms {
        available: os_actions::windows_compat::available(),
        state,
        summary: readiness.message(),
        size,
        problem: match &readiness {
            Readiness::Failed(error, _) => Some(error.clone()),
            _ => None,
        },
        progress: readiness.progress_fraction().map(f64::from),
        step,
        offered: readiness
            .offered_actions()
            .into_iter()
            .filter_map(|action| match action {
                os_actions::windows_compat::WindowsSupportAction::Install => {
                    Some(WindowsOffer::Install)
                }
                os_actions::windows_compat::WindowsSupportAction::Remove => {
                    Some(WindowsOffer::Remove)
                }
                // Checking for support is the codec pack's; the layer offers it never.
                os_actions::windows_compat::WindowsSupportAction::Refresh => None,
            })
            .collect(),
        ..WindowsPrograms::default()
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
