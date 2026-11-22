//! A record's listing paged where the record is held.
//!
//! A catalogue page of a record the service holds is answered by the record's
//! app worker: it lists what the record publishes to the caller and hands
//! back only the rows on the page ([`crate::worker::ConfinedReplica::survey_page`]). A chat
//! publishes every message with its actions; handing all of them over for
//! every page made each page cost the whole conversation, and an agent
//! naming one resource by path reads several pages.

use super::{
    CatalogueQuery, CatalogueRow, DocumentRoute, Worker, WorkspaceState, bounded, direct, named,
    resource_provider,
};
use tilo_actions::remote::{Caller, CataloguePage};
use tilo_actions::remote::{rows_weight, truncate_resources, truncate_rows};
use tilo_server_protocol::WorkspaceId;

/// The fewest rows asked of a resource at a time where some of its rows are kept from the
/// caller ([`Worker::shown_page`]).
const SHOWN_STRETCH: usize = 64;

impl Worker {
    /// The page `query` asks of the records at `routes`, and after them the
    /// workspace's `own` rows: what each record's held replica publishes to
    /// the caller, paged where it is held ([`crate::worker::ConfinedReplica::survey_page`]),
    /// then the operations its committed source reads it by. Only the rows
    /// on the page are handed over, so a page costs its rows and not every
    /// item a record holds. A record that does not mount lists nothing.
    pub(super) fn documents_page(
        &mut self,
        workspace_id: WorkspaceId,
        (caller, bounded): (&Caller, Option<&bounded::Bounded>),
        (routes, own): (&[DocumentRoute], Vec<CatalogueRow>),
        state: &WorkspaceState,
        query: &CatalogueQuery,
        revision: String,
    ) -> CataloguePage {
        // Asked only who publishes (an agent's path resolves that before each
        // call): the caller names the providers, and no resource is mounted or
        // laid out for rows nobody asked for.
        if query.max_results == 0 && query.provider.is_none() {
            return CataloguePage::window(revision, Vec::new(), query, false);
        }
        let size = query.page_size();
        // One more than the page: whether more follow.
        let wanted = size.saturating_add(1);
        // The admitted rows still to pass over before the page starts.
        let mut skip = query.offset();
        let mut rows: Vec<CatalogueRow> = Vec::new();
        // Whose conversations decide what is listed of them ([`direct`]).
        let person = bounded.map_or_else(|| self.member.clone(), |who| who.person.clone());
        let take = |rows: &mut Vec<CatalogueRow>, skip: &mut usize, more: Vec<CatalogueRow>| {
            let admitted: Vec<CatalogueRow> =
                more.into_iter().filter(|row| query.admits(row)).collect();
            let passed = admitted.len().min(*skip);
            *skip = skip.saturating_sub(admitted.len());
            let left = wanted.saturating_sub(rows_weight(rows));
            rows.extend(admitted.into_iter().skip(passed).take(left));
        };
        let mut failed: Option<(DocumentRoute, String)> = None;
        // A page bounded in resources is full once it holds one past them.
        let resources_full = |rows: &[CatalogueRow]| {
            query
                .resource_bound()
                .is_some_and(|most| resource_count(rows) > most)
        };
        for route in routes {
            // A document's worker reads its rows off this thread: the page waits for it there
            // and is assembled when this request is applied again.
            if self.call_waits.held() || rows_weight(&rows) >= wanted || resources_full(&rows) {
                break;
            }
            let provider = resource_provider(route);
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
            let window = (skip, wanted.saturating_sub(rows_weight(&rows)));
            let withheld = self.kept_from(&person, route, state);
            let whose = (bounded, &withheld);
            match self.shown_page(workspace_id, whose, route, state, query, window) {
                Ok((held, listed)) => {
                    self.unmounted.remove(route);
                    skip = skip.saturating_sub(held);
                    rows.extend(listed.into_iter().map(kept));
                }
                Err(error) => {
                    if self.call_waits.held() {
                        break;
                    }
                    self.did_not_mount(route, error.clone());
                    failed.get_or_insert_with(|| (route.clone(), error));
                }
            }
            // The record's registered reads, over its retained source: kept
            // while the record stands where it was ([`super::canonical`]).
            let heads = self
                .mounted
                .get(&self.holder_of(route))
                .and_then(|mounted| mounted.merged.clone());
            if let Some(link) = self.shared.link.get().cloned()
                && let Ok(operations) = {
                    let mut hand = super::call_wait::Handing::of(
                        &mut self.call_waits,
                        &mut self.part_waits,
                        &self.shared,
                    );
                    self.canonical.operations(
                        &self.catalogue,
                        &link,
                        (workspace_id, caller),
                        route,
                        heads.as_deref(),
                        &mut hand,
                    )
                }
            {
                take(
                    &mut rows,
                    &mut skip,
                    operations.into_iter().map(&row).collect(),
                );
            }
        }
        take(&mut rows, &mut skip, own);
        // An app the query named and this device could not list says why: a
        // page of no rows would tell its caller the app holds nothing, and an
        // agent would go looking for another name for what is there.
        if rows.is_empty()
            && query.provider.is_some()
            && let Some((route, error)) = failed
        {
            return CataloguePage::unavailable(
                revision,
                format!("{} cannot be listed: {error}", named(&route)),
            );
        }
        // Counted in the rows each stands for: a resource's actions sent as one
        // row are as many rows of the listing.
        let mut more = rows_weight(&rows) > size;
        truncate_rows(&mut rows, size);
        if let Some(most) = query.resource_bound() {
            more |= truncate_resources(&mut rows, most);
        }
        CataloguePage::window(revision, rows, query, more)
    }

