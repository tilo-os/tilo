//! When the person must be asked, what the prompt offers first, and what a
//! "Remember" may keep.
//!
//! - Every drive needs consent; a remembered choice is the only thing that
//!   stands in for it, and only for an internal drive (D2, D5).
//! - The prompt pre-selects "Open" only for an internal drive on the disk
//!   this computer runs from, such as the Windows partition next to it;
//!   everything else starts at "Open safely" (D4).
//! - A remembered internal drive must sit at the exact recorded location on
//!   an internal controller, with no hot-pluggable link, not removable,
//!   present since start-up, and still show every recorded identifier (D5).
//! - An external drive is never remembered for "Open": remembering it keeps
//!   "Open safely" pre-selected for the same identifiers, and it is still
//!   asked about on every insertion (D7).

use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::protocol::{Mode, Placement};
use crate::topology::Topology;

/// What the service knows about one attached NTFS volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveFacts {
    pub topology: Topology,
    pub identity: Identity,
    /// D5 criterion 4: the disk was attached when this computer started.
    pub present_at_boot: bool,
    /// On the same physical disk as the running system's root.
    pub on_install_disk: bool,
}

impl DriveFacts {
    /// D5 criteria 1 to 4.
    pub fn is_internal(&self) -> bool {
        self.topology.is_internal() && self.present_at_boot
    }

    pub fn placement(&self) -> Placement {
        if self.is_internal() {
            Placement::Internal
        } else {
            Placement::External
        }
    }
}

/// What a remembered choice is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Binding {
    /// An internal drive at this exact place in the hardware.
    Internal {
        device_path: String,
        identity: Identity,
    },
    /// Any drive showing these identifiers. Pre-selects only.
    External { identity: Identity },
}

/// A remembered choice for one drive, kept per person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub label: String,
    pub mode: Mode,
    pub binding: Binding,
}

impl Grant {
    pub fn placement(&self) -> Placement {
        match self.binding {
            Binding::Internal { .. } => Placement::Internal,
            Binding::External { .. } => Placement::External,
        }
    }

    /// Whether this grant speaks for the drive `facts` describes.
    pub fn applies_to(&self, facts: &DriveFacts) -> bool {
        match &self.binding {
            Binding::Internal {
                device_path,
                identity,
            } => {
                facts.is_internal()
                    && *device_path == facts.topology.device_path
                    && identity.is_complete()
                    && identity.matches(&facts.identity)
            }
            Binding::External { identity } => identity.matches(&facts.identity),
        }
    }
}

/// How the service treats a drive for one person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment<'a> {
    pub remembered: Option<&'a Grant>,
    pub needs_consent: bool,
    pub preselect: Mode,
    pub can_remember: bool,
    pub can_remember_open: bool,
}

/// Decide how `facts` is treated, given the person's remembered `grants`.
pub fn assess<'a>(facts: &DriveFacts, grants: &'a [Grant]) -> Assessment<'a> {
    let remembered = grants.iter().find(|grant| grant.applies_to(facts));
    let internal = facts.is_internal();
    let can_remember_open = internal && facts.identity.is_complete();
    let can_remember = can_remember_open
        || facts.identity.drive.is_some()
        || facts.identity.partition.is_some()
        || facts.identity.volume.is_some();
    match remembered {
        Some(grant) if matches!(grant.binding, Binding::Internal { .. }) => Assessment {
            remembered,
            needs_consent: false,
            preselect: grant.mode,
            can_remember,
            can_remember_open,
        },
        _ => Assessment {
            remembered,
            needs_consent: true,
            // A drive that looks like a remembered one but fails its checks
            // is treated as new, whatever disk it sits on (D5).
            preselect: if internal && facts.on_install_disk && !resembles_remembered(facts, grants)
            {
                Mode::Open
            } else {
                Mode::OpenSafely
            },
            can_remember,
            can_remember_open,
        },
    }
}

