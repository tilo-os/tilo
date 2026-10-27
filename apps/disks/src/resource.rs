//! Disks as a resource.
//!
//! The block devices of this machine are live device state: the surface
//! probes them and publishes each drive and partition as a
//! [`LiveRow`] beneath the storage resource, offering the
//! [`DisksEffect`]s that apply to it. Mounting, formatting or ejecting
//! changes one machine, never the document; the effects run where they are
//! invoked, and only in a live window.
//!
//! The document holds what the person authors: the names they give their
//! drives, keyed by serial number, which follow them to every device.

use std::collections::BTreeMap;
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::tilo_actions::ConfirmationPolicy;
use ui::tilo_resource::{self, Atom, Effect, Invoke, Model, Refusal, resource};
use ui::widget::Element;

use crate::app::{ActionKind, DisksApp};
use crate::model::{Drive, Partition, Topology};
use crate::offer::{self, Offer};

/// What the person authors about their storage devices.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Disks::valid)]
pub struct Disks {
    /// Drive serial number to the name the person gave it. A name is written
    /// whole: two people naming one drive at once keep one of the names.
    pub names: BTreeMap<String, Atom>,
}

impl Disks {
    /// A name is shown on one line: never blank, never padded.
    fn valid(&self) -> Result<(), Refusal> {
        if self
            .names
            .values()
            .any(|name| name.0.trim().is_empty() || name.0.trim() != name.0)
        {
            return Err(Refusal::new("a drive's name is not blank and not padded"));
        }
        Ok(())
    }

    /// Every named drive's name, by serial number.
    pub fn names(&self) -> BTreeMap<String, String> {
        self.names
            .iter()
            .map(|(serial, name)| (serial.clone(), name.0.clone()))
            .collect()
    }
}

#[resource(kind = "disks.system", label = "Storage devices")]
impl Disks {
    /// Name a drive, or forget its name.
    ///
    /// The name follows the drive to every device it is plugged into.
    #[action]
    pub fn name_drive(
        &mut self,
        #[param("The drive's serial number")] serial: String,
        #[param("The name to show; none or blank forgets it")] name: Option<String>,
    ) {
        match name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(name) => {
                self.names.insert(serial, Atom(name.to_owned()));
            }
            None => {
                self.names.remove(&serial);
            }
        }
    }
}

/// What Disks does to this machine, and how it shows it. Device effects
/// name their target by its kernel name (`sda`, `nvme0n1p2`) and are
/// checked against the latest probe before anything runs.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "disks.device")]
pub enum DisksEffect {
    /// Rescan storage devices.
    #[effect(kind = "read")]
    Rescan,
    /// Select.
    #[effect(kind = "read")]
    Select {
        #[param("Kernel name of a drive or partition, e.g. sda or sda1")]
        device: String,
    },
    /// Show detail tab.
    #[effect(kind = "read")]
    ShowTab {
        #[param("The tab to show", choices = ["overview", "smart", "partitions"])]
        tab: String,
    },
    /// Mount.
    #[effect(job)]
    Mount {
        #[param("Kernel name of the partition")]
        device: String,
    },
    /// Unmount.
    #[effect(job)]
    Unmount {
        #[param("Kernel name of the partition")]
        device: String,
    },
    /// Erase a drive or partition for good: every file on it is lost, and it
    /// starts again empty.
    #[effect(job, destructive, label = "Format")]
    Format {
        #[param("Kernel name of the drive or partition to erase")]
        device: String,
    },
    /// Benchmark.
    #[effect(job)]
    Benchmark {
        #[param("Kernel name of the drive or partition")]
        device: String,
    },
    /// Eject.
    #[effect(job)]
    Eject {
        #[param("Kernel name of the removable drive")]
        device: String,
    },
    /// Open a mounted filesystem where it lives, in Files.
    ///
    /// A mount is only useful once its files are reachable: this is the way
    /// there, without copying the path out of the row.
    #[effect(job, label = "Open in Files")]
    OpenFolder {
        #[param("Kernel name of the mounted partition")]
        device: String,
    },
    /// Show how much room a mounted filesystem's files take up, in the Disk
    /// Usage analyzer.
    #[effect(job, label = "Show disk usage")]
    DiskUsage {
        #[param("Kernel name of the mounted partition")]
        device: String,
    },
}

