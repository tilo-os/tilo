//! Disks as a [`tilo_resource::Resource`]: format/mount/eject are Effects; the
//! shared document holds a topology projection plus view selection.

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Availability,
    ConfirmationPolicy, ExecutionMode, OperationKind, ParamType, PermissionRequirement,
    Resource as PublishedResource, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::app::{ActionKind, DisksApp};
use crate::doc::DisksDoc;
use crate::model::Topology;

/// Shared disks document mutations: selection, tab, confirmation, status, and
/// the probed topology snapshot. Mount and format stay effects; they change
/// one machine, not the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DisksAction {
    SetSelected {
        index: u64,
    },
    /// `-1` is the whole disk.
    SetSelectedPart {
        part: i64,
    },
    SetTab {
        tab: String,
    },
    SetPending {
        pending: String,
    },
    SetStatus {
        status: String,
    },
    WriteTopology {
        json: String,
    },
}

impl Action for DisksAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetSelected { index } => DisksDoc::set_selected(doc, *index as usize),
            Self::SetSelectedPart { part } => {
                let part = (*part >= 0).then_some(*part as usize);
                DisksDoc::set_selected_part(doc, part);
            }
            Self::SetTab { tab } => DisksDoc::set_tab(doc, tab),
            Self::SetPending { pending } => DisksDoc::set_pending(doc, pending),
            Self::SetStatus { status } => DisksDoc::set_status(doc, status),
            Self::WriteTopology { json } => DisksDoc::write_topology_json(doc, json),
        }
        Ok(())
    }
}

/// Selection / tab / topology projection from the shared document.
#[derive(Clone, Debug, Default)]
pub struct DisksState {
    pub topology: Topology,
    pub selected: usize,
    pub selected_part: Option<usize>,
    pub tab: String,
    pub pending: String,
    pub status: String,
    /// Whether this surface may run privileged block-device jobs. Not document
    /// state: the widget sets it before publish, and a bare read uses the
    /// process `DISKS_LIVE` flag.
    pub live: bool,
}

/// The storage-devices resource.
pub struct Disks;

impl Resource for Disks {
    const KIND: &'static str = "disks.system";
    const LABEL: &'static str = "Storage devices";
    const ACTION_TYPE: &'static str = "disks.action";

    type State = DisksState;
    type Action = DisksAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<DisksAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DisksState, ModelError> {
        let topology = DisksDoc::topology_json(doc)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        Ok(DisksState {
            topology,
            selected: DisksDoc::selected(doc),
            selected_part: DisksDoc::selected_part(doc),
            tab: DisksDoc::tab(doc),
            pending: DisksDoc::pending(doc),
            status: DisksDoc::status(doc),
            live: live_disk_access(),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "select-drive",
                "Select drive",
                vec![ActionParam::required(
                    "index",
                    ParamType::Integer,
                    "Drive index in the topology",
                )],
                |args, _| {
                    let index = args
                        .integer("index")
                        .ok_or_else(|| "missing drive index".to_owned())?;
                    if index < 0 {
                        return Err("drive index must be ≥ 0".into());
                    }
                    Ok(DisksAction::SetSelected {
                        index: index as u64,
                    })
                },
            )
            .describe("Focus a drive in the shared selection."),
            ActionDecl::new(
                "select-partition",
                "Select partition",
                vec![ActionParam::required(
                    "part",
                    ParamType::Integer,
                    "Partition index, or -1 for the whole disk",
                )],
                |args, _| {
                    let part = args
                        .integer("part")
                        .ok_or_else(|| "missing partition index".to_owned())?;
                    Ok(DisksAction::SetSelectedPart { part })
                },
            )
            .describe("Focus a partition (or the whole disk) in the shared selection."),
            ActionDecl::new(
                "set-tab",
                "Set detail tab",
                vec![
                    ActionParam::required("tab", ParamType::Text, "overview | smart | partitions")
                        .with_choices(["overview", "smart", "partitions"]),
                ],
                |args, _| {
                    let tab = args
                        .text("tab")
                        .ok_or_else(|| "missing tab".to_owned())?
                        .to_owned();
                    Ok(DisksAction::SetTab { tab })
                },
            )
            .describe("Switch the shared detail tab."),
        ]
    }

    fn children(state: &DisksState) -> Vec<PublishedResource> {
        topology_children(state, state.live)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Per-drive format/mount/eject Effects are bound to topology children.
        Vec::new()
    }
}

