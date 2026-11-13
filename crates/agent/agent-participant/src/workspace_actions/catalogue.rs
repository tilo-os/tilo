//! What a workspace's host lists: its records' actions, the app rows it offers
//! to open, and where each opens.

use crate::participant::actions::open_row;
use tilo_actions::remote::{CatalogueProvider, workspace_provider};

use super::{
    ActionHostError, ActionKind, Caller, CataloguePage, CatalogueQuery, CatalogueRow,
    DEFAULT_ROWS_RECORDS, DocumentRoute, Worker, WorkspaceState, catalogue_keys,
    provider_workspace, record_of, record_provider,
};
use tilo_relay_protocol::WorkspaceId;

impl Worker {
    pub(super) fn catalogue_page(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        // A stranger is told nothing: the workspace's own records are for the
        // people it admits, and for the person whose device runs this service.
        // A view lookup is not a listing and carries its own governed check.
        if query.view_resource.is_none() && !self.admits_member(workspace_id, caller) {
            return Ok(CataloguePage::from_rows("0".into(), Vec::new(), query));
        }
        if let Some(provider) = query.provider.as_deref()
            && let Some(page) =
                self.registered_read_catalogue(workspace_id, caller, provider, query)
        {
            return Ok(page);
        }
        // A record named by its provider waits for its own keys; a listing
        // waits for the workspace's once ([`catalogue_keys`], which the
        // service's thread admitted this request by).
        let keys = catalogue_keys(query);
        let state = self.refresh(workspace_id, keys.read())?;
        let revision = state.revision.to_string();
        // A workspace nothing was written in yet is named by the provider
        // asked for, so its new records are offered; one this member's replica
        // holds nothing of yet — a public workspace sends its records when a
        // device opens them — is named by the session it joined.
        let asked = query.provider.as_deref().and_then(provider_workspace);
        let Some(cluster_id) = state
            .workspace
            .clone()
            .or_else(|| {
                asked
                    .filter(|named| WorkspaceId::parse(named) == Some(workspace_id))
                    .map(str::to_owned)
            })
            .or_else(|| {
                self.workspaces
                    .get(&workspace_id)
                    .and_then(|workspace| workspace.cluster_id(workspace_id))
            })
        else {
            return Ok(CataloguePage::from_rows(revision, Vec::new(), query));
        };
        let routes = self.records(&state, &cluster_id);
        let own_provider = workspace_provider(&cluster_id);
        let wanted: Vec<DocumentRoute> = match query.provider.as_deref() {
            Some(provider) if provider == own_provider => Vec::new(),
            Some(provider) => {
                let route = record_of(provider, &cluster_id)
                    .filter(|route| routes.contains(route))
                    .ok_or(ActionHostError::UnknownAction)?;
                vec![route]
            }
            None => routes.iter().take(DEFAULT_ROWS_RECORDS).cloned().collect(),
        };
        let bounded = self.bounded(caller);
        // The workspace's own rows: its apps, to open.
        let own_rows = if query
            .provider
            .as_deref()
            .is_none_or(|provider| provider == own_provider)
            && self.may_across(workspace_id, caller, ActionKind::Undoable)
        {
            self.open_rows(&cluster_id)
        } else {
            Vec::new()
        };
        let mut page = if query.view_resource.is_some() {
            let mut rows = self.listed_rows(workspace_id, bounded.as_ref(), &wanted, &state);
            rows.extend(own_rows);
            CataloguePage::from_rows(revision, rows, query)
        } else if !query.cursor.is_empty() && query.revision.as_deref() != Some(revision.as_str()) {
            CataloguePage::stale(revision)
        } else {
            // Paged where each record is held: a page costs its rows, however
            // many items the records hold.
            let at = (caller, bounded.as_ref());
            self.records_page(
                workspace_id,
                at,
                (&wanted, own_rows),
                &state,
                query,
                revision,
            )
        };
        if query.provider.is_none() {
            page.providers = routes
                .iter()
                .map(|route| CatalogueProvider {
                    provider: record_provider(route),
                    app_id: route.app_id.clone(),
                    workspace: cluster_id.clone(),
                    record_id: route.record.clone(),
                    browsable: true,
                })
                .chain(std::iter::once(CatalogueProvider {
                    provider: own_provider,
                    app_id: String::new(),
                    workspace: cluster_id.clone(),
                    record_id: String::new(),
                    browsable: false,
                }))
                .collect();
        }
        Ok(page)
    }

    /// The rows a listing shows for `wanted`: what each record offers, and,
    /// where one does not mount, why it did not, named so the line says which
    /// record and not only that one failed.
    fn listed_rows(
        &mut self,
        workspace_id: WorkspaceId,
        bounded: Option<&super::bounded::Bounded>,
        wanted: &[DocumentRoute],
        state: &WorkspaceState,
    ) -> Vec<CatalogueRow> {
        let mut rows = Vec::new();
        for route in wanted {
            let provider = record_provider(route);
            match self.listed(workspace_id, bounded, route, state) {
                Ok(descriptors) => rows.extend(descriptors.into_iter().map(|descriptor| {
                    CatalogueRow::for_action(provider.clone(), route.app_id.clone(), descriptor)
                })),
                Err(error) => tracing::warn!(
                    %error,
                    app = %route.app_id,
                    record = %route.record,
                    "workspace actions: {} did not mount",
                    super::named(route)
                ),
            }
        }
        rows
    }

    fn open_rows(&self, workspace: &str) -> Vec<CatalogueRow> {
        self.catalogue
            .iter()
            .map(|definition| open_row(workspace, definition.app_id(), definition.title()))
            .chain(
                self.catalogue
                    .iter_placed()
                    .map(|placed| open_row(workspace, placed.app_id, placed.title)),
            )
            .collect()
    }
}
