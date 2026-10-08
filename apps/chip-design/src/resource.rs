//! One CRDT resource/action vocabulary for imported engineering evidence.

use std::collections::BTreeMap;

use crate::view::ChipEffect;
use serde::{Deserialize, Serialize};
use ui::automerge::{ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_actions::{Action, ActionError, ActionParam, ActionRegistry, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, Effect, Invoke, ModelError, Resource, SearchEntry};

use crate::model::{MAX_REPORTS, StorageReport, report_id};
use crate::verification::VerificationRecord;

const REPORTS: &str = "reports";
const VERIFICATION_PREFIX: &str = "verification:";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub reports: BTreeMap<String, StorageReport>,
    pub experiments: BTreeMap<String, crate::project_links::Experiment>,
    pub experiments_truncated: bool,
    pub experiment_name: String,
    pub experiment_check_id: String,
    pub operator_graph: crate::operator_graph::OperatorState,
    pub verifications: BTreeMap<String, VerificationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProjectAction {
    RemoveExperiment {
        name: String,
    },
    SetExperimentName {
        value: String,
    },
    SetExperimentCheck {
        value: String,
    },
    BindExperiment {
        json: String,
    },
    RetainInvocation {
        name: String,
        json: String,
    },
    EditGraph {
        edit: crate::operator_graph::OperatorAction,
    },
    ImportReport {
        json: String,
    },
    RemoveReport {
        report: String,
    },
    ImportVerification {
        json: String,
    },
    RemoveVerification {
        report: String,
    },
}

impl Action for ProjectAction {
    fn change_kind(&self) -> ui::tilo_actions::ChangeKind {
        if self.external_reference() {
            ui::tilo_actions::ChangeKind::External
        } else {
            ui::tilo_actions::ChangeKind::Content
        }
    }
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::RemoveExperiment { name } => crate::project_links::remove(doc, name)?,
            Self::SetExperimentName { value } => {
                crate::project_setup::set_field(doc, "experiment-name", value)?;
            }
            Self::SetExperimentCheck { value } => {
                crate::project_setup::set_field(doc, "experiment-check", value)?;
            }
            Self::BindExperiment { json } => {
                let experiment =
                    crate::project_links::Experiment::parse(json).map_err(ActionError::Refused)?;
                if !experiment.invocations.is_empty() {
                    return Err(ActionError::Refused(
                        "binding cannot introduce invocation references".into(),
                    ));
                }
                let key = format!("{}{}", crate::project_links::PREFIX, experiment.name);
                if doc.get(ROOT, &key)?.is_some()
                    || crate::project_links::has_references(doc, &experiment.name)
                {
                    return Err(ActionError::Refused(
                        "experiment already exists; retain its original history".into(),
                    ));
                }
                if doc
                    .keys(ROOT)
                    .filter(|key| key.starts_with(crate::project_links::PREFIX))
                    .count()
                    >= crate::project_links::MAX_PROJECTS
                {
                    return Err(ActionError::Refused("experiment limit reached".into()));
                }
                doc.put(ROOT, key, json.as_str())?;
            }
            Self::RetainInvocation { name, json } => crate::project_links::retain(doc, name, json)?,
            Self::EditGraph { edit } => edit.apply(doc)?,
            Self::ImportReport { json } => {
                StorageReport::parse(json).map_err(ActionError::Refused)?;
                let id = report_id(json);
                let Some((Value::Object(ObjType::Map), reports)) = doc.get(ROOT, REPORTS)? else {
                    return Err(ActionError::Refused(
                        "project report collection is not initialized".into(),
                    ));
                };
                if doc.get(&reports, &id)?.is_some() {
                    return Ok(());
                }
                if doc.length(&reports) >= MAX_REPORTS {
                    return Err(ActionError::Refused("project report limit reached".into()));
                }
                doc.put(&reports, id, json.as_str())?;
            }
            Self::ImportVerification { json } => {
                let (id, stored) =
                    VerificationRecord::import(json).map_err(ActionError::Refused)?;
                let key = verification_key(&id);
                if doc.get(ROOT, &key)?.is_some() {
                    return Ok(());
                }
                if doc
                    .keys(ROOT)
                    .filter(|key| key.starts_with(VERIFICATION_PREFIX))
                    .count()
                    >= MAX_REPORTS
                {
                    return Err(ActionError::Refused(
                        "project verification limit reached".into(),
                    ));
                }
                doc.put(ROOT, key, stored)?;
            }
            Self::RemoveVerification { report } => {
                doc.delete(ROOT, verification_key(report))?;
            }
            Self::RemoveReport { report } => {
                if let Some((Value::Object(ObjType::Map), reports)) = doc.get(ROOT, REPORTS)? {
                    doc.delete(&reports, report.as_str())?;
                }
            }
        }
        Ok(())
    }
}

