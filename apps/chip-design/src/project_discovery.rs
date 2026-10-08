//! Discovery keeps public references inert and carries the offered revision.
use crate::project_links::ActionLink;
use ui::tilo_actions::remote::{CataloguePage, CatalogueQuery, CatalogueRow, RemoteActionTarget};

const MAX_RESOURCE_PAGES: usize = 8;

pub(super) async fn provider<Q, F>(query: &Q, link: &ActionLink) -> Result<String, String>
where
    Q: Fn(CatalogueQuery) -> F + Sync,
    F: std::future::Future<Output = Result<CataloguePage, String>> + Send,
{
    if link.provider.is_empty() {
        return Err("legacy link has no original publisher; select the resource again".into());
    }
    let providers = query(CatalogueQuery {
        max_results: 0,
        ..Default::default()
    })
    .await?;
    if providers.stale_cursor || providers.budget_exhausted {
        return Err("original publisher discovery is incomplete".into());
    }
    let mut matching = providers.providers.iter().filter(|provider| {
        provider.provider == link.provider
            && provider.browsable
            && provider.room == link.route.room
            && provider.app_id == link.route.app_id
            && provider.record_id == link.route.record_id
    });
    let current = matching
        .next()
        .ok_or("original publisher is no longer advertised; select the resource again")?;
    if matching.next().is_some() {
        return Err("original publisher metadata is ambiguous".into());
    }
    Ok(current.provider.clone())
}

fn check_row(
    link: &ActionLink,
    target: &RemoteActionTarget,
    row: &CatalogueRow,
) -> Result<(), String> {
    let descriptor = row
        .action
        .as_ref()
        .ok_or("original action descriptor is missing")?;
    if row.target().as_ref() != Some(target)
        || row.app_id != link.route.app_id
        || descriptor.semantic_identity.as_ref() != Some(&link.semantic_identity)
        || descriptor.resource_kind != link.semantic_identity.kind
        || row.resource.id != link.resource.id
        || row.resource.kind != link.resource.kind
    {
        return Err("original action semantic identity changed; bind a new experiment".into());
    }
    // Confirmation belongs to the existing platform door; never manufacture it.
    if !matches!(
        descriptor.contract.advertised_confirmation(),
        ui::tilo_actions::ConfirmationPolicy::None
    ) {
        return Err("original action requires confirmation through its own platform action".into());
    }
    Ok(())
}

async fn offered<Q, F>(
    query: &Q,
    link: &ActionLink,
    target: &RemoteActionTarget,
) -> Result<(CatalogueRow, String), String>
where
    Q: Fn(CatalogueQuery) -> F + Sync,
    F: std::future::Future<Output = Result<CataloguePage, String>> + Send,
{
    let mut cursor = Vec::new();
    let mut revision = None;
    let mut offered = None;
    for _ in 0..MAX_RESOURCE_PAGES {
        let page = query(CatalogueQuery {
            provider: Some(target.provider.clone()),
            parent_resource: Some(link.resource.id.clone()),
            cursor: cursor.clone(),
            revision: revision.clone(),
            max_results: 64,
            ..Default::default()
        })
        .await?;
        if page.stale_cursor
            || page.revision.is_empty()
            || revision
                .as_ref()
                .is_some_and(|revision| revision != &page.revision)
        {
            return Err("original resource publication changed during discovery".into());
        }
        revision = Some(page.revision);
        for row in page.rows {
            if row.target().as_ref() == Some(target) {
                check_row(link, target, &row)?;
                if offered.replace(row).is_some() {
                    return Err("original action is ambiguously advertised".into());
                }
            }
        }
        if page.next_cursor.is_empty() {
            if !page.exhausted || page.budget_exhausted {
                return Err("original resource discovery is incomplete".into());
            }
            return Ok((
                offered.ok_or("original action is no longer offered")?,
                revision.ok_or("original revision is missing")?,
            ));
        }
        if page.next_cursor == cursor {
            return Err("original resource cursor did not advance".into());
        }
        cursor = page.next_cursor;
    }
    Err("original resource discovery exceeds bounded pages".into())
}

