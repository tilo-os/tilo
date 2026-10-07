//! One CRDT resource/action vocabulary for imported engineering evidence.

use std::collections::BTreeMap;

use crate::view::ChipEffect;
use serde::{Deserialize, Serialize};
use ui::automerge::{ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_actions::{Action, ActionError, ActionParam, ActionRegistry, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, Effect, Invoke, ModelError, Resource, SearchEntry};

use crate::model::{MAX_REPORTS, StorageReport, report_id};

const REPORTS: &str = "reports";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub reports: BTreeMap<String, StorageReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProjectAction {
    ImportReport { json: String },
    RemoveReport { report: String },
}

impl Action for ProjectAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
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
            Self::RemoveReport { report } => {
                if let Some((Value::Object(ObjType::Map), reports)) = doc.get(ROOT, REPORTS)? {
                    doc.delete(&reports, report.as_str())?;
                }
            }
        }
        Ok(())
    }
}

pub struct ChipProject;

impl Resource for ChipProject {
    const KIND: &'static str = "chip_design.project";
    const LABEL: &'static str = "Chip Design";
    const ACTION_TYPE: &'static str = "chip_design.action";
    type State = ProjectState;
    type Action = ProjectAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<ProjectAction>(Self::ACTION_TYPE);
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
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
            .reports
            .iter()
            .map(|(id, report)| report_resource(id, report))
            .collect()
    }

    fn child(state: &ProjectState, resource: &str) -> Option<PublishedResource> {
        let id = resource.strip_prefix("chip_design.report:")?;
        state
            .reports
            .get(id)
            .map(|report| report_resource(id, report))
    }

    const READABLE: bool = true;

    fn read_content(
        state: &ProjectState,
        _doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        match item {
            None => Ok(
                serde_json::json!({"kind": Self::KIND, "reports": state.reports.iter().map(|(id, report)| serde_json::json!({
                "resource": report_resource_id(id), "model": report.model,
                "revision": report.revision, "evidence": report.evidence,
            })).collect::<Vec<_>>()}),
            ),
            Some(item) => {
                let id = item
                    .strip_prefix("chip_design.report:")
                    .ok_or("invalid report resource")?;
                let report = state.reports.get(id).ok_or("report no longer exists")?;
                serde_json::to_value(report).map_err(|error| error.to_string())
            }
        }
    }

    fn read(doc: &Doc) -> Result<ProjectState, ModelError> {
        let mut state = ProjectState::default();
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
        Ok(state)
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new("import-report", "Import analytical report", vec![ActionParam::required("json", ParamType::Text, "Complete model_profile JSON report")], |args, _| {
                let json = args.text("json").ok_or("missing report JSON")?.to_owned();
                StorageReport::parse(&json)?;
                Ok(ProjectAction::ImportReport { json })
            }).describe("Import reported storage evidence; does not authenticate the model or validate chip performance."),
            remove_declaration(),
        ]
    }

    fn search(state: &ProjectState) -> Vec<SearchEntry> {
        state
            .reports
            .iter()
            .map(|(id, report)| SearchEntry {
                anchor: Some(id.clone()),
                title: report.model.clone(),
                text: report.revision.clone(),
                ..Default::default()
            })
            .collect()
    }
}

pub fn import(json: String) -> Result<Invoke<ChipProject>, String> {
    StorageReport::parse(&json)?;
    Invoke::apply(&ProjectAction::ImportReport { json }).map_err(|error| error.to_string())
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