/// Whether `facts` shares a location or an identifier with a remembered
/// internal drive it does not match.
fn resembles_remembered(facts: &DriveFacts, grants: &[Grant]) -> bool {
    grants.iter().any(|grant| match &grant.binding {
        Binding::Internal {
            device_path,
            identity,
        } => {
            !grant.applies_to(facts)
                && (*device_path == facts.topology.device_path
                    || shares_identifier(identity, &facts.identity))
        }
        Binding::External { .. } => false,
    })
}

fn shares_identifier(recorded: &Identity, seen: &Identity) -> bool {
    (recorded.drive.is_some() && recorded.drive == seen.drive)
        || (recorded.partition.is_some() && recorded.partition == seen.partition)
        || (recorded.volume.is_some() && recorded.volume == seen.volume)
}

/// The grant a "Remember" keeps for `facts` after the person chose `mode`,
/// or `None` when nothing may be remembered for this drive.
pub fn remember(facts: &DriveFacts, mode: Mode, id: String, label: String) -> Option<Grant> {
    let assessment = assess(facts, &[]);
    if assessment.can_remember_open {
        Some(Grant {
            id,
            label,
            mode,
            binding: Binding::Internal {
                device_path: facts.topology.device_path.clone(),
                identity: facts.identity.clone(),
            },
        })
    } else if assessment.can_remember {
        Some(Grant {
            id,
            label,
            mode: Mode::OpenSafely,
            binding: Binding::External {
                identity: facts.identity.clone(),
            },
        })
    } else {
        None
    }
}