pub(super) async fn resolve<Q, F>(
    query: &Q,
    link: &ActionLink,
) -> Result<(RemoteActionTarget, String), String>
where
    Q: Fn(CatalogueQuery) -> F + Sync,
    F: std::future::Future<Output = Result<CataloguePage, String>> + Send,
{
    let target = RemoteActionTarget {
        provider: provider(query, link).await?,
        address: link.address.clone(),
    };
    let (offered, revision) = offered(query, link, &target).await?;
    let resolved = query(CatalogueQuery::resolve(&target, revision.clone())).await?;
    if resolved.stale_cursor
        || resolved.budget_exhausted
        || resolved.rows.len() != 1
        || resolved.rows.first() != Some(&offered)
    {
        return Err("original action changed after its offered revision".into());
    }
    provider(query, link).await?;
    // Resolution is Found(row)/StaleRevision, not a new page revision echo.
    Ok((target, revision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Future, ready};
    use std::task::{Context, Poll, Waker};
    use ui::tilo_actions::remote::{CatalogueProvider, CatalogueResource, CatalogueRow};
    use ui::tilo_actions::{
        ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
    };

    fn link() -> ActionLink {
        serde_json::from_value(serde_json::json!({
            "provider":"native-provider",
            "route":{"room":"test-room","app_id":"so.tilo.code-editor","record_id":"original-record"},
            "address":{"resource":"opaque-original-file","action":"read-text-snapshot"},
            "resource":{"id":"opaque-original-file","kind":"code_editor.file"},
            "semantic_identity":{"kind":"code_editor.file","key":"stable-file-key"}
        })).unwrap()
    }
    fn row(link: &ActionLink) -> CatalogueRow {
        CatalogueRow {
            provider: link.provider.clone(),
            app_id: link.route.app_id.clone(),
            parent_resource: link.resource.id.clone(),
            resource: CatalogueResource {
                id: link.resource.id.clone(),
                kind: link.resource.kind.clone(),
                label: "Original file".into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                address: link.address.clone(),
                semantic_identity: Some(link.semantic_identity.clone()),
                resource_kind: link.resource.kind.clone(),
                resource_label: "Original file".into(),
                action_label: "Read snapshot".into(),
                description: None,
                locus: ExecutionLocus::default(),
                kind: OperationKind::Effect,
                contract: ActionContract::default(),
                input: InputSpec::None,
                params: Vec::new(),
                view: None,
            }),
            has_children: false,
            visibility: None,
        }
    }
    fn lookup(
        allow_canonical: bool,
        failure: &str,
    ) -> Result<(RemoteActionTarget, String), String> {
        let mut link = link();
        if failure == "legacy" {
            link.provider.clear();
        }
        let offered = row(&link);
        let resolved = std::sync::atomic::AtomicBool::new(false);
        // Stub only the catalogue protocol; no custody or invocation authority is minted.
        let query = |query: CatalogueQuery| {
            let result = if query.canonical_route.is_some() && !allow_canonical {
                Err("ordinary native publisher has no private canonical route".into())
            } else if query.provider.is_none() {
                let mut provider = CatalogueProvider {
                    provider: link.provider.clone(),
                    app_id: link.route.app_id.clone(),
                    room: link.route.room.clone(),
                    record_id: link.route.record_id.clone(),
                    browsable: true,
                };
                match failure {
                    "foreign-room" => provider.room = "another-room".into(),
                    "foreign-record" => provider.record_id = "another-record".into(),
                    "replacement-peer" => provider.provider = "replacement-peer".into(),
                    "not-browsable" => provider.browsable = false,
                    "retired-after-resolution"
                        if resolved.load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        provider.record_id = "changed-record".into();
                    }
                    _ => {}
                }
                Ok(CataloguePage {
                    providers: vec![provider],
                    ..Default::default()
                })
            } else if query.target.is_some() {
                if query.revision.as_deref() == Some("chosen-r1") {
                    resolved.store(true, std::sync::atomic::Ordering::Relaxed);
                    let mut row = offered.clone();
                    if failure == "changed-row" {
                        row.resource.id = "another-public-alias".into();
                    }
                    // Found(row) intentionally has no page revision echo.
                    Ok(CataloguePage {
                        stale_cursor: failure == "stale",
                        rows: vec![row],
                        ..Default::default()
                    })
                } else {
                    Err("resolving a target needs the revision it was chosen at".into())
                }
            } else if query.parent_resource.as_deref() == Some(link.resource.id.as_str()) {
                if failure == "pagination" && query.cursor.is_empty() {
                    Ok(CataloguePage {
                        revision: "chosen-r1".into(),
                        next_cursor: vec![1],
                        budget_exhausted: true,
                        ..Default::default()
                    })
                } else if failure == "page-bound" {
                    let next = query
                        .cursor
                        .first()
                        .copied()
                        .unwrap_or_default()
                        .saturating_add(1);
                    Ok(CataloguePage {
                        revision: "chosen-r1".into(),
                        next_cursor: vec![next],
                        budget_exhausted: true,
                        ..Default::default()
                    })
                } else if !query.cursor.is_empty() && query.revision.as_deref() != Some("chosen-r1")
                {
                    Err("cursor needs its original revision".into())
                } else {
                    Ok(CataloguePage {
                        revision: "chosen-r1".into(),
                        rows: vec![offered.clone()],
                        exhausted: true,
                        ..Default::default()
                    })
                }
            } else {
                Err("browse the actual offered resource address".into())
            };
            ready(result)
        };
        ready_answer(resolve(&query, &link))
    }

    fn ready_answer<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(answer) => answer,
            Poll::Pending => panic!("ready protocol fixture must finish"),
        }
    }

    #[test]
    fn native_lookup_browses_original_resource_and_preserves_chosen_revision_without_echo() {
        let (target, revision) =
            lookup(false, "").expect("ordinary native discovery must resolve its offered row");
        assert_eq!(target, row(&link()).target().unwrap());
        assert_eq!(revision, "chosen-r1");
    }

    #[test]
    fn target_resolution_requires_the_original_chosen_revision_even_for_private_routes() {
        let (target, revision) =
            lookup(true, "").expect("resolution must carry a revision obtained from browsing");
        assert_eq!(target, row(&link()).target().unwrap());
        assert_eq!(revision, "chosen-r1");
    }
    #[test]
    fn discovery_refuses_changed_metadata_and_never_substitutes_a_publisher() {
        for failure in [
            "legacy",
            "foreign-room",
            "foreign-record",
            "replacement-peer",
            "not-browsable",
            "retired-after-resolution",
            "changed-row",
            "stale",
            "page-bound",
        ] {
            assert!(
                lookup(false, failure).is_err(),
                "must refuse {failure} rather than infer a new action or publisher"
            );
        }
    }

    #[test]
    fn bounded_resource_pagination_keeps_the_offered_cursor_revision() {
        let (target, revision) = lookup(false, "pagination")
            .expect("bounded partial pages may continue using the offered cursor");
        assert_eq!(target, row(&link()).target().unwrap());
        assert_eq!(revision, "chosen-r1");
    }
}