    /// Log why the document at `route` did not mount for a listing: once for
    /// each cause, as an agent lists at every step of its turn.
    pub(super) fn did_not_mount(&mut self, route: &DocumentRoute, error: String) {
        if self.unmounted.get(route) == Some(&error) {
            tracing::debug!(%error, app = %route.app_id, document = %route.document, "workspace actions: a document still does not mount");
            return;
        }
        tracing::warn!(
            %error,
            app = %route.app_id,
            document = %route.document,
            "workspace actions: {} did not mount",
            super::named(route)
        );
        self.unmounted.insert(route.clone(), error);
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
        // An app this device has only a package of is listed from the surface
        // its package carries, paged the way a mounted resource's own listing is.
        if self.catalogue.get(&route.app_id).is_none() {
            // A record whose app this host runs from its own package is listed
            // from what it publishes, read through that package.
            if let Some(rows) = self.package_page(workspace_id, bounded, route, query, window) {
                return rows;
            }
            let rows = self
                .packaged_descriptors(workspace_id, bounded, route)?
                .into_iter()
                .map(|descriptor| {
                    CatalogueRow::for_action(String::new(), route.app_id.clone(), descriptor)
                })
                .filter(|row| query.admits(row));
            let (skip, take) = window;
            let mut total = 0_usize;
            let mut page = Vec::new();
            for row in rows {
                if total >= skip && page.len() < take {
                    page.push(row);
                }
                total = total.saturating_add(1);
            }
            return Ok((total, page));
        }
        if let Some(who) = bounded {
            self.bounded_page(workspace_id, who, (route, state), query, window)
                .map_err(|error| error.to_string())
        } else {
            self.mount(route, state)?;
            let holder = self.holder_of(route);
            match self.listed_from(
                (
                    workspace_id,
                    &super::Writer::Mounted(holder.clone()),
                    &holder,
                ),
                state,
                query,
                window,
            ) {
                super::part_load::Listed::Page(total, rows) => Ok((total, rows)),
                super::part_load::Listed::Held => Err(super::part_wait::not_ready().to_string()),
                super::part_load::Listed::Answering => {
                    Err("the app's worker is still answering".to_owned())
                }
            }
        }
    }

    /// [`Self::listed_page`] without the rows `withheld` keeps from the caller: what is said in
    /// a direct conversation their person takes no part in ([`direct`]). How many rows they are
    /// listed up to the window's end, and those in the window. The window is counted in the
    /// rows the caller is given, so a row left out leaves no gap in a page: the next one takes
    /// its place.
    fn shown_page(
        &mut self,
        workspace_id: WorkspaceId,
        (bounded, withheld): (Option<&bounded::Bounded>, &direct::Kept),
        route: &DocumentRoute,
        state: &WorkspaceState,
        query: &CatalogueQuery,
        (skip, take): (usize, usize),
    ) -> Result<(usize, Vec<tilo_actions::remote::CatalogueRow>), String> {
        if withheld.is_empty() {
            return self.listed_page(workspace_id, bounded, route, state, query, (skip, take));
        }
        // What is withheld is decided a row at a time: the rows are asked for one by one, and
        // sent a resource at a time once decided.
        if query.by_resource {
            let each = CatalogueQuery {
                by_resource: false,
                ..query.clone()
            };
            let (shown, page) = self.shown_page(
                workspace_id,
                (bounded, withheld),
                route,
                state,
                &each,
                (skip, take),
            )?;
            return Ok((shown, tilo_actions::remote::compact(page)));
        }
        // Asked a stretch at a time from the first row on, until the window is full or the
        // listing ends: a stretch that comes back short was the last.
        let stretch = take.max(SHOWN_STRETCH);
        let mut asked = 0_usize;
        let mut shown = 0_usize;
        let mut page = Vec::new();
        loop {
            let from = (asked, stretch);
            let (_, listed) = self.listed_page(workspace_id, bounded, route, state, query, from)?;
            let last = listed.len() < stretch;
            asked = asked.saturating_add(listed.len());
            for row in listed {
                if withheld.keeps(&row) {
                    continue;
                }
                if shown >= skip && page.len() < take {
                    page.push(row);
                }
                shown = shown.saturating_add(1);
            }
            if last || page.len() >= take {
                return Ok((shown, page));
            }
        }
    }
}

/// How many resources `rows` hold rows of, each resource's rows next to each
/// other.
fn resource_count(rows: &[CatalogueRow]) -> usize {
    rows.iter()
        .enumerate()
        .filter(|(at, row)| {
            at.checked_sub(1)
                .and_then(|before| rows.get(before))
                .is_none_or(|before| before.resource.id != row.resource.id)
        })
        .count()
}
