//! A record's listing paged where the record is held.
//!
//! A catalogue page of a record the service holds is answered by the record's
//! app worker: it lists what the record publishes to the caller and hands
//! back only the rows on the page ([`crate::worker::ConfinedReplica::survey_page`]). A chat
//! publishes every message with its actions; handing all of them over for
//! every page made each page cost the whole conversation, and an agent
//! naming one resource by path reads several pages.

use super::{
    CatalogueQuery, CatalogueRow, DocumentRoute, Worker, WorkspaceState, bounded, reads,
    record_provider,
};
use tilo_actions::remote::{Caller, CataloguePage};
use tilo_relay_protocol::WorkspaceId;

impl Worker {
    /// The page `query` asks of the records at `routes`, and after them the
    /// workspace's `own` rows: what each record's held replica publishes to
    /// the caller, paged where it is held ([`crate::worker::ConfinedReplica::survey_page`]),
    /// then the operations its committed source reads it by. Only the rows
    /// on the page are handed over, so a page costs its rows and not every
    /// item a record holds. A record that does not mount lists nothing.
    pub(super) fn records_page(
        &mut self,
        workspace_id: WorkspaceId,
        (caller, bounded): (&Caller, Option<&bounded::Bounded>),
        (routes, own): (&[DocumentRoute], Vec<CatalogueRow>),
        state: &WorkspaceState,
        query: &CatalogueQuery,
        revision: String,
    ) -> CataloguePage {
        let size = query.page_size();
        // One more than the page: whether more follow.
        let wanted = size.saturating_add(1);
        // The admitted rows still to pass over before the page starts.
        let mut skip = query.offset();
        let mut rows: Vec<CatalogueRow> = Vec::new();
        let take = |rows: &mut Vec<CatalogueRow>, skip: &mut usize, more: Vec<CatalogueRow>| {
            let admitted: Vec<CatalogueRow> =
                more.into_iter().filter(|row| query.admits(row)).collect();
            let passed = admitted.len().min(*skip);
            *skip = skip.saturating_sub(admitted.len());
            let room = wanted.saturating_sub(rows.len());
            rows.extend(admitted.into_iter().skip(passed).take(room));
        };
        for route in routes {
            if rows.len() >= wanted {
                break;
            }
            let provider = record_provider(route);
            let row = |descriptor| {
                CatalogueRow::for_action(provider.clone(), route.app_id.clone(), descriptor)
            };
            // The worker's rows keep the resource each stands under, so a
            // query naming one resource of the record lists its children.
            let kept = |mut held: CatalogueRow| {
                held.provider.clone_from(&provider);
                held.app_id.clone_from(&route.app_id);
                held
            };
            let window = (skip, wanted.saturating_sub(rows.len()));
            match self.listed_page(workspace_id, bounded, route, state, query, window) {
                Ok((held, listed)) => {
                    skip = skip.saturating_sub(held);
                    rows.extend(listed.into_iter().map(kept));
                }
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "workspace actions: a record did not mount");
                }
            }
            if let Some(link) = self.shared.link.get().cloned()
                && let Ok(mut read) = reads::CanonicalRead::prepare(
                    &self.catalogue,
                    link.as_ref(),
                    workspace_id,
                    caller,
                    route,
                )
                && read
                    .recheck(link.as_ref(), workspace_id, caller, route)
                    .is_ok()
            {
                take(
                    &mut rows,
                    &mut skip,
                    read.operations().into_iter().map(&row).collect(),
                );
            }
        }
        take(&mut rows, &mut skip, own);
        let more = rows.len() > size;
        rows.truncate(size);
        CataloguePage::window(revision, rows, query, more)
    }

    /// [`Self::listed`], one page of it as `query` lists it: how many
    /// operations it admits in all, and those in `window` (an offset and
    /// how many), each row where it stands.
    fn listed_page(
        &mut self,
        workspace_id: WorkspaceId,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
        state: &WorkspaceState,
        query: &CatalogueQuery,
        window: (usize, usize),
    ) -> Result<(usize, Vec<tilo_actions::remote::CatalogueRow>), String> {
        match bounded {
            Some(who) => self
                .bounded_page(workspace_id, who, (route, state), query, window)
                .map_err(|error| error.to_string()),
            None => self
                .mount(route, state)
                .map(|mounted| mounted.app.survey_page(query, window)),
        }
    }
}