impl ProjectAction {
    fn external_reference(&self) -> bool {
        matches!(self, Self::RetainInvocation { .. })
    }
}

pub struct ChipProject;

impl Resource for ChipProject {
    const KIND: &'static str = "chip_design.project";
    const LABEL: &'static str = "Chip Design";
    const ACTION_TYPE: &'static str = "chip_design.action";
    const AUDITED_LOW_MUTATIONS: bool = true;
    fn allows_low_mutation(action: &Self::Action) -> bool {
        matches!(action, ProjectAction::RetainInvocation { .. })
    }
    type State = ProjectState;
    type Action = ProjectAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<ProjectAction>(Self::ACTION_TYPE);
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        crate::operator_graph::OperatorGraph::initialize(doc)?;
        match doc.get(ROOT, REPORTS)? {
            Some((Value::Object(ObjType::Map), _)) => {}
            None => {
                doc.put_object(ROOT, REPORTS, ObjType::Map)?;
            }
            Some(_) => return Err(ModelError::invalid("report collection is not a map")),
        }
        Ok(())
    }

    fn children(state: &ProjectState) -> Vec<PublishedResource> {
        state
            .experiments
            .values()
            .map(crate::project_composition::resource)
            .chain(std::iter::once(graph_child(&state.operator_graph)))
            .chain(
                state
                    .reports
                    .iter()
                    .map(|(id, report)| report_resource(id, report))
                    .chain(
                        state
                            .verifications
                            .iter()
                            .map(|(id, record)| verification_resource(id, record)),
                    ),
            )
            .collect()
    }

    fn child(state: &ProjectState, resource: &str) -> Option<PublishedResource> {
        if let Some(name) = resource.strip_prefix("chip_design.experiment:") {
            return state
                .experiments
                .get(name)
                .map(crate::project_composition::resource);
        }
        if let Some(anchor) = resource.strip_prefix("chip_design.flow:") {
            let (report, kind, id) = crate::evidence_flow_view::split(anchor)?;
            crate::evidence_flow_view::item(state, anchor)?;
            return Some(flow_child(report, kind, id, id));
        }
        if resource == crate::operator_graph::RESOURCE_ID {
            return Some(graph_child(&state.operator_graph));
        }
        if let Some(id) = resource.strip_prefix("chip_design.verification:") {
            return state
                .verifications
                .get(id)
                .map(|record| verification_resource(id, record));
        }
        let id = resource.strip_prefix("chip_design.report:")?;
        state
            .reports
            .get(id)
            .map(|report| report_resource(id, report))
    }

    const READABLE: bool = true;

    fn read_content(
        state: &ProjectState,
        doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if let Some(name) = item.and_then(|item| item.strip_prefix("chip_design.experiment:")) {
            return state
                .experiments
                .get(name)
                .ok_or_else(|| crate::view::tr("chip-project-unavailable"))
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()));
        }
        if let Some(anchor) = item.and_then(|item| item.strip_prefix("chip_design.flow:")) {
            return crate::evidence_flow_view::item(state, anchor)
                .ok_or_else(|| "evidence item no longer exists".into());
        }
        if item == Some(crate::operator_graph::RESOURCE_ID) {
            return crate::operator_graph::OperatorGraph::read_content(
                &state.operator_graph,
                doc,
                None,
            );
        }
        match item {
            None => Ok(
                serde_json::json!({"kind": Self::KIND, "experiments": state.experiments, "experiments_truncated":state.experiments_truncated, "experiment_setup":{"name":state.experiment_name,"check_id":state.experiment_check_id}, "operator_graph": {"resource":crate::operator_graph::RESOURCE_ID,"qualification":"structural_design_only","nodes":state.operator_graph.nodes.len(),"active_edges":state.operator_graph.active.len(),"rejected_candidates":state.operator_graph.rejected.len()}, "reports": state.reports.iter().map(|(id, report)| serde_json::json!({
                "resource": report_resource_id(id), "model": report.model,
                "revision": report.revision, "evidence": report.evidence,
            })).collect::<Vec<_>>(), "verifications": state.verifications.iter().map(|(id, record)| serde_json::json!({
                "resource": verification_resource_id(id), "evidence": record.receipt.evidence,
                "status": record.receipt.status,
            })).collect::<Vec<_>>()}),
            ),
            Some(item) => {
                if let Some(id) = item.strip_prefix("chip_design.verification:") {
                    let record = state
                        .verifications
                        .get(id)
                        .ok_or("verification no longer exists")?;
                    return serde_json::to_value(record).map_err(|error| error.to_string());
                }
                let id = item
                    .strip_prefix("chip_design.report:")
                    .ok_or("invalid report resource")?;
                let report = state.reports.get(id).ok_or("report no longer exists")?;
                serde_json::to_value(report).map_err(|error| error.to_string())
            }
        }
    }

    fn read(doc: &Doc) -> Result<ProjectState, ModelError> {
        let mut state = ProjectState {
            operator_graph: crate::operator_graph::OperatorGraph::read(doc)?,
            experiment_name: crate::project_setup::read_field(doc, "experiment-name")?,
            experiment_check_id: crate::project_setup::read_field(doc, "experiment-check")?,
            ..ProjectState::default()
        };
        for (index, key) in doc
            .keys(ROOT)
            .filter(|key| key.starts_with(crate::project_links::PREFIX))
            .enumerate()
        {
            if index == crate::project_links::MAX_PROJECTS {
                state.experiments_truncated = true;
                break;
            }
            let Some((value, _)) = doc.get(ROOT, &key)? else {
                continue;
            };
            let experiment = crate::project_links::read(
                doc,
                value
                    .as_str()
                    .ok_or_else(|| ModelError::invalid("invalid experiment"))?,
            )
            .map_err(ModelError::invalid)?;
            if key != format!("{}{}", crate::project_links::PREFIX, experiment.name) {
                return Err(ModelError::invalid("experiment identity mismatch"));
            }
            state
                .experiments
                .insert(experiment.name.clone(), experiment);
        }
        let reports = match doc.get(ROOT, REPORTS)? {
            Some((Value::Object(ObjType::Map), reports)) => reports,
            None => return Ok(state),
            Some(_) => return Err(ModelError::invalid("report collection is not a map")),
        };
        {
            for id in doc.keys(&reports) {
                let Some((value, _)) = doc.get(&reports, &id)? else {
                    continue;
                };
                let Some(json) = value.as_str() else {
                    return Err(ModelError::invalid("report is not JSON text"));
                };
                let report = StorageReport::parse(json).map_err(ModelError::invalid)?;
                if report_id(json) != id {
                    return Err(ModelError::invalid("report content identity mismatch"));
                }
                state.reports.insert(id, report);
            }
        }
        for key in doc
            .keys(ROOT)
            .filter(|key| key.starts_with(VERIFICATION_PREFIX))
        {
            let id = key
                .strip_prefix(VERIFICATION_PREFIX)
                .ok_or_else(|| ModelError::invalid("invalid verification key"))?;
            let Some((value, _)) = doc.get(ROOT, &key)? else {
                continue;
            };
            let json = value
                .as_str()
                .ok_or_else(|| ModelError::invalid("verification is not JSON text"))?;
            let record = VerificationRecord::read(json).map_err(ModelError::invalid)?;
            if report_id(json) != id {
                return Err(ModelError::invalid("verification identity mismatch"));
            }
            state.verifications.insert(id.to_owned(), record);
        }
        Ok(state)
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            experiment_remove_declaration(),
            ActionDecl::new("set-experiment-name", "Name experiment", vec![ActionParam::required("value", ParamType::Text, "Experiment name")], |args, _| Ok(ProjectAction::SetExperimentName { value: args.text("value").ok_or("missing experiment name")?.into() })),
            ActionDecl::new("set-experiment-check", "Choose approved check identity", vec![ActionParam::required("value", ParamType::Text, "Identity from the project's existing approved checks")], |args, _| Ok(ProjectAction::SetExperimentCheck { value: args.text("value").ok_or("missing check identity")?.into() })),
            ActionDecl::new("bind-experiment", "Link project actions", vec![ActionParam::required("json", ParamType::Text, "Named experiment with exact current Code and Git record, action and semantic references")], |args, _| {
                let json = args.text("json").ok_or("missing experiment links")?.to_owned();
                let experiment = crate::project_links::Experiment::parse(&json)?;
                if !experiment.invocations.is_empty() { return Err("binding cannot introduce invocation references".into()); }
                Ok(ProjectAction::BindExperiment { json })
            }).describe("Configure links, which confer no authority; running re-resolves the original public actions under current caller permissions."),
            ActionDecl::new("import-report", "Import analytical report", vec![ActionParam::required("json", ParamType::Text, "Complete analytical storage report JSON")], |args, _| {
                let json = args.text("json").ok_or("missing report JSON")?.to_owned();
                StorageReport::parse(&json)?;
                Ok(ProjectAction::ImportReport { json })
            }).describe("Import reported storage evidence; does not authenticate the model or validate chip performance."),
            ActionDecl::new("import-verification", "Import hardware verification receipt", vec![
                ActionParam::required("json", ParamType::Text, "Complete immutable hardware verification receipt JSON")
            ], |args, _| {
                let json = args.text("json").ok_or("missing receipt JSON")?.to_owned();
                VerificationRecord::import(&json)?;
                Ok(ProjectAction::ImportVerification { json })
            }).describe("Import reported engineering evidence; does not authenticate artifacts or establish model quality, silicon measurements, or manufacturability."),
            remove_verification_declaration(),
            remove_declaration(),
        ]
    }

    fn search(state: &ProjectState) -> Vec<SearchEntry> {
        state
            .experiments
            .values()
            .map(|experiment| SearchEntry {
                anchor: Some(format!("experiment:{}", experiment.name)),
                title: experiment.name.clone(),
                text: experiment.check_id.clone(),
                ..Default::default()
            })
            .chain(
                state
                    .reports
                    .iter()
                    .map(|(id, report)| SearchEntry {
                        anchor: Some(id.clone()),
                        title: report.model.clone(),
                        text: report.revision.clone(),
                        ..Default::default()
                    })
                    .chain(state.verifications.iter().map(|(id, record)| SearchEntry {
                        anchor: Some(id.clone()),
                        title: record.receipt.evidence_flow.as_ref().map_or_else(
                            || record.receipt.evidence.clone(),
                            |flow| flow.title.clone(),
                        ),
                        text: format!(
                            "{:?} {}",
                            record.receipt.status,
                            record.receipt.excludes.join(" ")
                        ),
                        ..Default::default()
                    }))
                    .chain(
                        state
                            .verifications
                            .iter()
                            .flat_map(|(id, record)| flow_search(id, record)),
                    ),
            )
            .collect()
    }
}

