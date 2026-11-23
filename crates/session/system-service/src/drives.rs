//! The `drives` resource: what [`tilo_system_resources::drives`] says it is, published with the
//! actions of [`DrivesAction`] and marked unavailable where this computer has no drive service.
//!
//! The machine side is the NTFS mount service's socket
//! (`crates/os/ntfs-mount/src/protocol.rs`): it lists the volumes attached now, the ones the
//! person chose to remember, and opens, closes, forgets and reconfigures them after checking
//! that the caller may ask. This module only translates between its protocol and the shared
//! resource, so no window speaks the socket on its own.

use std::path::Path;
use std::time::Duration;

use ntfs_mount::protocol::{
    self, Consent, Drive, Mode, Placement, ReadOnlyReason as ProtocolReadOnlyReason,
    RememberOffer as ProtocolRememberOffer, RememberedDrive, Request, Response,
};
use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::{offer, unavailable};
use tilo_system_resources::drives::{
    Drive as DriveReading, DriveMode, Drives, DrivesAction, Mounted, Placement as PlacementReading,
    ReadOnlyReason, RememberOffer, Remembered, RememberedDrive as RememberedDriveReading, DRIVES,
};

use crate::i18n::name;

/// How long one request may take. Opening a drive checks the volume first, so it is given the
/// same tens of seconds the shell gives it.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The `drives` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.drives", label = "Drives")]
pub struct DrivesReading;

/// The `drives` resource reading `drives`.
#[must_use]
pub fn resource(drives: &Drives) -> ResourceSpec {
    let mut resource = offer::<DrivesAction>(tilo_resource::typed_resource::<DrivesReading>(
        DRIVES,
        name("so.tilo.system.drives", "Drives"),
    ));
    if !drives.serving {
        for action in ["mount", "unmount", "forget", "set-mode"] {
            unavailable(
                &mut resource,
                action,
                "no_drive_service",
                "This computer has no drive service",
            );
        }
    }
    resource
}

/// The volumes and remembered choices the drive service answers with now.
#[must_use]
pub fn reading() -> Drives {
    let drives = call(Request::Drives);
    let remembered = call(Request::Remembered);
    let error = match (&drives, &remembered) {
        (Err(error), _) | (_, Err(error)) => Some(error.clone()),
        (Ok(_), Ok(_)) => None,
    };
    let serving = drives.is_ok() || remembered.is_ok();
    Drives {
        serving,
        drives: match drives {
            Ok(Response::Drives { drives }) => drives.into_iter().map(drive_reading).collect(),
            _ => Vec::new(),
        },
        remembered: match remembered {
            Ok(Response::Remembered { drives }) => {
                drives.into_iter().map(remembered_reading).collect()
            }
            _ => Vec::new(),
        },
        read_error: error,
        ..Drives::default()
    }
}

/// Make `change` on the drive service.
///
/// # Errors
/// The service refused it, it failed, or it did not answer.
pub fn change(change: &crate::device::DriveChange) -> Result<(), String> {
    use crate::device::DriveChange;
    let request = match change {
        DriveChange::Mount {
            drive,
            safely,
            remember,
        } => Request::Open {
            drive: drive.clone(),
            consent: Some(Consent {
                mode: mode(*safely),
                remember: *remember,
            }),
        },
        DriveChange::Unmount { drive } => Request::Close {
            drive: drive.clone(),
        },
        DriveChange::Forget { grant } => Request::Forget {
            grant: grant.clone(),
        },
        DriveChange::SetMode { grant, safely } => Request::SetMode {
            grant: grant.clone(),
            mode: mode(*safely),
        },
    };
    match call(request)? {
        Response::Done | Response::Opened { .. } => Ok(()),
        Response::Refused { reason } => Err(reason),
        Response::Failed { message } => Err(message),
        Response::NeedsConsent { .. } => {
            Err("The drive needs the person's agreement before it opens".to_owned())
        }
        Response::Drives { .. } | Response::Remembered { .. } => {
            Err("The drive service answered something else".to_owned())
        }
    }
}

fn call(request: Request) -> Result<Response, String> {
    protocol::call(Path::new(protocol::SOCKET_PATH), request, CALL_TIMEOUT)
        .map_err(|error| error.to_string())
}

fn mode(safely: bool) -> Mode {
    if safely {
        Mode::OpenSafely
    } else {
        Mode::Open
    }
}

fn drive_mode(mode: Mode) -> DriveMode {
    match mode {
        Mode::Open => DriveMode::Open,
        Mode::OpenSafely => DriveMode::OpenSafely,
    }
}

fn placement(placement: Placement) -> PlacementReading {
    match placement {
        Placement::Internal => PlacementReading::Internal,
        Placement::External => PlacementReading::External,
    }
}

fn drive_reading(drive: Drive) -> DriveReading {
    DriveReading {
        id: drive.id,
        label: drive.label,
        size_bytes: drive.size_bytes,
        placement: placement(drive.placement),
        hot_added: drive.hot_added,
        remembered: drive.remembered.map(|remembered| Remembered {
            grant: remembered.grant,
            mode: drive_mode(remembered.mode),
        }),
        needs_consent: drive.needs_consent,
        preselect: drive_mode(drive.preselect),
        can_remember: match drive.can_remember {
            ProtocolRememberOffer::Nothing => RememberOffer::Nothing,
            ProtocolRememberOffer::OpenSafely => RememberOffer::OpenSafely,
            ProtocolRememberOffer::AnyMode => RememberOffer::AnyMode,
        },
        mounted: drive.mounted.map(|mounted| Mounted {
            path: mounted.path,
            mode: drive_mode(mounted.mode),
            read_only: mounted.read_only.map(|reason| match reason {
                ProtocolReadOnlyReason::WindowsHibernated => ReadOnlyReason::WindowsHibernated,
                ProtocolReadOnlyReason::WindowsNotShutDown => ReadOnlyReason::WindowsNotShutDown,
                ProtocolReadOnlyReason::NeedsCheck => ReadOnlyReason::NeedsCheck,
            }),
        }),
    }
}

fn remembered_reading(drive: RememberedDrive) -> RememberedDriveReading {
    RememberedDriveReading {
        grant: drive.grant,
        label: drive.label,
        placement: placement(drive.placement),
        mode: drive_mode(drive.mode),
        mounted: drive.mounted,
    }
}
