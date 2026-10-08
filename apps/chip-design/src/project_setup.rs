//! Human setup uses shared inputs and the platform's typed resource selection.
use crate::project_links::{ActionLink, Experiment};
use crate::resource::{ChipProject, ProjectAction};
use crate::view::{ChipEffect, tr};
use ui::automerge::{ROOT, ReadDoc};
use ui::collaboration::published_actions::{PublishedActionAnswer, PublishedActions};
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::{
    CanonicalActionRoute, RemoteActionMessage, ResourceSelectionFilter, SelectedResource,
    ViewResourceQuery,
};
use ui::tilo_actions::{ActionError, ActionOutput};
use ui::tilo_resource::{ActionRef, Doc, Model, ModelError, Slot};
use ui::widget::{Column, Element, Field};

pub fn set_field(doc: &mut Doc, key: &str, value: &str) -> Result<(), ActionError> {
    if value.len() > 256 {
        return Err(ActionError::Refused(
            "experiment field exceeds bounds".into(),
        ));
    }
    value
        .to_owned()
        .store(doc, Slot::Key(&ROOT, key))
        .map_err(|error| ActionError::Refused(error.to_string()))
}
pub fn read_field(doc: &Doc, key: &str) -> Result<String, ModelError> {
    match doc.get(ROOT, key)? {
        Some((value, id)) => String::load(doc, &value, &id),
        None => Ok(String::new()),
    }
}

async fn select(
    actions: &PublishedActions,
    filter: ResourceSelectionFilter,
    dismissed: &str,
    unavailable: &str,
) -> Result<ActionLink, String> {
    let answer = actions.select_resource(filter)?.await;
    match answer {
        PublishedActionAnswer::Answer(RemoteActionMessage::ResourceSelectionResponse {
            result: Ok(Some(selected)),
            ..
        }) => link(selected),
        PublishedActionAnswer::Answer(RemoteActionMessage::ResourceSelectionResponse {
            result: Ok(None),
            ..
        }) => Err(dismissed.to_owned()),
        _ => Err(unavailable.to_owned()),
    }
}
fn link(selected: SelectedResource) -> Result<ActionLink, String> {
    let descriptor = selected
        .row
        .action
        .ok_or("selected resource has no original action")?;
    if selected.row.provider != selected.provider.provider
        || selected.row.app_id != selected.provider.app_id
        || selected.provider.record_id.is_empty()
    {
        return Err("selected resource is not its original record publication".into());
    }
    Ok(ActionLink {
        provider: selected.provider.provider,
        route: CanonicalActionRoute {
            room: selected.provider.room,
            app_id: selected.provider.app_id,
            record_id: selected.provider.record_id,
        },
        address: descriptor.address,
        resource: ViewResourceQuery {
            id: selected.row.resource.id,
            kind: selected.row.resource.kind,
        },
        semantic_identity: descriptor
            .semantic_identity
            .ok_or("selected resource lacks genuine semantic identity")?,
    })
}

pub fn choose(cx: &mut EffectCx<ChipProject>) -> Result<Effected, String> {
    let (name, check_id) = cx.live().with(|state| {
        (
            state.experiment_name.clone(),
            state.experiment_check_id.clone(),
        )
    });
    if name.trim().is_empty() || check_id.trim().is_empty() {
        return Err(tr("chip-project-fields-required"));
    }
    if cx
        .live()
        .with(|state| state.experiments.contains_key(&name))
    {
        return Err(tr("chip-project-name-used"));
    }
    let actions = cx.published_actions()?;
    // Fluent catalogues belong to the UI thread, not the async worker.
    let dismissed = tr("chip-selection-dismissed");
    let unavailable = tr("chip-selection-unavailable");
    Ok(cx.spawn(
        move |_| async move {
            let source = select(
                &actions,
                ResourceSelectionFilter {
                    app_id: "so.tilo.code-editor".into(),
                    resource_kind: "code_editor.file".into(),
                    action: "read-text-snapshot".into(),
                },
                &dismissed,
                &unavailable,
            )
            .await?;
            let check = select(
                &actions,
                ResourceSelectionFilter {
                    app_id: "so.tilo.git-client".into(),
                    resource_kind: "git_client.repo".into(),
                    action: "run-approved-check".into(),
                },
                &dismissed,
                &unavailable,
            )
            .await?;
            let experiment = Experiment {
                name,
                check_id,
                source,
                check,
                invocations: Vec::new(),
                references_truncated: false,
            };
            experiment.validate()?;
            Ok::<_, String>(experiment)
        },
        |(), selected, cx| {
            let experiment = selected?;
            if !cx.live().with(|state| {
                state.experiment_name == experiment.name
                    && state.experiment_check_id == experiment.check_id
            }) {
                return Err(tr("chip-project-fields-changed"));
            }
            // Selection is non-authoritative. The normal attributed binding action
            // validates and commits the configuration; live UI shows only its result.
            cx.apply(&ProjectAction::BindExperiment {
                json: serde_json::to_string(&experiment).map_err(|error| error.to_string())?,
            });
            Ok(Effected::Done(ActionOutput {
                value: Some(serde_json::json!({
                    "selected_original_resources": true,
                    "project_binding_submitted": true
                })),
                resources: Vec::new(),
            }))
        },
    ))
}

pub fn body(cx: &ViewCx<ChipProject>) -> Element {
    let name: ActionRef<ChipProject, String> = ActionRef::new("set-experiment-name", &["value"]);
    let check: ActionRef<ChipProject, String> = ActionRef::new("set-experiment-check", &["value"]);
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(Box::new(Field::new(
                tr("chip-project-name"),
                cx.input(|state| state.experiment_name.clone(), name),
            )))
            .push(Box::new(Field::new(
                tr("chip-project-check"),
                cx.input(|state| state.experiment_check_id.clone(), check),
            )))
            .push(Box::new(cx.effect_button(
                &tr("chip-link-project"),
                &ChipEffect::LinkProject,
            ))),
    )
}
