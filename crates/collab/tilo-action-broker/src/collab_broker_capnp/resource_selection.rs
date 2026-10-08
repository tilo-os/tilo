//! Attended selection does not grant authority or invoke the selected action.
use super::continuation_frame::CheckedContinuation;
use super::{CapnpChannelHandle, Promise, Rc, channel_handle};
use tilo_actions::remote::{
    ActionHostError, CatalogueQuery, RemoteActionFrame, RemoteActionMessage,
    ResourceSelectionCandidate, ResourceSelectionFilter, ResourceSelectionPromptRequest,
    SelectedResource,
};

use tilo_actions::remote::MAX_RESOURCE_SELECTION_CANDIDATES as MAX_CANDIDATES;
const MAX_QUERIES: usize = 128;

fn eligible(row: &tilo_actions::remote::CatalogueRow, filter: &ResourceSelectionFilter) -> bool {
    row.app_id == filter.app_id
        && row.resource.kind == filter.resource_kind
        && row.action.as_ref().is_some_and(|action| {
            action.address.action == filter.action
                && action.resource_kind == filter.resource_kind
                && action
                    .semantic_identity
                    .as_ref()
                    .is_some_and(tilo_actions::ResourceSemanticIdentity::is_valid)
        })
}

#[expect(
    clippy::future_not_send,
    reason = "catalogue capabilities run on the broker LocalSet"
)]
async fn select(
    channel: &CapnpChannelHandle,
    filter: ResourceSelectionFilter,
    continuation: &CheckedContinuation,
) -> Result<Option<SelectedResource>, ActionHostError> {
    if [&filter.app_id, &filter.resource_kind, &filter.action]
        .iter()
        .any(|field| field.is_empty() || field.len() > 256)
    {
        return Err(ActionHostError::NotPermitted);
    }
    let service = channel
        .state
        .services
        .resource_selections
        .as_ref()
        .ok_or(ActionHostError::NotPermitted)?;
    let context = channel
        .document_read_context()
        .map_err(|_| ActionHostError::NotPermitted)?;
    continuation
        .check_publication(channel)
        .await
        .map_err(|_| ActionHostError::NotPermitted)?;
    let CandidateBudget {
        candidates,
        truncated,
        ..
    } = enumerate(channel, continuation, &filter).await?;
    let route = channel.route();
    continuation
        .check_publication(channel)
        .await
        .map_err(|_| ActionHostError::NotPermitted)?;
    if candidates.is_empty() {
        return Err(ActionHostError::Unavailable {
            reason: "no current authorized resources offer the requested semantic action".into(),
        });
    }
    let mut choice = service.select(
        context.clone(),
        ResourceSelectionPromptRequest {
            room: route.cluster_id.clone(),
            source_app_id: route.app_id,
            source_record_id: route.record_id,
            candidates: candidates.clone(),
            truncated,
        },
    );
    let chosen = loop {
        tokio::select! {
            result = &mut choice => break result.map_err(|reason| ActionHostError::Unavailable { reason })?,
            () = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                context.revalidate().map_err(|_| ActionHostError::NotPermitted)?;
                continuation.check_publication(channel).await.map_err(|_| ActionHostError::NotPermitted)?;
            }
        }
    };
    context
        .revalidate()
        .map_err(|_| ActionHostError::NotPermitted)?;
    continuation
        .check_publication(channel)
        .await
        .map_err(|_| ActionHostError::NotPermitted)?;
    let Some(token) = chosen else {
        return Ok(None);
    };
    let selected = candidates
        .into_iter()
        .find(|candidate| candidate.token == token)
        .ok_or(ActionHostError::NotPermitted)?
        .selected;
    revalidate_selection(channel, continuation, &selected).await?;
    context
        .revalidate()
        .map_err(|_| ActionHostError::NotPermitted)?;
    // A Found resolution carries the validated row, not a new page revision.
    Ok(Some(selected))
}

#[derive(Default)]
struct CandidateBudget {
    candidates: Vec<ResourceSelectionCandidate>,
    queries: usize,
    truncated: bool,
}

#[expect(
    clippy::future_not_send,
    reason = "catalogue capabilities run on the broker LocalSet"
)]
async fn enumerate(
    channel: &CapnpChannelHandle,
    continuation: &CheckedContinuation,
    filter: &ResourceSelectionFilter,
) -> Result<CandidateBudget, ActionHostError> {
    let providers = channel
        .state
        .action_query(
            &continuation.caller,
            &CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            },
        )
        .await
        .ok_or(ActionHostError::NotPermitted)??;
    if providers.stale_cursor {
        return Err(ActionHostError::NotPermitted);
    }
    let room = channel.route().cluster_id;
    let mut budget = CandidateBudget::default();
    let matching: Vec<_> = providers
        .providers
        .into_iter()
        .filter(|provider| {
            provider.browsable
                && provider.app_id == filter.app_id
                && provider.room == room
                && !provider.record_id.is_empty()
        })
        .collect();
    budget.truncated |= matching.len() > MAX_CANDIDATES;
    for discovered in matching.into_iter().take(MAX_CANDIDATES) {
        continuation
            .check_publication(channel)
            .await
            .map_err(|_| ActionHostError::NotPermitted)?;
        collect_provider(channel, continuation, filter, &discovered, &mut budget).await?;
    }
    Ok(budget)
}