pub fn import(json: String) -> Result<Invoke<ChipProject>, String> {
    let evidence: serde_json::Value = if json.len() <= crate::model::MAX_REPORT_BYTES {
        serde_json::from_str(&json).map_err(|error| error.to_string())?
    } else {
        return Err("report exceeds the 1 MiB import limit".into());
    };
    let action = if evidence.get("evidence").and_then(serde_json::Value::as_str)
        == Some("analytical_storage_bound")
    {
        StorageReport::parse(&json)?;
        ProjectAction::ImportReport { json }
    } else {
        VerificationRecord::import(&json)?;
        ProjectAction::ImportVerification { json }
    };
    Invoke::apply(&action).map_err(|error| error.to_string())
}

fn remove_declaration() -> ActionDecl<ChipProject> {
    ActionDecl::new(
        "remove-report",
        "Remove report",
        vec![ActionParam::required(
            "report",
            ParamType::Text,
            "Content identity of the report",
        )],
        |args, _| {
            Ok(ProjectAction::RemoveReport {
                report: args
                    .text("report")
                    .ok_or("missing report identity")?
                    .to_owned(),
            })
        },
    )
}

pub(crate) fn report_resource_id(id: &str) -> String {
    format!("chip_design.report:{id}")
}

fn report_resource(id: &str, report: &StorageReport) -> PublishedResource {
    PublishedResource::new(report_resource_id(id), "chip_design.report", &report.model)
        .with_action(
            ChipEffect::Inspect {
                report: Some(id.to_owned()),
            }
            .bound(),
        )
        .with_action(ui::tilo_resource::items::bound(
            &remove_declaration(),
            &[("report", serde_json::Value::String(id.to_owned()))],
        ))
}