impl DisksEffect {
    /// The device operation `kind` on `device`.
    pub(crate) fn operation(kind: ActionKind, device: &str) -> Self {
        let device = device.to_owned();
        match kind {
            ActionKind::Mount => Self::Mount { device },
            ActionKind::Unmount => Self::Unmount { device },
            ActionKind::Format => Self::Format { device },
            ActionKind::Benchmark => Self::Benchmark { device },
            ActionKind::Eject => Self::Eject { device },
        }
    }
}

/// The session link that opens the Disk Usage analyzer in the workspace the
/// window is in. Its job is a different one from partitioning — what the
/// person's own files take up, not what the hardware is — so the two apps
/// find each other by link and stay two apps.
pub(crate) const USAGE_LINK: &str = "tilo://open#/me/so.tilo.disk-usage";

/// The mount point of `device` on `topology`, when it is mounted.
pub(crate) fn mount_point(topology: &Topology, device: &str) -> Option<String> {
    topology
        .drives
        .iter()
        .flat_map(|drive| drive.partitions.iter())
        .find(|part| part.name == device)
        .and_then(|part| part.mountpoint.clone())
        .filter(|mount| !mount.trim().is_empty())
}

/// The published row of the drive named `leaf`.
pub(crate) fn drive_row_id(leaf: &str) -> String {
    format!("disk:{leaf}")
}

/// The published row of the partition named `leaf`.
pub(crate) fn partition_row_id(leaf: &str) -> String {
    format!("partition:{leaf}")
}

/// What formatting `part` of `drive` (`None`: the whole drive) erases, in
/// the words the confirmation asks with: the partitions and the files a
/// person would lose, never only the action's name.
fn format_loses(drive: &Drive, part: Option<&Partition>) -> String {
    let Some(part) = part else {
        return match drive.partitions.len() {
            0 => "Everything on it is erased for good.".to_owned(),
            1 => "Its partition and every file on it are erased for good.".to_owned(),
            n => format!("Its {n} partitions and every file on them are erased for good."),
        };
    };
    let on = part
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map_or_else(
            || "it".to_owned(),
            |label| format!("\u{201c}{label}\u{201d}"),
        );
    match part.used.filter(|used| *used > 0) {
        Some(used) => format!(
            "The {} of files on {on} are erased for good.",
            ui::format::size(used)
        ),
        None => format!("Every file on {on} is erased for good."),
    }
}

/// `kind` on `device`, as a row offers it: unavailable, with the reason,
/// when `offer` says so. Formatting asks the caller to type the device's
/// name, as erasing it deserves, and says what is lost.
fn row_operation(
    kind: ActionKind,
    device: &str,
    offer: Offer,
    lost: impl FnOnce() -> String,
) -> ui::tilo_actions::ActionSpec {
    let mut spec = DisksEffect::operation(kind, device).bound();
    if kind.destructive() {
        spec.type_mut().contract.confirmation = ConfirmationPolicy::Phrase {
            phrase: device.to_owned(),
        };
        spec.type_mut().description = Some(lost());
    }
    if let Offer::Unavailable(reason) = offer {
        spec.type_mut().contract.availability = ui::tilo_actions::Availability::Unavailable {
            code: "disks_target_in_use".into(),
            reason: reason.into(),
        };
    }
    spec
}

/// Each of `actions` that `part` of `drive` shows, as row operations.
fn operations(
    mut row: LiveRow,
    drive: &Drive,
    part: Option<&Partition>,
    device: &str,
    actions: &[ActionKind],
) -> LiveRow {
    for &action in actions {
        let offer = offer::offer(drive, part, action);
        if offer.is_shown() {
            row = row.operation(row_operation(action, device, offer, || {
                format_loses(drive, part)
            }));
        }
    }
    row
}