/// Whether this process may run privileged block-device jobs (`DISKS_LIVE`).
fn live_disk_access() -> bool {
    std::env::var("DISKS_LIVE").is_ok()
}

/// Drive and partition children for [`tilo_resource::publish`]: document
/// select actions plus machine-local Effects (mount/format/eject/benchmark).
pub(crate) fn topology_children(state: &DisksState, live: bool) -> Vec<PublishedResource> {
    state
        .topology
        .drives
        .iter()
        .enumerate()
        .map(|(index, drive)| {
            let mut disk = PublishedResource::new(
                format!("disk:{}", drive.name),
                "disks.drive",
                drive.model.clone(),
            );
            if let Ok(select) = ActionSpec::immediate(
                "select",
                "Select drive",
                Disks::ACTION_TYPE,
                &DisksAction::SetSelected {
                    index: index as u64,
                },
            ) {
                disk.actions.push(select.reversible());
            }
            // Whole-drive operations are meaningful independently of any
            // partition selection. Publishing them here lets search, history,
            // collaboration and agents address the same operation contract.
            for action in [ActionKind::Format, ActionKind::Benchmark] {
                if let Some(spec) = effect_operation_spec(action, &drive.name, None, live) {
                    disk.actions.push(spec);
                }
            }
            if drive.removable
                && let Some(spec) =
                    effect_operation_spec(ActionKind::Eject, &drive.name, None, live)
            {
                disk.actions.push(spec);
            }
            for (part_index, part) in drive.partitions.iter().enumerate() {
                let mut partition = PublishedResource::new(
                    format!("partition:{}", part.name),
                    "disks.partition",
                    part.name.clone(),
                );
                if let Ok(select) = ActionSpec::immediate(
                    "select",
                    "Select partition",
                    Disks::ACTION_TYPE,
                    &DisksAction::SetSelectedPart {
                        part: part_index as i64,
                    },
                ) {
                    partition.actions.push(select.reversible());
                }
                let mount_action = if part.mountpoint.is_some() {
                    ActionKind::Unmount
                } else {
                    ActionKind::Mount
                };
                for action in [mount_action, ActionKind::Benchmark, ActionKind::Format] {
                    if let Some(spec) =
                        effect_operation_spec(action, &part.name, part.fstype.as_deref(), live)
                    {
                        partition.actions.push(spec);
                    }
                }
                disk.children.push(partition);
            }
            disk
        })
        .collect()
}

/// Canonical Effect descriptor for a disk or partition job.
pub(crate) fn effect_operation_spec(
    action: ActionKind,
    leaf: &str,
    fstype: Option<&str>,
    live: bool,
) -> Option<ActionSpec> {
    let payload = vec![
        action.key().to_string(),
        leaf.to_string(),
        fstype.unwrap_or_default().to_string(),
    ];
    let spec = ActionSpec::operation(
        action.key(),
        action.label().trim_end_matches('…'),
        OperationKind::Effect,
        "disks.operation",
        &payload,
    )
    .ok()?;
    let destructive = action.destructive();
    Some(spec.with_contract(ActionContract {
        availability: if live {
            Availability::Available
        } else {
            Availability::Unavailable {
                code: "live_access_disabled".into(),
                reason: "Live disk access is disabled".into(),
            }
        },
        permission: PermissionRequirement::Admin,
        risk: if destructive {
            RiskLevel::Destructive
        } else {
            RiskLevel::Privileged
        },
        confirmation: if destructive {
            ConfirmationPolicy::Phrase {
                phrase: leaf.to_string(),
            }
        } else {
            ConfirmationPolicy::Confirm
        },
        retry: if action == ActionKind::Benchmark {
            RetryPolicy::Idempotent
        } else {
            RetryPolicy::AtMostOnce
        },
        execution: ExecutionMode::Background { cancellable: false },
        result: ResultSpec::Job,
        ..ActionContract::default()
    }))
}

/// Hosted-only extras that are not on the framework resource. Confirm dialogs
/// and in-flight mount jobs are widget state today, not published resources.
pub(crate) fn live_extras(_app: &DisksApp) -> Vec<PublishedResource> {
    Vec::new()
}

/// Framework app.
pub struct DisksShell;

