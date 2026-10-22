//! Which operations a drive or partition offers, and why one it would offer
//! is unavailable.
//!
//! One answer for every door: the window bar, a row's context menu, the
//! published rows agents and scripts invoke, and the check an invocation
//! passes before anything runs. An operation that would destroy or detach
//! the running system (its root, its home folders, what starts it, or the
//! whole drive they live on) is unavailable, with the reason in plain words.
//! One that makes no sense for the target (ejecting a fixed internal drive,
//! unmounting what is not mounted) is not offered at all.

use crate::app::ActionKind;
use crate::model::{Drive, Partition};

/// What an operation can do for a target now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// It makes no sense here; it is not shown.
    Hidden,
    Available,
    /// It is shown, but cannot run, for this reason.
    Unavailable(&'static str),
}

impl Offer {
    pub(crate) fn is_available(self) -> bool {
        self == Offer::Available
    }

    pub(crate) fn is_shown(self) -> bool {
        self != Offer::Hidden
    }
}

/// Why an encrypted partition is not formatted here.
pub(crate) const KEEPS_ENCRYPTION: &str = "Formatting would remove its encryption";
/// Why a locked encrypted partition cannot be mounted.
pub(crate) const LOCKED: &str = "Locked; unlock it first";

/// The filesystem Mount and Unmount act on for `part` (or, for the whole
/// drive, its only partition, so a single-filesystem stick mounts without
/// drilling in).
pub(crate) fn mount_target<'a>(
    drive: &'a Drive,
    part: Option<&'a Partition>,
) -> Option<&'a Partition> {
    part.or(match drive.partitions.as_slice() {
        [only] => Some(only),
        _ => None,
    })
}

/// What `action` can do on `part` of `drive` (`None`: the whole drive).
pub(crate) fn offer(drive: &Drive, part: Option<&Partition>, action: ActionKind) -> Offer {
    match action {
        ActionKind::Mount => match mount_target(drive, part) {
            Some(fs) if fs.mountpoint.is_none() => {
                if fs
                    .encryption
                    .as_ref()
                    .is_some_and(|e| e.unlocked_as.is_none())
                {
                    Offer::Unavailable(LOCKED)
                } else {
                    Offer::Available
                }
            }
            _ => Offer::Hidden,
        },
        ActionKind::Unmount => match mount_target(drive, part) {
            Some(fs) if fs.mountpoint.is_some() => match drive.partition_use(fs) {
                Some(reason) => Offer::Unavailable(reason),
                None => Offer::Available,
            },
            _ => Offer::Hidden,
        },
        ActionKind::Format => {
            let reason = match part {
                Some(part) if part.encryption.is_some() => {
                    drive.partition_use(part).or(Some(KEEPS_ENCRYPTION))
                }
                Some(part) => drive.partition_use(part),
                None => drive.system_use(),
            };
            reason.map_or(Offer::Available, Offer::Unavailable)
        }
        ActionKind::Benchmark => Offer::Available,
        ActionKind::Eject if drive.removable => Offer::Available,
        ActionKind::Eject => Offer::Hidden,
    }
}

/// Every operation `part` of `drive` shows, in the order they are offered.
pub(crate) fn shown(drive: &Drive, part: Option<&Partition>) -> Vec<(ActionKind, Offer)> {
    ActionKind::bar()
        .into_iter()
        .map(|action| (action, offer(drive, part, action)))
        .filter(|(_, offer)| offer.is_shown())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HOLDS_SYSTEM;

    fn yoga() -> Drive {
        crate::probe::captured::yoga().unwrap().drives.remove(0)
    }

    fn part<'a>(drive: &'a Drive, name: &str) -> &'a Partition {
        drive.partitions.iter().find(|p| p.name == name).unwrap()
    }

    /// Nothing that would destroy or detach the running system can run, and
    /// each says why in plain words: the whole system drive, its root, its
    /// home folders, and what starts it. Windows' own data stays formattable,
    /// and a fixed internal drive offers no Eject at all.
    #[test]
    fn the_running_systems_drive_and_partitions_refuse_destructive_actions_with_the_reason() {
        let drive = yoga();
        assert_eq!(
            offer(&drive, None, ActionKind::Format),
            Offer::Unavailable(HOLDS_SYSTEM)
        );
        assert_eq!(offer(&drive, None, ActionKind::Eject), Offer::Hidden);
        assert!(
            shown(&drive, None)
                .iter()
                .all(|(action, _)| *action != ActionKind::Eject),
            "a fixed internal drive never shows Eject"
        );
        assert_eq!(offer(&drive, None, ActionKind::Benchmark), Offer::Available);

        let home = part(&drive, "nvme0n1p4");
        assert_eq!(
            offer(&drive, Some(home), ActionKind::Unmount),
            Offer::Unavailable("Holds the home folders")
        );
        assert_eq!(
            offer(&drive, Some(home), ActionKind::Format),
            Offer::Unavailable("Holds the home folders")
        );
        assert_eq!(offer(&drive, Some(home), ActionKind::Mount), Offer::Hidden);

        let root = part(&drive, "nvme0n1p7");
        assert_eq!(
            offer(&drive, Some(root), ActionKind::Unmount),
            Offer::Unavailable(HOLDS_SYSTEM)
        );
        assert_eq!(
            offer(&drive, Some(root), ActionKind::Format),
            Offer::Unavailable(HOLDS_SYSTEM)
        );

        for (name, what) in [
            ("nvme0n1p5", "Tilo's EFI"),
            ("nvme0n1p6", "Tilo's boot"),
            ("nvme0n1p1", "the firmware's EFI"),
        ] {
            assert_eq!(
                offer(&drive, Some(part(&drive, name)), ActionKind::Format),
                Offer::Unavailable("Starts this device"),
                "{what} partition"
            );
        }
        let efi = part(&drive, "nvme0n1p5");
        assert_eq!(
            offer(&drive, Some(efi), ActionKind::Mount),
            Offer::Available,
            "looking inside the EFI partition destroys nothing"
        );

        let windows = part(&drive, "nvme0n1p3");
        assert_eq!(
            offer(&drive, Some(windows), ActionKind::Format),
            Offer::Available
        );
    }

    /// A drive that holds no running system formats; a removable one ejects.
    #[test]
    fn other_drives_keep_their_actions() {
        let usb = crate::model::Topology::sample()
            .drives
            .into_iter()
            .find(|drive| drive.removable)
            .unwrap();
        assert_eq!(offer(&usb, None, ActionKind::Format), Offer::Available);
        assert_eq!(offer(&usb, None, ActionKind::Eject), Offer::Available);
        assert_eq!(
            offer(&usb, None, ActionKind::Unmount),
            Offer::Available,
            "its only filesystem unmounts from the whole drive"
        );
    }
}