/// The row of one partition: select it, and the operations it shows, the
/// same ones its context menu offers.
fn partition_row(drive: &Drive, part: &Partition) -> LiveRow {
    let row = LiveRow::new(
        partition_row_id(&part.name),
        "disks.partition",
        part.name.clone(),
    )
    .effect(&DisksEffect::Select {
        device: part.name.clone(),
    });
    let row = operations(
        row,
        drive,
        Some(part),
        &part.name,
        &[
            ActionKind::Mount,
            ActionKind::Unmount,
            ActionKind::Benchmark,
            ActionKind::Format,
        ],
    );
    // A mounted filesystem's files are reachable, so both ways to go and
    // look at them are offered where the mount is.
    if part.mountpoint.is_none() {
        return row;
    }
    row.operation(row_view_operation(
        &DisksEffect::OpenFolder {
            device: part.name.clone(),
        },
        "Open in Files",
    ))
    .operation(row_view_operation(
        &DisksEffect::DiskUsage {
            device: part.name.clone(),
        },
        "How much space is left?",
    ))
}

/// `effect` as a row operation, named `label`: what a row can do that
/// changes nothing on the machine.
fn row_view_operation(effect: &DisksEffect, label: &str) -> ui::tilo_actions::ActionSpec {
    let mut spec = effect.bound();
    spec.type_mut().label.clone_from(&label.to_owned());
    spec
}

/// The row of one drive, with its partitions beneath it.
pub(crate) fn drive_row(drive: &Drive, names: &BTreeMap<String, String>) -> LiveRow {
    let label = drive
        .serial
        .as_ref()
        .and_then(|serial| names.get(serial))
        .cloned()
        .unwrap_or_else(|| drive.model.clone());
    let row = LiveRow::new(drive_row_id(&drive.name), "disks.drive", label).effect(
        &DisksEffect::Select {
            device: drive.name.clone(),
        },
    );
    let mut row = operations(
        row,
        drive,
        None,
        &drive.name,
        &[ActionKind::Format, ActionKind::Benchmark, ActionKind::Eject],
    );
    for part in &drive.partitions {
        row = row.row(partition_row(drive, part));
    }
    row
}

/// The framework app.
pub struct DisksShell;

/// Screenshot scenarios: the sample machine, staged, and (`laptop*`) a real
/// laptop's captured storage. A phone-sized capture stages the same state as
/// its wide twin.
const SCENARIOS: &[&str] = &[
    "laptop",
    "laptop-partitions",
    "laptop-health",
    "health",
    "partitions",
    "partition",
    "usb",
    "failing",
    "named",
    "toast",
    "toast-error",
    "empty",
    "loading",
    "narrow",
    "drawer",
    "phone",
    "phone-drawer",
    "phone-health",
    "phone-partitions",
];

impl ResourceApp for DisksShell {
    type Resource = Disks;
    const ID: &'static str = "so.tilo.disks";
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Disks];
    const TITLE: &'static str = "Disks";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("disks");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .and_then(|name| match name {
                "narrow" => Some((800, 600)),
                // Narrow enough that the drive list collapses into a drawer.
                "drawer" => Some((640, 600)),
                _ if name.starts_with("phone") => Some((520, 820)),
                // Tall enough for the list and the chosen partition's
                // details under it.
                "laptop-partitions" => Some((1280, 1280)),
                _ => None,
            })
            .unwrap_or((1280, 800))
    }

    fn fixture(fixture: &mut Fixture<Disks>) {
        let topology = match fixture.scenario() {
            Some("empty" | "loading") => Topology::default(),
            Some(name) if name.starts_with("laptop") => {
                crate::probe::captured::yoga().unwrap_or_default()
            }
            _ => Topology::sample(),
        };
        fixture.provide(topology);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Disks>> {
        if name != Some("named") {
            return Vec::new();
        }
        vec![Disks::NAME_DRIVE.with(("S6Z1NJ0T934217X".to_owned(), Some("Workstation".to_owned())))]
    }

    fn view(cx: &mut ViewCx<Disks>) -> Element {
        cx.surface(DisksApp::new())
    }
}

impl ResourceSurface<Disks, DisksEffect> for DisksApp {
    fn present(&mut self, state: &Rc<Disks>) {
        self.adopt_names(&state.names());
    }

    fn attach(&mut self, cx: SurfaceCx<Disks>) {
        let fixture = cx.fixture::<Topology>().map(|topology| (*topology).clone());
        self.attach_framework(cx.dispatcher(), cx.mode(), fixture, cx.waker());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: DisksEffect, cx: &mut EffectCx<Disks, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if !self.is_running() {
            return Vec::new();
        }
        let busy = |effect| {
            EffectStatus::unavailable(
                effect,
                "disks.busy",
                "Another disk operation is still running",
            )
        };
        vec![
            busy(DisksEffect::MOUNT),
            busy(DisksEffect::UNMOUNT),
            busy(DisksEffect::FORMAT),
            busy(DisksEffect::BENCHMARK),
            busy(DisksEffect::EJECT),
        ]
    }