impl ResourceApp for DisksShell {
    type Resource = Disks;
    const ID: &'static str = "so.tilo.disks";
    const TITLE: &'static str = "Disks";
    const ICON: Option<&'static str> = Some("disks");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Disks>) -> Element {
        cx.surface(DisksApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "health",
            "partitions",
            "partition",
            "usb",
            "failing",
            "format",
            "toast",
            "toast-error",
            "empty",
            "narrow",
            "drawer",
            "phone",
            "phone-drawer",
            "phone-health",
            "phone-partitions",
            "phone-format",
        ]
    }

    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        match name {
            "narrow" | "drawer" => Some((800, 600)),
            _ if name.starts_with("phone") => Some((520, 820)),
            _ => None,
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Disks>> {
        // A phone-sized capture stages the same state as its wide twin.
        let base = name.map(|name| name.strip_prefix("phone-").unwrap_or(name));
        let actions: Vec<DisksAction> = match base {
            Some("health") => vec![DisksAction::SetTab {
                tab: "smart".into(),
            }],
            Some("partitions") => vec![
                DisksAction::SetTab {
                    tab: "partitions".into(),
                },
                DisksAction::SetSelectedPart { part: 1 },
            ],
            Some("partition") => vec![DisksAction::SetSelectedPart { part: 1 }],
            Some("usb") => vec![DisksAction::SetSelected { index: 3 }],
            Some("failing") => vec![
                DisksAction::SetSelected { index: 2 },
                DisksAction::SetTab {
                    tab: "smart".into(),
                },
            ],
            Some("format") => vec![
                DisksAction::SetSelectedPart { part: 2 },
                DisksAction::SetPending {
                    pending: "format:0:2".into(),
                },
            ],
            Some("toast") => vec![DisksAction::SetStatus {
                status: "Mounted /dev/sdc1".into(),
            }],
            Some("toast-error") => vec![DisksAction::SetStatus {
                status: "Mount failed: the device is busy".into(),
            }],
            Some("empty") => vec![DisksAction::WriteTopology {
                json: serde_json::to_string(&Topology::default()).unwrap_or_default(),
            }],
            _ => Vec::new(),
        };
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Disks> for DisksApp {
    fn present(&mut self, state: &std::rc::Rc<DisksState>) {
        self.adopt_shared(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Disks>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}

pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Disks>>,
    hosted: bool,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Disks>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &DisksAction) {
        let context = ui::tilo_actions::ActionContext::for_action("so.tilo.disks", "Disks", action);
        match &self.framework {
            Some(dispatcher) => {
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Disks::ACTION_TYPE, action, context),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Topology;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_topology_door_and_hosted_keeps_no_duplicate_root() {
        let mut doc = AutoCommit::new();
        DisksDoc::seed(&mut doc, &Topology::sample());
        let state = Disks::read(&doc).expect("seeded disks state");
        let published = ui::tilo_resource::publish::<Disks>("disks", "Storage devices", &state);
        assert_eq!(published.kind, "disks.system");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "select-drive"),
            "document select-drive must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-tab"),
            "document set-tab must be on the framework resource"
        );
        assert!(
            !published.children.is_empty(),
            "topology children must be on the framework resource"
        );
        let disk = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "disk:nvme0n1")
            .expect("sample nvme drive");
        let disk_ids: Vec<&str> = disk.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(disk_ids.contains(&"select"), "{disk_ids:?}");
        assert!(disk_ids.contains(&"format"), "{disk_ids:?}");
        assert!(disk_ids.contains(&"benchmark"), "{disk_ids:?}");
        let partition = disk
            .children
            .iter()
            .find(|child| child.kind == "disks.partition")
            .expect("partition child");
        let part_ids: Vec<&str> = partition.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(part_ids.contains(&"select"), "{part_ids:?}");
        assert!(
            part_ids.iter().any(|id| matches!(*id, "mount" | "unmount")),
            "{part_ids:?}"
        );

        let mut fresh = DisksApp::new();
        let mut empty = AutoCommit::new();
        fresh.sync(&mut empty, &ui::automerge::ROOT);
        assert!(DisksDoc::topology_json(&empty).is_some());
        assert_eq!(DisksDoc::tab(&empty), "overview");
        assert_eq!(DisksDoc::selected(&empty), 0);

        let mut app = DisksApp::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "disks.system");
        assert!(
            !standalone[0].children.is_empty(),
            "standalone publishes the topology tree"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "disks").is_none());
        assert!(
            ui::resources::resource(&app, &seeded, "disk:nvme0n1")
                .is_some_and(|disk| disk.actions.iter().any(|action| action.id == "select")),
            "a hosted drive stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras.is_empty(),
            "hosted must not republish disks.system beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }
}
