//! What a workspace's host lists: its records' actions, the app rows it offers
//! to open, and where each opens.

use crate::record_actions::{open_row, open_row_needing};
use tilo_actions::remote::workspace_invocations::{self, InvocationsQuery, MOST};
use tilo_actions::remote::{CatalogueProvider, workspace_provider};

use super::{
    ActionCallError, ActionKind, Caller, CataloguePage, CatalogueQuery, CatalogueRow,
    DEFAULT_DOCUMENTS, DocumentRoute, Worker, WorkspaceState, catalogue_keys, is_part,
    provider_workspace, resource_of, resource_provider,
};
use tilo_server_protocol::WorkspaceId;

impl Worker {
    pub(super) fn catalogue_page(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
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
        let refresh_began = std::time::Instant::now();
        let state = self.refresh(workspace_id, keys.read())?;
        let refresh_ms = refresh_began.elapsed().as_millis();
        // A workspace nothing was written in yet is named by the provider
        // asked for, so its new records are offered; one this member's replica
        // holds nothing of yet — a public workspace sends its records when a
        // device opens them — is named by the session it joined.
        let asked_own = query
            .provider
            .as_deref()
            .and_then(provider_workspace)
            .is_some_and(|named| WorkspaceId::parse(named) == Some(workspace_id));
        let Some(cluster_id) = state
            .workspace
            .clone()
            .or_else(|| asked_own.then(|| workspace_id.to_string()))
            .or_else(|| {
                self.workspaces
                    .get(&workspace_id)
                    .and_then(|workspace| workspace.cluster_id(workspace_id))
            })
        else {
            return Ok(CataloguePage::from_rows(
                state.revision_of(query.provider.as_deref()),
                Vec::new(),
                query,
            ));
        };
        let routes = self.documents(Some(workspace_id), &state, &cluster_id);
        let (wanted, state) = self.wanted_routes(
            workspace_id,
            caller,
            (&cluster_id, &routes, asked_own),
            state,
            query,
        )?;
        // The revision of what this page reads: another app's writes leave it where it is, so
        // a page read on from a cursor still reads one snapshot of it while they land.
        let revision = state.revision_of(query.provider.as_deref());
        let bounded = self.bounded(caller);
        let page_began = std::time::Instant::now();
        let own_rows = self.own_rows(workspace_id, caller, (&cluster_id, asked_own), query);
        let mut page = if query.view_resource.is_some() {
            let mut rows = self.listed_rows(workspace_id, bounded.as_ref(), &wanted, &state);
            rows.extend(own_rows);
            CataloguePage::from_rows(revision, rows, query)
        } else if !query.cursor.is_empty()
            && query
                .revision
                .as_deref()
                .is_none_or(|named| state.stale(Some(named)))
        {
            CataloguePage::stale(revision)
        } else {
            // Paged where each record is held: a page costs its rows, however
            // many items the records hold.
            let at = (caller, bounded.as_ref());
            self.documents_page(
                workspace_id,
                at,
                (&wanted, own_rows),
                &state,
                query,
                revision,
            )
        };
        if query.provider.is_none() {
            let own_provider = workspace_provider(&cluster_id);
            page.providers = routes
                .iter()
                .map(|route| CatalogueProvider {
                    provider: resource_provider(route),
                    app_id: route.app_id.clone(),
                    workspace: cluster_id.clone(),
                    document_id: route.document.clone(),
                    // A part of a chain is held with the record that keeps
                    // it, never opened as one of its own: offered as one, it
                    // was the app's newest record, which an agent's call
                    // acted on and opened a window of (one that cannot open
                    // it as the app's record), and whose link it answered.
                    browsable: !is_part(&route.document),
                })
                .chain(std::iter::once(CatalogueProvider {
                    provider: own_provider,
                    app_id: String::new(),
                    workspace: cluster_id.clone(),
                    document_id: String::new(),
                    browsable: false,
                }))
                .collect();
            page.providers
                .push(workspace_invocations::publisher(&cluster_id));
        }
        tracing::debug!(
            records = routes.len(),
            rows = page.rows.len(),
            provider = query.provider.is_some(),
            refresh_ms = refresh_ms,
            page_ms = page_began.elapsed().as_millis(),
            "workspace actions: a catalogue page was assembled"
        );
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
        // Whose conversations decide what is listed of them ([`super::direct`]).
        let person = bounded.map_or_else(|| self.member.clone(), |who| who.person.clone());
        for route in wanted {
            let provider = resource_provider(route);
            let withheld = self.kept_from(&person, route, state);
            match self.listed(workspace_id, bounded, route, state) {
                Ok(descriptors) => {
                    self.unmounted.remove(route);
                    rows.extend(
                        descriptors
                            .into_iter()
                            .filter(|descriptor| !withheld.names(&descriptor.address.resource))
                            .map(|descriptor| {
                                CatalogueRow::for_action(
                                    provider.clone(),
                                    route.app_id.clone(),
                                    descriptor,
                                )
                            }),
                    );
                }
                Err(error) => self.did_not_mount(route, error),
            }
        }
        rows
    }

