//! Composition through the original caller's published actions and framework Jobs.
use crate::project_links::{ActionLink, Experiment, InvocationLink};
use crate::resource::{ChipProject, ProjectAction, ProjectState};
use crate::view::ChipEffect;
use ui::collaboration::published_actions::{PublishedActionAnswer, PublishedActions};
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::{
    CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, ViewResourceQuery,
};
use ui::tilo_actions::{ActionOutput, InvocationId, Resource as PublishedResource};
use ui::tilo_resource::{ActionRef, Effect};
use ui::widget::{Column, Element, Property, PropertyList, Text};

pub fn resource(experiment: &Experiment) -> PublishedResource {
    PublishedResource::new(
        format!("chip_design.experiment:{}", experiment.name),
        "chip_design.experiment",
        &experiment.name,
    )
    .with_action(ui::framework::read_item(format!(
        "chip_design.experiment:{}",
        experiment.name
    )))
    .with_action(
        ChipEffect::RunExperiment {
            name: experiment.name.clone(),
        }
        .bound(),
    )
    .with_action(ui::tilo_resource::items::bound(
        &crate::resource::experiment_remove_declaration(),
        &[("name", serde_json::Value::String(experiment.name.clone()))],
    ))
}

async fn page(actions: &PublishedActions, query: CatalogueQuery) -> Result<CataloguePage, String> {
    match actions.query(query)?.await {
        PublishedActionAnswer::Answer(RemoteActionMessage::CatalogueResponse {
            result: Ok(page),
            ..
        }) if !page.stale_cursor => Ok(page),
        _ => Err("original action discovery unavailable; no invocation was issued".into()),
    }
}

async fn provider(actions: &PublishedActions, link: &ActionLink) -> Result<String, String> {
    crate::project_discovery::provider(&|query| page(actions, query), link).await
}

async fn resolve(
    actions: &PublishedActions,
    link: &ActionLink,
) -> Result<(RemoteActionTarget, String), String> {
    crate::project_discovery::resolve(&|query| page(actions, query), link).await
}

async fn execute(
    actions: PublishedActions,
    link: ActionLink,
    invocation: InvocationId,
    arguments: serde_json::Value,
) -> Result<ActionOutput, String> {
    let (target, revision) = resolve(&actions, &link)
        .await
        .map_err(|error| format!("child UUID {invocation:?} was not dispatched: {error}"))?;
    let argument = InvokeArgument::Json(
        arguments
            .as_object()
            .ok_or("invalid typed experiment arguments")?
            .clone(),
    );
    let request = InvokeRequest {
        invocation,
        target,
        argument,
        context: None,
        confirmation: None,
        revision: Some(revision),
        on_behalf_of: None,
    };
    match actions
        .invoke_and_wait(request)
        .map_err(|error| {
            format!("child UUID {invocation:?} dispatch outcome unavailable: {error}")
        })?
        .await
    {
        PublishedActionAnswer::Answer(RemoteActionMessage::Receipt(receipt))
            if receipt.invocation == invocation =>
        {
            match receipt.status {
                ReceiptStatus::Completed { output } => Ok(output),
                ReceiptStatus::Failed { error } => Err(format!(
                    "original invocation {invocation:?} failed: {}",
                    error.message
                )),
                ReceiptStatus::Rejected { error } => Err(format!(
                    "original invocation {invocation:?} refused: {error:?}"
                )),
                ReceiptStatus::Cancelled { .. } => {
                    Err(format!("original invocation {invocation:?} cancelled"))
                }
                ReceiptStatus::Accepted => Err(format!(
                    "original invocation {invocation:?} remains unresolved"
                )),
            }
        }
        _ => Err(format!(
            "original invocation {invocation:?} outcome unknown; observe that original job; do not reissue it"
        )),
    }
}