fn remove_verification_declaration() -> ActionDecl<ChipProject> {
    ActionDecl::new(
        "remove-verification",
        "Remove verification receipt",
        vec![ActionParam::required(
            "report",
            ParamType::Text,
            "Shared receipt content identity",
        )],
        |args, _| {
            Ok(ProjectAction::RemoveVerification {
                report: args
                    .text("report")
                    .ok_or("missing verification identity")?
                    .to_owned(),
            })
        },
    )
}

pub(crate) fn verification_resource_id(id: &str) -> String {
    format!("chip_design.verification:{id}")
}

fn verification_resource(id: &str, record: &VerificationRecord) -> PublishedResource {
    PublishedResource::new(
        verification_resource_id(id),
        "chip_design.verification",
        &record.receipt.evidence,
    )
    .with_children(flow_children(id, record))
    .with_action(
        ChipEffect::Inspect {
            report: Some(id.to_owned()),
        }
        .bound(),
    )
    .with_action(ui::tilo_resource::items::bound(
        &remove_verification_declaration(),
        &[("report", serde_json::Value::String(id.to_owned()))],
    ))
}

fn verification_key(id: &str) -> String {
    format!("{VERIFICATION_PREFIX}{id}")
}

fn graph_child(state: &crate::operator_graph::OperatorState) -> PublishedResource {
    ui::tilo_resource::publish_child::<crate::operator_graph::OperatorGraph, ChipProject>(
        crate::operator_graph::RESOURCE_ID,
        "Operator graph",
        Some(state),
        |edit| ProjectAction::EditGraph { edit },
    )
    .with_action(ChipEffect::OperatorGraph.bound())
}

