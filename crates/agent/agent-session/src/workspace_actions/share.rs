//! `share-link` on a record this host runs headless: the link minted from
//! the resource's views as the caller may read them, under the web entry
//! the host announces.

use tilo_actions::remote::{ActionHostError, Caller, CatalogueQuery};
use tilo_actions::{ActionFailure, ActionOutput};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::WorkspaceId;

use super::{Outcome, Worker, WorkspaceState, record_provider};

impl Worker {
    /// `share-link` on a record this host runs headless. An app's root mints
    /// it where the app runs; here no root runs, so the host mints the same
    /// link from the resource's views as `caller` (a person: an agent asks as
    /// the person it acts for) may read them now (the governed lookup a link's
    /// page makes too), under the web entry the host announces.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn share_link(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (route, state, kind): (&DocumentRoute, &WorkspaceState, String),
        target: &tilo_actions::remote::RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, Outcome> {
        let asker = self.link_asker(workspace_id, caller, (route, state), target)?;
        let caller = &asker;
        let failed = |message: String| {
            Outcome::Failed(ActionFailure {
                code: "share_link".into(),
                message,
                details: None,
                retryable: false,
            })
        };
        let resource = &target.address.resource;
        // The resource's views are attached where the app renders, which the
        // lookup below runs. A resource with none is a record's root: its
        // link names the record.
        // The record itself, opened at its primary view.
        let opened = self.holder_of(route);
        let root = self
            .catalogue
            .get(&route.app_id)
            .is_some_and(|definition| definition.record_kind.as_str() == resource.as_str());
        let plain = ["view", "start", "end"]
            .iter()
            .all(|name| arguments.get(*name).is_none_or(serde_json::Value::is_null));
        // The root of a part of a chain is the record it is held beside,
        // whose link names that record: no view of it is looked up.
        if root && plain {
            let request = ui::tilo_resource::share::ShareRequest {
                resource: resource.clone(),
                kind,
                views: Vec::new(),
                invite: false,
                args: arguments,
            };
            return self.record_link(workspace_id, caller, &opened, &request);
        }
        let wanted = tilo_actions::remote::ViewResourceQuery {
            id: resource.clone(),
            kind: kind.clone(),
        };
        let found = if let Some(found) = self.mounted_views(workspace_id, caller, route, &wanted) {
            found
        } else {
            let query = CatalogueQuery {
                provider: Some(record_provider(route)),
                view_resource: Some(wanted),
                ..CatalogueQuery::default()
            };
            self.view_catalogue_page(workspace_id, caller, &query)
                .map(|page| {
                    let descriptors = page.rows.into_iter().filter_map(|row| row.action).collect();
                    (descriptors, page.locator)
                })
        };
        let (descriptors, locator) = found.map_err(Outcome::Rejected)?;
        let views = descriptors
            .iter()
            .filter(|descriptor| descriptor.address.resource == *resource)
            .filter_map(|descriptor| {
                descriptor
                    .view
                    .as_ref()
                    .filter(|view| view.open_action == descriptor.address.action)
            })
            .map(|view| ui::tilo_resource::share::SharedView {
                id: view.id.clone(),
                primary: view.primary,
                range: view.range.is_some(),
                desktop: view.desktop,
            })
            .collect();
        let request = ui::tilo_resource::share::ShareRequest {
            resource: resource.clone(),
            kind,
            views,
            invite: false,
            args: arguments,
        };
        let mut locator =
            locator.ok_or_else(|| failed("This workspace's host serves no web page".into()))?;
        // A part of a chain opens with the record it is held beside: the
        // link's page mounts that record, whose window holds the part and
        // offers its items at the address asked for here.
        locator.provider = record_provider(&opened);
        let url = ui::framework::host_link(&request, &opened.app_id, &opened.record, &locator)
            .map_err(failed)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({ "url": url })),
            resources: Vec::new(),
        })
    }
}

impl Worker {
    /// The views `wanted` offers `caller`, and where the record opens, from
    /// the record this host holds mounted, brought up to date for the action
    /// asking: no copy of the record is read and decoded afresh, which costs
    /// more the more it holds. `None` when the record is not held here as
    /// itself (a part of a chain, opened with the record it is beside).
    pub(super) fn mounted_views(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        wanted: &tilo_actions::remote::ViewResourceQuery,
    ) -> Option<Result<FoundViews, ActionHostError>> {
        if self.holder_of(route) != *route {
            return None;
        }
        let mounted = self.mounted.get(route)?;
        let refuse = || ActionHostError::UnknownAction;
        let link = self.shared.link.get()?;
        let Some(policy) = link.view_read_policy(workspace, caller, &route.app_id, &route.record)
        else {
            return Some(Err(refuse()));
        };
        let views = match mounted.app.resource_views(wanted, &policy) {
            Ok(views) => views,
            Err(error) => {
                return Some(Err(ActionHostError::Unavailable {
                    reason: error.to_string(),
                }));
            }
        };
        Some(Ok((
            views,
            link.view_locator(&policy.workspace, &record_provider(route)),
        )))
    }

    /// Who asks for a link on `caller`'s behalf: someone served within
    /// bounds asks as the person they are or act for, and only for a link
    /// their own replica offers them.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn link_asker(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (route, state): (&DocumentRoute, &WorkspaceState),
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<Caller, Outcome> {
        let Some(who) = self.bounded(caller) else {
            return Ok(caller.clone());
        };
        let offered = self
            .bounded_descriptor(workspace_id, &who, (route, state), &target.address)
            .map_err(Outcome::Rejected)?;
        if offered.is_some_and(|descriptor| descriptor.address == target.address) {
            Ok(who.person_caller)
        } else {
            Err(Outcome::Rejected(ActionHostError::UnknownAction))
        }
    }

    /// The link to the record at `route` itself, opened at its primary view:
    /// none of its views is looked up, only that `caller` may read it now
    /// and where it opens.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn record_link(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        request: &ui::tilo_resource::share::ShareRequest,
    ) -> Result<ActionOutput, Outcome> {
        let failed = |message: String| {
            Outcome::Failed(ActionFailure {
                code: "share_link".into(),
                message,
                details: None,
                retryable: false,
            })
        };
        let refused = || Outcome::Rejected(ActionHostError::UnknownAction);
        let link = self.shared.link.get().ok_or_else(refused)?;
        let policy = link
            .view_read_policy(workspace_id, caller, &route.app_id, &route.record)
            .ok_or_else(refused)?;
        let locator = link
            .view_locator(&policy.workspace, &record_provider(route))
            .ok_or_else(|| failed("This workspace's host serves no web page".into()))?;
        let url = ui::framework::host_link(request, &route.app_id, &route.record, &locator)
            .map_err(failed)?;
        Ok(ActionOutput {
            value: Some(serde_json::json!({ "url": url })),
            resources: Vec::new(),
        })
    }
}

/// The views a resource offers, and where its record opens.
type FoundViews = (
    Vec<tilo_actions::ActionDescriptor>,
    Option<tilo_actions::remote::ResourceViewLocator>,
);
