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
#[effect(operation = "disks.device", device_only)]
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
        #[param("overview | smart | partitions")]
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
    /// Format.
    #[effect(job, destructive)]
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

/// The published row of the drive named `leaf`.
pub(crate) fn drive_row_id(leaf: &str) -> String {
    format!("disk:{leaf}")
}

/// The published row of the partition named `leaf`.
pub(crate) fn partition_row_id(leaf: &str) -> String {
    format!("partition:{leaf}")
}

/// `kind` on `device`, as a row offers it. Formatting asks the caller to
/// type the device's name, as erasing it deserves.
fn row_operation(kind: ActionKind, device: &str) -> ui::tilo_actions::ActionSpec {
    let mut spec = DisksEffect::operation(kind, device).bound();
    if kind.destructive() {
        spec.contract.confirmation = ConfirmationPolicy::Phrase {
            phrase: device.to_owned(),
        };
    }
    spec
}

/// The row of one partition: select it, and the operations that apply.
fn partition_row(part: &Partition) -> LiveRow {
    let mount = if part.mountpoint.is_some() {
        ActionKind::Unmount
    } else {
        ActionKind::Mount
    };
    LiveRow::new(
        partition_row_id(&part.name),
        "disks.partition",
        part.name.clone(),
    )
    .effect(&DisksEffect::Select {
        device: part.name.clone(),
    })
    .operation(row_operation(mount, &part.name))
    .operation(row_operation(ActionKind::Benchmark, &part.name))
    .operation(row_operation(ActionKind::Format, &part.name))
}

/// The row of one drive, with its partitions beneath it.
pub(crate) fn drive_row(drive: &Drive, names: &BTreeMap<String, String>) -> LiveRow {
    let label = drive
        .serial
        .as_ref()
        .and_then(|serial| names.get(serial))
        .cloned()
        .unwrap_or_else(|| drive.model.clone());
    let mut row = LiveRow::new(drive_row_id(&drive.name), "disks.drive", label)
        .effect(&DisksEffect::Select {
            device: drive.name.clone(),
        })
        .operation(row_operation(ActionKind::Format, &drive.name))
        .operation(row_operation(ActionKind::Benchmark, &drive.name));
    if drive.removable {
        row = row.operation(row_operation(ActionKind::Eject, &drive.name));
    }
    for part in &drive.partitions {
        row = row.row(partition_row(part));
    }
    row
}

/// The framework app.
pub struct DisksShell;

/// Screenshot scenarios: the sample machine, staged. A phone-sized capture
/// stages the same state as its wide twin.
const SCENARIOS: &[&str] = &[
    "health",
    "partitions",
    "partition",
    "usb",
    "failing",
    "named",
    "toast",
    "toast-error",
    "empty",
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
    const TITLE: &'static str = "Disks";
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
                _ => None,
            })
            .unwrap_or((1280, 800))
    }

    fn fixture(fixture: &mut Fixture<Disks>) {
        let topology = if fixture.scenario() == Some("empty") {
            Topology::default()
        } else {
            Topology::sample()
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
        crate::locales::install();
        cx.surface(DisksApp::new())
    }
}

impl ResourceSurface<Disks, DisksEffect> for DisksApp {
    fn present(&mut self, state: &Rc<Disks>) {
        self.adopt_names(&state.names());
    }

    fn attach(&mut self, cx: SurfaceCx<Disks>) {
        let fixture = cx.fixture::<Topology>().map(|topology| (*topology).clone());
        self.attach_framework(cx.dispatcher(), cx.mode(), fixture);
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
        app.harness().click_named("disks-action-benchmark").unwrap();
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

        // An agent names a drive in one call.
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
            assert_eq!(sample, scenario != Some("empty"), "{scenario:?}");
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
