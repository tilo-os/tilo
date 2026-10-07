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
    pub verifications: BTreeMap<String, VerificationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProjectAction {
    ImportReport { json: String },
    RemoveReport { report: String },
    ImportVerification { json: String },
    RemoveVerification { report: String },
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
            .chain(
                state
                    .verifications
                    .iter()
                    .map(|(id, record)| verification_resource(id, record)),
            )
            .collect()
    }

    fn child(state: &ProjectState, resource: &str) -> Option<PublishedResource> {
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
        _doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        match item {
            None => Ok(
                serde_json::json!({"kind": Self::KIND, "reports": state.reports.iter().map(|(id, report)| serde_json::json!({
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
            ActionDecl::new("import-report", "Import analytical report", vec![ActionParam::required("json", ParamType::Text, "Complete model_profile JSON report")], |args, _| {
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
            }).describe("Import reported integer RTL evidence; does not authenticate artifacts or establish model quality or physical PPA."),
            remove_verification_declaration(),
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
            .chain(state.verifications.iter().map(|(id, record)| SearchEntry {
                anchor: Some(id.clone()),
                title: record.receipt.evidence.clone(),
                text: format!(
                    "{:?} {}",
                    record.receipt.status,
                    record.receipt.excludes.join(" ")
                ),
                ..Default::default()
            }))
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