fn retain(cx: &EffectCx<ChipProject>, name: &str, link: &InvocationLink) -> Result<(), String> {
    cx.apply(&ProjectAction::RetainInvocation {
        name: name.into(),
        json: serde_json::to_string(&link).map_err(|error| error.to_string())?,
    });
    Ok(())
}

pub fn run(name: String, cx: &mut EffectCx<ChipProject>) -> Result<Effected, String> {
    let experiment = cx
        .live()
        .with(|state| state.experiments.get(&name).cloned())
        .ok_or_else(|| crate::view::tr("chip-project-unavailable"))?;
    experiment.validate()?;
    let actions = cx.published_actions()?;
    let parent = cx
        .invocation()
        .ok_or("experiment requires original invocation custody")?;
    let invocation = InvocationId::new();
    let source_link = InvocationLink {
        parent,
        invocation,
        action: experiment.source.clone(),
        resources: Vec::new(),
    };
    let source = experiment.source.clone();
    Ok(cx.spawn(
        move |_| {
            execute(
                actions,
                source,
                invocation,
                serde_json::json!({"start_chars":0,"max_chars":4096}),
            )
        },
        move |(), result, cx| {
            // Canonical Jobs own each original child. Informative metadata is
            // queued after its answer, never an optimistic execution precondition.
            let mut completed_source = source_link;
            if let Ok(output) = &result {
                completed_source.resources.clone_from(&output.resources);
            }
            let _ = retain(cx, &name, &completed_source);
            result?;
            // A snapshot page is an original reference, not proof of the later check's inputs.
            let current = cx
                .live()
                .with(|state| state.experiments.get(&name).cloned())
                .ok_or_else(|| crate::view::tr("chip-project-unavailable"))?;
            if current.source != experiment.source
                || current.check != experiment.check
                || current.check_id != experiment.check_id
            {
                return Err("experiment binding changed during source inspection".into());
            }
            let actions = cx.published_actions()?;
            let invocation = InvocationId::new();
            let child_link = InvocationLink {
                parent,
                invocation,
                action: experiment.check.clone(),
                resources: Vec::new(),
            };
            let arguments = serde_json::json!({"check_id":experiment.check_id});
            Ok(cx.spawn(
                move |_| execute(actions, experiment.check, invocation, arguments),
                move |(), result, cx| {
                    let mut completed = child_link;
                    if let Ok(output) = &result {
                        completed.resources.clone_from(&output.resources);
                    }
                    let _ = retain(cx, &name, &completed);
                    // A refused metadata edit must not replace an original child
                    // result or lose its UUID in an unknown failure message.
                    Ok(Effected::Done(result?))
                },
            ))
        },
    ))
}

pub fn open(name: &str, source: bool, cx: &mut EffectCx<ChipProject>) -> Result<Effected, String> {
    let experiment = cx
        .live()
        .with(|state| state.experiments.get(name).cloned())
        .ok_or_else(|| crate::view::tr("chip-project-unavailable"))?;
    let link = if source {
        experiment.source
    } else {
        experiment.check
    };
    let actions = cx.published_actions()?;
    Ok(cx.spawn(
        move |_| async move {
            let provider = provider(&actions, &link).await?;
            let views = page(
                &actions,
                CatalogueQuery {
                    provider: Some(provider.clone()),
                    view_resource: Some(ViewResourceQuery {
                        id: link.resource.id.clone(),
                        kind: link.resource.kind.clone(),
                    }),
                    ..Default::default()
                },
            )
            .await?;
            if views.budget_exhausted || views.revision.is_empty() {
                return Err("original resource view discovery is incomplete".into());
            }
            let mut matching = views.rows.iter().filter(|row| {
                row.provider == provider
                    && row.app_id == link.route.app_id
                    && row.resource.id == link.resource.id
                    && row.resource.kind == link.resource.kind
                    && row.action.as_ref().is_some_and(|action| {
                        action.semantic_identity.as_ref() == Some(&link.semantic_identity)
                            && action.view.as_ref().is_some_and(|view| view.primary)
                    })
            });
            let row = matching
                .next()
                .ok_or("original resource has no current primary view")?;
            if matching.next().is_some() {
                return Err("original resource primary view is ambiguous".into());
            }
            let target = row.target().ok_or("original view action is missing")?;
            crate::project_discovery::provider(&|query| page(&actions, query), &link).await?;
            let invocation = InvocationId::new();
            let request = InvokeRequest {
                invocation,
                target,
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some(views.revision),
                on_behalf_of: None,
            };
            match actions
                .invoke_and_wait(request)
                .map_err(|error| {
                    format!("original view invocation {invocation:?} dispatch unavailable: {error}")
                })?
                .await
            {
                PublishedActionAnswer::Answer(RemoteActionMessage::Receipt(receipt))
                    if receipt.invocation == invocation =>
                {
                    match receipt.status {
                        ReceiptStatus::Completed { output } => Ok(output),
                        other => Err(format!(
                            "original view invocation {invocation:?} refused: {other:?}"
                        )),
                    }
                }
                _ => Err(format!(
                    "original view invocation {invocation:?} outcome unknown"
                )),
            }
        },
        |(), result, _| Ok(Effected::Done(result?)),
    ))
}