/// Whether a remembered grant may be switched to `mode`.
pub fn mode_allowed(grant: &Grant, mode: Mode) -> bool {
    mode == Mode::OpenSafely || matches!(grant.binding, Binding::Internal { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{Controller, Link};

    fn internal_facts() -> DriveFacts {
        DriveFacts {
            topology: Topology {
                device_path:
                    "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1/nvme0n1p3"
                        .into(),
                disk: "nvme0n1".into(),
                controller: Some(Controller::NvmeRootPort),
                hotplug_link: None,
                removable: false,
                virtual_device: false,
            },
            identity: Identity {
                drive: Some("wwn:eui.1".into()),
                partition: Some("guid-1".into()),
                volume: Some(0xABCD),
            },
            present_at_boot: true,
            on_install_disk: true,
        }
    }

    /// A USB stick that copied every identifier of the internal drive.
    fn forged_stick() -> DriveFacts {
        let mut facts = internal_facts();
        facts.topology = Topology {
            device_path: "devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb/sdb1".into(),
            disk: "sdb".into(),
            controller: None,
            hotplug_link: Some(Link::Usb),
            removable: false,
            virtual_device: false,
        };
        facts.present_at_boot = false;
        facts.on_install_disk = false;
        facts
    }

    fn remembered_open(facts: &DriveFacts) -> Grant {
        remember(facts, Mode::Open, "g1".into(), "Windows".into()).unwrap()
    }

    #[test]
    fn every_unremembered_drive_needs_consent() {
        let internal = assess(&internal_facts(), &[]);
        assert!(internal.needs_consent);
        assert_eq!(internal.preselect, Mode::Open);
        let stick = assess(&forged_stick(), &[]);
        assert!(stick.needs_consent);
        assert_eq!(stick.preselect, Mode::OpenSafely);
    }

    #[test]
    fn open_is_preselected_only_for_an_internal_drive_on_the_install_disk() {
        let mut second_disk = internal_facts();
        second_disk.on_install_disk = false;
        assert_eq!(assess(&second_disk, &[]).preselect, Mode::OpenSafely);
        let mut late = internal_facts();
        late.present_at_boot = false;
        assert_eq!(assess(&late, &[]).preselect, Mode::OpenSafely);
    }

    #[test]
    fn a_remembered_internal_drive_opens_without_asking() {
        let facts = internal_facts();
        let grants = [remembered_open(&facts)];
        let assessment = assess(&facts, &grants);
        assert!(!assessment.needs_consent);
        assert_eq!(assessment.preselect, Mode::Open);
        assert_eq!(
            assessment.remembered.map(|grant| grant.id.as_str()),
            Some("g1")
        );
    }

    #[test]
    fn a_forged_stick_fails_location_whatever_identifiers_it_copies() {
        let grants = [remembered_open(&internal_facts())];
        let assessment = assess(&forged_stick(), &grants);
        assert!(assessment.needs_consent);
        assert_eq!(assessment.preselect, Mode::OpenSafely);
        assert!(assessment.remembered.is_none());
    }

    #[test]
    fn each_location_criterion_alone_voids_a_remembered_grant() {
        type Mutation = (&'static str, fn(&mut DriveFacts));
        let grants = [remembered_open(&internal_facts())];
        let mutations: [Mutation; 6] = [
            ("moved to another port", |facts| {
                facts.topology.device_path =
                    "devices/pci0000:00/0000:00:1c.4/0000:3e:00.0/nvme/nvme1/nvme1n1/nvme1n1p3"
                        .into();
            }),
            ("not on an internal controller", |facts| {
                facts.topology.controller = Some(Controller::NvmeBehindBridge);
            }),
            ("behind a Thunderbolt link", |facts| {
                facts.topology.hotplug_link = Some(Link::ExternalPci);
            }),
            ("removable", |facts| facts.topology.removable = true),
            ("hot-added during the session", |facts| {
                facts.present_at_boot = false;
            }),
            ("virtual", |facts| facts.topology.virtual_device = true),
        ];
        for (why, mutate) in mutations {
            let mut facts = internal_facts();
            mutate(&mut facts);
            let assessment = assess(&facts, &grants);
            assert!(assessment.needs_consent, "{why}: must ask again");
            assert_eq!(
                assessment.preselect,
                Mode::OpenSafely,
                "{why}: treated as new"
            );
        }
    }

    #[test]
    fn a_swapped_drive_at_the_same_place_is_new() {
        let grants = [remembered_open(&internal_facts())];
        let swaps: [fn(&mut Identity); 4] = [
            |identity| identity.drive = Some("wwn:other".into()),
            |identity| identity.partition = Some("guid-2".into()),
            |identity| identity.volume = Some(1),
            |identity| identity.volume = None,
        ];
        for swap in swaps {
            let mut facts = internal_facts();
            swap(&mut facts.identity);
            assert!(assess(&facts, &grants).needs_consent);
        }
    }

    #[test]
    fn external_drives_remember_only_open_safely_and_still_ask() {
        let stick = forged_stick();
        let grant = remember(&stick, Mode::Open, "g2".into(), "Stick".into()).unwrap();
        assert_eq!(
            grant.mode,
            Mode::OpenSafely,
            "Open on a stick lasts one insertion"
        );
        assert!(matches!(grant.binding, Binding::External { .. }));
        assert!(!mode_allowed(&grant, Mode::Open));
        let grants = [grant];
        let assessment = assess(&stick, &grants);
        assert!(
            assessment.needs_consent,
            "removable drives prompt on every insert"
        );
        assert_eq!(assessment.preselect, Mode::OpenSafely);
        assert!(assessment.remembered.is_some());
        let mut other = stick.clone();
        other.identity.volume = Some(99);
        assert!(
            assess(&other, &grants).remembered.is_none(),
            "only for the same IDs"
        );
    }

    #[test]
    fn an_internal_drive_without_every_identifier_cannot_keep_open() {
        let mut facts = internal_facts();
        facts.identity.partition = None;
        let assessment = assess(&facts, &[]);
        assert!(!assessment.can_remember_open);
        let grant = remember(&facts, Mode::Open, "g3".into(), "Data".into()).unwrap();
        assert_eq!(grant.mode, Mode::OpenSafely);
        let mut anonymous = forged_stick();
        anonymous.identity = Identity::default();
        assert!(remember(&anonymous, Mode::OpenSafely, "g4".into(), "?".into()).is_none());
    }
}