#[expect(
    clippy::future_not_send,
    reason = "catalogue capabilities run on the broker LocalSet"
)]
async fn collect_provider(
    channel: &CapnpChannelHandle,
    continuation: &CheckedContinuation,
    filter: &ResourceSelectionFilter,
    provider: &tilo_actions::remote::CatalogueProvider,
    budget: &mut CandidateBudget,
) -> Result<(), ActionHostError> {
    let mut pending = std::collections::VecDeque::from([(None, Vec::new(), None)]);
    let mut visited = std::collections::HashSet::new();
    while let Some((parent_resource, cursor, revision)) = pending.pop_front() {
        if budget.queries >= MAX_QUERIES || budget.candidates.len() >= MAX_CANDIDATES {
            budget.truncated = true;
            break;
        }
        continuation
            .check_publication(channel)
            .await
            .map_err(|_| ActionHostError::NotPermitted)?;
        budget.queries = budget.queries.saturating_add(1);
        let page = channel
            .state
            .action_query(
                &continuation.caller,
                &CatalogueQuery {
                    provider: Some(provider.provider.clone()),
                    parent_resource: parent_resource.clone(),
                    cursor,
                    revision,
                    max_results: 64,
                    ..CatalogueQuery::default()
                },
            )
            .await
            .ok_or(ActionHostError::NotPermitted)??;
        if page.stale_cursor {
            return Err(ActionHostError::NotPermitted);
        }
        budget.truncated |= page.budget_exhausted;
        for row in page.rows {
            if row.provider != provider.provider || row.app_id != provider.app_id {
                return Err(ActionHostError::NotPermitted);
            }
            if eligible(&row, filter) && budget.candidates.len() >= MAX_CANDIDATES {
                budget.truncated = true;
            }
            if eligible(&row, filter) && budget.candidates.len() < MAX_CANDIDATES {
                budget.candidates.push(ResourceSelectionCandidate {
                    token: uuid::Uuid::new_v4(),
                    selected: SelectedResource {
                        provider: provider.clone(),
                        row: row.clone(),
                        revision: page.revision.clone(),
                    },
                });
            }
            if row.has_children && pending.len() >= MAX_QUERIES {
                budget.truncated = true;
            }
            if row.has_children
                && visited.insert(row.resource.id.clone())
                && pending.len() < MAX_QUERIES
            {
                pending.push_back((Some(row.resource.id), Vec::new(), None));
            }
        }
        if !page.next_cursor.is_empty() && pending.len() >= MAX_QUERIES {
            budget.truncated = true;
        }
        if !page.next_cursor.is_empty() && pending.len() < MAX_QUERIES {
            pending.push_back((parent_resource, page.next_cursor, Some(page.revision)));
        }
    }
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "catalogue capabilities run on the broker LocalSet"
)]
async fn revalidate_selection(
    channel: &CapnpChannelHandle,
    continuation: &CheckedContinuation,
    selected: &SelectedResource,
) -> Result<(), ActionHostError> {
    revalidate_provider(channel, continuation, selected).await?;
    let target = selected.row.target().ok_or(ActionHostError::NotPermitted)?;
    let page = channel
        .state
        .action_query(
            &continuation.caller,
            &CatalogueQuery::resolve(&target, selected.revision.clone()),
        )
        .await
        .ok_or(ActionHostError::NotPermitted)??;
    continuation
        .check_publication(channel)
        .await
        .map_err(|_| ActionHostError::NotPermitted)?;
    if page.stale_cursor || page.rows.len() != 1 || page.rows.first() != Some(&selected.row) {
        return Err(ActionHostError::NotPermitted);
    }
    revalidate_provider(channel, continuation, selected).await?;
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "catalogue capabilities run on the broker LocalSet"
)]
async fn revalidate_provider(
    channel: &CapnpChannelHandle,
    continuation: &CheckedContinuation,
    selected: &SelectedResource,
) -> Result<(), ActionHostError> {
    let current = channel
        .state
        .action_query(
            &continuation.caller,
            &CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            },
        )
        .await
        .ok_or(ActionHostError::NotPermitted)??;
    if current.stale_cursor
        || !selected.provider.browsable
        || selected.provider.record_id.is_empty()
        || selected.provider.room != channel.route().cluster_id
        || !current.providers.contains(&selected.provider)
    {
        return Err(ActionHostError::NotPermitted);
    }
    Ok(())
}

pub(super) fn serve(
    channel: Rc<CapnpChannelHandle>,
    correlation: u64,
    filter: ResourceSelectionFilter,
    continuation: Option<CheckedContinuation>,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    Promise::from_future(async move {
        let result = match continuation {
            Some(continuation) => select(&channel, filter, &continuation).await,
            None => Err(ActionHostError::NotPermitted),
        };
        let frame = RemoteActionFrame::new(RemoteActionMessage::ResourceSelectionResponse {
            correlation,
            result,
        })
        .encode()
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_frame(&frame);
        Ok(())
    })
}