pub fn body(cx: &ViewCx<ChipProject>, state: &ProjectState) -> Element {
    let mut body = Column::new().with_intrinsic_height();
    if state.experiments_truncated {
        body = body.push(Box::new(PropertyList::new(vec![Property::new(
            crate::view::tr("chip-projects-truncated-label"),
            crate::view::tr("chip-projects-truncated"),
        )])));
    }
    for experiment in state.experiments.values() {
        let mut properties = vec![
            Property::new(
                crate::view::tr("chip-project-reference-scope"),
                crate::view::tr("chip-project-reference-scope-value"),
            ),
            Property::new(crate::view::tr("chip-project-check"), &experiment.check_id),
            Property::new(
                crate::view::tr("chip-project-source"),
                &experiment.source.semantic_identity.key,
            ),
            Property::new(
                crate::view::tr("chip-project-repository"),
                &experiment.check.semantic_identity.key,
            ),
        ];
        if experiment.references_truncated {
            properties.push(Property::new(
                crate::view::tr("chip-project-references-truncated-label"),
                crate::view::tr("chip-project-references-truncated"),
            ));
        }
        properties.extend(experiment.invocations.iter().map(|link| {
            Property::new(
                crate::view::tr("chip-original-invocation"),
                link.invocation.0.to_string(),
            )
        }));
        properties.extend(
            experiment
                .invocations
                .iter()
                .flat_map(|link| link.resources.iter())
                .map(|resource| {
                    Property::new(crate::view::tr("chip-original-output-reference"), resource)
                }),
        );
        body = body
            .push(Box::new(
                Text::new(&experiment.name).with_role(ui::tokens::TextRole::Heading),
            ))
            .push(Box::new(cx.effect_button(
                &crate::view::tr("chip-open-source"),
                &ChipEffect::OpenProjectResource {
                    name: experiment.name.clone(),
                    source: true,
                },
            )))
            .push(Box::new(cx.effect_button(
                &crate::view::tr("chip-open-repository"),
                &ChipEffect::OpenProjectResource {
                    name: experiment.name.clone(),
                    source: false,
                },
            )))
            .push(Box::new(
                cx.button(
                    &crate::view::tr("chip-remove-experiment"),
                    ActionRef::<ChipProject, String>::new("remove-experiment", &["name"])
                        .with(experiment.name.clone()),
                ),
            ))
            .push(Box::new(PropertyList::new(properties)))
            .push(Box::new(cx.effect_button(
                &crate::view::tr("chip-run-experiment"),
                &ChipEffect::RunExperiment {
                    name: experiment.name.clone(),
                },
            )));
    }
    Box::new(body)
}