fn flow_child(report: &str, kind: &str, id: &str, title: &str) -> PublishedResource {
    let child = PublishedResource::new(
        crate::evidence_flow_view::resource_id(report, kind, id),
        format!("chip_design.flow_{kind}"),
        title,
    )
    .with_action(
        ChipEffect::InspectEvidence {
            report: report.into(),
            item: format!("{kind}/{id}"),
        }
        .bound(),
    );
    if kind == "subflow" {
        child.with_action(
            ChipEffect::OpenEvidenceSubflow {
                report: report.into(),
                id: id.into(),
            }
            .bound(),
        )
    } else {
        child
    }
}
fn flow_children(report: &str, record: &VerificationRecord) -> Vec<PublishedResource> {
    let Some(flow) = &record.receipt.evidence_flow else {
        return Vec::new();
    };
    flow.stages
        .iter()
        .map(|stage| {
            let macros: Vec<_> = stage
                .placement
                .iter()
                .flat_map(|placement| {
                    placement.macros.iter().map(|instance| {
                        flow_child(
                            report,
                            "placement",
                            &format!("{}/{}", stage.id, instance.id),
                            &instance.name,
                        )
                    })
                })
                .collect();
            flow_child(report, "stage", &stage.id, &stage.title).with_children(macros)
        })
        .chain(
            flow.subflows
                .iter()
                .map(|link| flow_child(report, "subflow", &link.id, &link.title)),
        )
        .chain(
            record
                .receipt
                .artifact_sha256
                .keys()
                .map(|path| flow_child(report, "artifact", path, path)),
        )
        .collect()
}
fn flow_search(report: &str, record: &VerificationRecord) -> Vec<SearchEntry> {
    let children = flow_children(report, record);
    children
        .iter()
        .chain(children.iter().flat_map(|child| child.children.iter()))
        .map(|child| SearchEntry {
            anchor: Some(
                child
                    .id
                    .0
                    .strip_prefix("chip_design.flow:")
                    .unwrap_or_default()
                    .to_owned(),
            ),
            title: child.label.clone(),
            text: "reported_unverified".into(),
            ..Default::default()
        })
        .collect()
}

pub(crate) fn experiment_remove_declaration() -> ActionDecl<ChipProject> {
    ActionDecl::new(
        "remove-experiment",
        "Remove experiment links",
        vec![ActionParam::required(
            "name",
            ParamType::Text,
            "Experiment name",
        )],
        |args, _| {
            Ok(ProjectAction::RemoveExperiment {
                name: args.text("name").ok_or("missing experiment name")?.into(),
            })
        },
    )
}