    /// Every drive on this machine with its partitions; a handful at most,
    /// so the whole set is published.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.drive_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;
    use ui::tilo_resource::Resource;

    /// A live window over the sample machine: effects run, devices are
    /// never touched.
    struct SampleDisks;

    impl ResourceApp for SampleDisks {
        type Resource = Disks;
        const ID: &'static str = "so.tilo.disks";
        const TITLE: &'static str = "Disks";

        fn view(cx: &mut ViewCx<Disks>) -> Element {
            cx.surface(DisksApp::sample())
        }
    }

    fn find<'a>(
        resources: &'a [ui::tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a ui::tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    fn action_ids(resource: &ui::tilo_actions::Resource) -> Vec<&str> {
        resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    #[test]
    fn drives_and_partitions_are_live_rows_offering_only_effects() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        let resources = app.harness().resources();
        let root = find(&resources, Disks::KIND).expect("the storage resource");
        let root_ids = action_ids(root);
        for id in [
            "name-drive",
            "rescan",
            "select",
            "show-tab",
            "format",
            "mount",
        ] {
            assert!(root_ids.contains(&id), "{id} in {root_ids:?}");
        }

        let disk = find(&resources, "disk:nvme0n1").expect("the sample nvme drive");
        assert_eq!(disk.kind, "disks.drive");
        assert_eq!(action_ids(disk), ["select", "format", "benchmark"]);
        let format = disk
            .actions
            .iter()
            .find(|action| action.id == "format")
            .unwrap();
        assert_eq!(
            format.contract.confirmation,
            ConfirmationPolicy::Phrase {
                phrase: "nvme0n1".into()
            },
            "erasing a drive asks for its name"
        );
        assert_eq!(
            format.description.as_deref(),
            Some("Its 4 partitions and every file on them are erased for good."),
            "the question says what erasing the drive loses"
        );
        let media = find(&resources, "partition:sdb1").expect("the media partition");
        let format = media
            .actions
            .iter()
            .find(|action| action.id == "format")
            .unwrap();
        assert_eq!(
            format.description.as_deref(),
            Some("The 1.5 TB of files on \u{201c}Media\u{201d} are erased for good."),
            "the question names the partition and what it holds"
        );
        let partition = find(&resources, "partition:nvme0n1p2").expect("a partition row");
        assert!(
            action_ids(partition)
                .iter()
                .any(|id| matches!(*id, "mount" | "unmount"))
        );
        for row in [disk, partition] {
            assert!(
                row.actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "a device row never changes the document"
            );
        }
        let removable = app
            .harness()
            .resources()
            .iter()
            .flat_map(|root| root.children.clone())
            .filter(|row| row.actions.iter().any(|action| action.id == "eject"))
            .count();
        assert_eq!(removable, 1, "only the removable drive ejects");
    }

    #[test]
    fn a_mounted_filesystem_offers_both_ways_on_its_files() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        let resources = app.harness().resources();
        let mounted = find(&resources, "partition:sdb1").expect("the media partition");
        assert!(
            mounted
                .actions
                .iter()
                .any(|action| action.id == "open-folder"),
            "a mounted row offers the way to its files: {:?}",
            action_ids(mounted)
        );
        assert!(
            mounted
                .actions
                .iter()
                .any(|action| action.id == "disk-usage"),
            "and how much room they take: {:?}",
            action_ids(mounted)
        );
        let sample = crate::model::Topology::sample();
        let unmounted_row = sample
            .drives
            .iter()
            .flat_map(|drive| drive.partitions.iter())
            .find(|part| part.mountpoint.is_none());
        if let Some(unmounted) = unmounted_row {
            let row = find(&resources, &format!("partition:{}", unmounted.name))
                .expect("an unmounted partition row");
            assert!(
                row.actions
                    .iter()
                    .all(|action| !matches!(action.id.as_str(), "open-folder" | "disk-usage")),
                "a filesystem that is not mounted has nowhere to go: {:?}",
                action_ids(row)
            );
        }

        // Both are offered on the sample machine, which says what it would do.
        let said = |output: ui::tilo_actions::ActionOutput| {
            output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let opened = said(
            app.effect(&DisksEffect::OpenFolder {
                device: "sdb1".into(),
            })
            .unwrap(),
        );
        assert!(
            opened.contains("would open /mnt/media in Files"),
            "{opened:?}"
        );
        let usage = said(
            app.effect(&DisksEffect::DiskUsage {
                device: "sdb1".into(),
            })
            .unwrap(),
        );
        assert!(usage.contains("would open Disk Usage"), "{usage:?}");
    }

    #[test]
    fn a_filesystem_that_is_not_mounted_has_nowhere_to_go() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        // A whole disk has no mount point of its own.
        assert!(
            app.effect(&DisksEffect::OpenFolder {
                device: "nvme0n1".into()
            })
            .is_err(),
            "a device that is not a mounted filesystem is refused, not opened"
        );
        assert!(
            app.effect(&DisksEffect::DiskUsage {
                device: "nvme0n1".into()
            })
            .is_err()
        );
    }

    #[test]
    fn the_usage_link_names_the_analyzer_in_the_persons_own_workspace() {
        assert_eq!(
            USAGE_LINK, "tilo://open#/me/so.tilo.disk-usage",
            "an app and no record opens the app as launching it would"
        );
        assert!(
            ui::tilo_reference::link::Link::parse_native(USAGE_LINK).is_ok(),
            "the session's own grammar takes it"
        );
        let topology = crate::model::Topology::sample();
        assert_eq!(
            mount_point(&topology, "sdb1").as_deref(),
            Some("/mnt/media")
        );
        assert_eq!(mount_point(&topology, "sdb"), None, "a disk is not mounted");
    }

    /// The drive page's two ways on reach the machine's own effects: the
    /// buttons name the mounted filesystem the section has selected, and the
    /// sample machine says what it would do rather than doing it.
    #[test]
    fn the_drive_pages_ways_on_run_the_effects_they_name() {
        // The drive page acts for the filesystem holding the most, not for
        // whichever partition comes first.
        let topology = crate::model::Topology::sample();
        let mount = topology.drives[0]
            .partitions
            .iter()
            .filter(|part| part.mountpoint.is_some())
            .max_by_key(|part| part.used.unwrap_or(0))
            .and_then(|part| part.mountpoint.clone())
            .expect("the sample drive has a mounted filesystem");
        assert_ne!(mount, "/boot/efi", "a boot partition is not the drive");

        let mut app = TestApp::<SampleDisks>::new().unwrap();
        app.harness()
            .click_named("disks-open-in-files")
            .expect("the drive page offers the way to the files");
        app.settle();
        let opened = app.harness().inspection_json().unwrap();
        assert!(
            opened.contains(&format!("would open {mount} in Files")),
            "the button opened {mount}: {opened}"
        );
        app.harness()
            .click_named("disks-space-left")
            .expect("the drive page offers how much space is left");
        app.settle();
        let usage = app.harness().inspection_json().unwrap();
        assert!(
            usage.contains(&format!("would open Disk Usage for {mount}")),
            "the button asked the analyzer about {mount}: {usage}"
        );
    }

    #[test]
    fn selecting_a_row_and_running_its_operations_reach_the_surface() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        app.call_on("partition:sda1", "select", serde_json::json!({}))
            .unwrap();
        app.effect(&DisksEffect::ShowTab {
            tab: "partitions".into(),
        })
        .unwrap();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("sda1"), "the partition is selected");

        // A benchmark on the sample machine reports what it would run.
        let output = app
            .effect(&DisksEffect::Benchmark {
                device: "sda".into(),
            })
            .unwrap();
        assert!(
            output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .is_some_and(|status| status.contains("/dev/sda")),
        );

        // A target the probe does not know is refused before anything runs.
        assert!(
            app.effect(&DisksEffect::Mount {
                device: "nvme0n1".into()
            })
            .is_err(),
            "a whole disk is not mounted"
        );
        assert!(
            app.effect_confirmed(&DisksEffect::Format {
                device: "gone0".into()
            })
            .is_err()
        );
    }

    #[test]
    fn an_action_bar_button_runs_the_operation_its_row_offers() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
            id: ActionKind::Benchmark.key().to_owned(),
        });
        app.settle();
        app.harness().relayout();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("benchmark on /dev/nvme0n1"),
            "the click reached the effect on the selected drive"
        );
    }

    #[test]
    fn formatting_waits_for_confirmation() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        assert!(matches!(
            app.effect(&DisksEffect::Format {
                device: "sdb1".into()
            }),
            Err(ui::tilo_actions::DoorError::ConfirmationRequired)
        ));
        let output = app
            .effect_confirmed(&DisksEffect::Format {
                device: "sdb1".into(),
            })
            .unwrap();
        assert!(format!("{:?}", output.value).contains("Block.Format"));
    }

    #[test]
    fn drive_names_are_document_state_shown_on_the_rows() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        app.invoke(Disks::NAME_DRIVE.with((
            "S6Z1NJ0T934217X".to_owned(),
            Some(" Workstation ".to_owned()),
        )))
        .unwrap();
        assert_eq!(
            app.state()
                .names()
                .get("S6Z1NJ0T934217X")
                .map(String::as_str),
            Some("Workstation"),
            "a name is trimmed"
        );
        let resources = app.harness().resources();
        assert_eq!(
            find(&resources, "disk:nvme0n1").map(|row| row.label.as_str()),
            Some("Workstation")
        );
        assert!(app.undo(), "naming a drive is an undo step");
        assert!(app.state().names.is_empty());

        app.call(
            "name-drive",
            serde_json::json!({ "serial": "S6Z1NJ0T934217X", "name": "Fast" }),
        )
        .unwrap();
        assert_eq!(
            app.state()
                .names()
                .get("S6Z1NJ0T934217X")
                .map(String::as_str),
            Some("Fast")
        );
        app.call(
            "name-drive",
            serde_json::json!({ "serial": "S6Z1NJ0T934217X", "name": " " }),
        )
        .unwrap();
        assert!(app.state().names.is_empty(), "a blank name forgets it");
        let padded = Disks::paths()
            .names
            .key("S6Z1NJ0T934217X")
            .set(Atom(" Fast".into()));
        assert!(
            app.invoke(Invoke::edit(&padded).unwrap()).is_err(),
            "a field edit is held to the same rule"
        );
    }

    #[test]
    fn history_names_the_drive_named() {
        let mut app = TestApp::<SampleDisks>::new().unwrap();
        app.call(
            "name-drive",
            serde_json::json!({ "serial": "S6Z1NJ0T934217X", "name": "Workstation" }),
        )
        .unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .history()
            .into_iter()
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(sentences, ["Named a drive “Workstation”"]);
    }

    #[test]
    fn two_devices_naming_different_drives_keep_both_names() {
        let mut left = ui::prelude::Replica::<Disks>::new();
        let mut right = left.fork();
        left.apply(Disks::NAME_DRIVE.with(("A".to_owned(), Some("Work".to_owned()))))
            .unwrap();
        right
            .apply(Disks::NAME_DRIVE.with(("B".to_owned(), Some("Backups".to_owned()))))
            .unwrap();
        left.sync_with(&mut right).unwrap();
        assert_eq!(left.state().unwrap(), right.state().unwrap());
        assert_eq!(left.state().unwrap().names().len(), 2);
    }

    #[test]
    fn every_scenario_captures_the_sample_machine_without_touching_it() {
        for scenario in std::iter::once(None).chain(SCENARIOS.iter().copied().map(Some)) {
            let mut app = TestApp::<DisksShell>::capture(scenario).unwrap();
            let resources = app.harness().resources();
            let sample = find(&resources, "disk:nvme0n1").is_some();
            assert_eq!(
                sample,
                !matches!(scenario, Some("empty" | "loading")),
                "{scenario:?}"
            );
            let audit = app.harness().native_app_audit();
            assert!(audit.is_certified(), "{scenario:?}: {audit:#?}");
        }
        let mut named = TestApp::<DisksShell>::capture(Some("named")).unwrap();
        assert!(
            named
                .harness()
                .inspection_json()
                .unwrap()
                .contains("Workstation")
        );
        let mut failed = TestApp::<DisksShell>::capture(Some("toast-error")).unwrap();
        assert!(
            failed
                .harness()
                .inspection_json()
                .unwrap()
                .contains("failed")
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::DisksShell::scenarios() {
            let mut app = TestApp::<super::DisksShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