    /// The records a page of `query` lists in `cluster_id`, with the state they are listed in:
    /// the one its provider names, every app's rows, or the first of the workspace's records. A
    /// document an app keeps for its whole workspace is listed through the record the app keeps,
    /// which publishes it beside itself, founded as `caller` when the workspace holds none of the
    /// app's record: the first caller naming one of the app's documents reaches what it declares.
    fn wanted_routes(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (cluster_id, routes, asked_own): (&str, &[DocumentRoute], bool),
        state: WorkspaceState,
        query: &CatalogueQuery,
    ) -> Result<(Vec<DocumentRoute>, WorkspaceState), ActionCallError> {
        let wanted = match query.provider.as_deref() {
            Some(_) if asked_own => Vec::new(),
            Some(provider) => {
                let route =
                    resource_of(provider, cluster_id).ok_or(ActionCallError::UnknownAction)?;
                if collab_types::workspace_document(&route.app_id, &route.document).is_some() {
                    let (holder, state) = self
                        .holder_of_workspace_document(workspace_id, caller, &route, state)
                        .ok_or(ActionCallError::UnknownAction)?;
                    return Ok((vec![holder], state));
                }
                if !routes.contains(&route) {
                    return Err(ActionCallError::UnknownAction);
                }
                vec![route]
            }
            None => routes.iter().take(DEFAULT_DOCUMENTS).cloned().collect(),
        };
        Ok((wanted, state))
    }

    /// The workspace's own rows `query` asks for, as `caller` is shown them: its apps, to
    /// open, and its folder with the files in it by their paths ([`super::files_listing`]).
    fn own_rows(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (cluster_id, asked_own): (&str, bool),
        query: &CatalogueQuery,
    ) -> Vec<CatalogueRow> {
        if query.provider.is_some() && !asked_own {
            return Vec::new();
        }
        let mut rows = if self.may_across(workspace_id, caller, ActionKind::Undoable) {
            self.open_rows(cluster_id)
        } else {
            Vec::new()
        };
        rows.extend(self.file_rows(workspace_id, caller, cluster_id, query));
        rows
    }

    fn open_rows(&self, workspace: &str) -> Vec<CatalogueRow> {
        self.catalogue
            .iter()
            .map(|definition| open_row(workspace, definition.app_id(), definition.title()))
            .chain(self.catalogue.iter_placed().map(|placed| {
                open_row_needing(workspace, placed.app_id, placed.title, placed.needs)
            }))
            .collect()
    }
}

impl Worker {
    /// A page of the workspace's invocations as a published tree
    /// ([`workspace_invocations::tree_rows`]): the invocations newest first, read as far as
    /// the page reaches. Its revision is how many there are, so a page read on from a cursor
    /// reads the listing it was cut from, and one that grew since is read again from its start.
    pub(super) fn invocations_page(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        let wanted = query
            .offset()
            .saturating_add(query.page_size())
            .saturating_add(1);
        let mut listed = Vec::new();
        let mut cursor = None;
        let mut total;
        loop {
            let page = self.invocations(
                workspace_id,
                caller,
                &InvocationsQuery {
                    limit: MOST,
                    cursor: cursor.take(),
                    ..InvocationsQuery::default()
                },
            )?;
            total = page.total;
            listed.extend(page.items);
            match page.next_cursor {
                Some(next) if listed.len() < wanted => cursor = Some(next),
                _ => break,
            }
        }
        let rows = workspace_invocations::tree_rows(&listed);
        Ok(CataloguePage::from_rows(total.to_string(), rows, query))
    }
}
