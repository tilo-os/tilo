//! What a workspace holds, found by words, and what changed in it: this service's answers to a
//! search ([`tilo_actions::remote::ResourceSearch`]) and to a watch of the whole workspace
//! ([`tilo_actions::remote::ChangeFeed`]), asked of it across the wire or by a turn the same
//! device carries out.
//!
//! The search ranks what each record holds as its app tells search about it: the entries the app
//! publishes to the search index ([`ui::framework::resource_index::hosted_entries`]), one per
//! resource of the record (a note by its title and its paragraphs, a file by its name), read in a
//! confined app worker once per change of the record. A record whose app tells search nothing is
//! ranked whole, by its text read app-agnostically from the document ([`crate::search`]). A
//! record this device holds no keys for has no text here and is not found by it.
//!
//! A caller is answered only with what it may read, asked for the answers a find returns and
//! kept per caller until a write lands, so a workspace of a hundred documents does not pay a
//! permission read for every one to rank one query:
//!
//! - what the workspace's policy lets it read now, an agent held to the rules its turn carries
//!   ([`Worker::readable`]). A listing is not enough to go by: it also names what a caller may
//!   change and not read;
//! - of what is said in a direct conversation, only what the person asking takes part in, an
//!   agent by the person it acts for ([`placed_for`]);
//! - the files of the workspace's folder only where it may read what the Files app keeps.
//!
//! A search may be narrowed to where a thing was said and when ([`Within`]). What an app tells
//! search of a resource says both, where it has them (a message: its conversation and when it
//! was written), so only those entries are ranked, and with no words they are listed, the
//! newest first. What says neither, and the files, are outside a narrowed search.
//!
//! Each hit is then answered as the resource the caller's listing names it by, asked through the
//! same catalogue a listing reads, so a caller acts on a hit by the path it lists: the note, not
//! the notebook it is in.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{ChangePage, Found, Pending, RemoteActionMessage, Within, pending};
use ui::history_source::feed::DocumentKey;
use ui::search::index_publication::{IndexEntry, IndexWeight};

use super::bounded::Bounded;
use super::search_access::{Readable, Reader};
use super::{
    ActionCallError, ActionKind, Answer, Caller, CatalogueQuery, CatalogueRow, DocumentRoute,
    Needs, TreePath, Worker, WorkspaceId, record_provider,
};
use super::{Job, WorkspaceActions};
use crate::workspace_documents::Reading;

/// The most things one search answers with.
const MOST_FOUND: usize = 20;

/// The most rows of one app's listing read to name the resource a search found in it.
const NAMING_ROWS: u32 = 64;

/// The most containers at an app's top level read, one page each, to name a
/// resource it lists under one of them (a chain's issues).
const NAMING_CONTAINERS: usize = 3;

/// Whether `person` is answered with `entry` as far as where it was said goes: what is said in a
/// direct conversation is found by the people it is between and by nobody else, whatever they
/// may read of the app that keeps it. An agent finds what the person it acts for takes part in,
/// never what it takes part in itself for somebody else. What is placed anywhere else, or
/// nowhere, is not decided here. The rule is the one a listing and a read are answered by
/// ([`super::direct::said_to`]).
fn placed_for(person: &ui::permissions::policy::PersonId, entry: &IndexEntry) -> bool {
    let Some(place) = &entry.place else {
        return true;
    };
    super::direct::said_to(person, &place.identity)
}

/// Whether `entry` is inside what a search is narrowed to: where its app says it is, and when.
fn inside(within: &Within, entry: &IndexEntry) -> bool {
    let place = entry
        .place
        .as_ref()
        .map(|place| (place.anchor.as_str(), place.identity.as_str()));
    within.holds(place, entry.when)
}

/// Where a resource found in the document at `route` is listed, nearest first:
/// a chain's item under the chain's own resource, then under each resource its
/// id extends, then at the document's top. A part of a chain is a piece of the
/// document that keeps the chain, and what it holds is published by the chain
/// (under `chain:` and that document's id), not by the part.
fn naming_parents(route: &DocumentRoute, resource: &str) -> Vec<Option<String>> {
    super::opened::chain_of(&route.document)
        .map(|chain| ui::resource::chain_resource_id(&chain))
        .into_iter()
        .map(Some)
        .chain(
            resource
                .match_indices('/')
                .map(|(at, _)| resource.get(..at).map(str::to_owned))
                .rev()
                .chain(std::iter::once(None)),
        )
        .collect()
}

impl Worker {
    /// What a person types to name a resource as it is (`#12`, a handle) is found as written:
    /// the entry whose identity field is the whole of `needle` answers first, ahead of every
    /// entry that merely holds the word.
    fn identity_matches(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        who: Option<&Bounded>,
        needle: &str,
        docs: &[crate::search::SearchDoc],
        searched: &[Option<IndexEntry>],
        found: &mut Vec<Found>,
    ) {
        for (entry, doc) in searched.iter().zip(docs) {
            if found.len() >= MOST_FOUND {
                break;
            }
            let Some(entry) = entry else { continue };
            let identity = entry.fields.iter().any(|(text, weight)| {
                *weight == IndexWeight::Identity && text.trim().eq_ignore_ascii_case(needle.trim())
            });
            if !identity {
                continue;
            }
            let readable = self.readable_cached(workspace_id, who, &doc.route);
            if !readable
                .as_ref()
                .is_some_and(|readable| readable.reads(&doc.route, &entry.resource))
            {
                continue;
            }
            let resource = self
                .listed_resource(&doc.route, entry)
                .or_else(|| self.named_resource(workspace_id, caller, who, &doc.route, entry));
            if let Some(resource) = resource
                && !found.iter().any(|held| held.resource == resource.resource)
            {
                found.push(resource);
            }
        }
    }

    /// What `needle` finds among the documents of `workspace_id`, best first, as `caller` lists
    /// them.
    pub(super) fn search(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (needle, within): (&str, &Within),
    ) -> Result<Vec<Found>, ActionCallError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionCallError::NotPermitted);
        }
        let began = std::time::Instant::now();
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        let refresh_ms = began.elapsed().as_millis();
        let Some(cluster_id) = state.workspace.clone() else {
            return Ok(Vec::new());
        };
        // What the caller may read is asked only for the answers a find returns, and kept per
        // caller: nothing they may not read is answered, and a second find pays nothing for it.
        let who = self.bounded(caller);
        let person = who
            .as_ref()
            .map_or_else(|| self.member.clone(), |who| who.person.clone());
        let routes: Vec<DocumentRoute> = self
            .documents(Some(workspace_id), &state, &cluster_id)
            .into_iter()
            .take(crate::search::MOST_SEARCHED)
            .collect();
        let (docs, searched) =
            self.searchable(workspace_id, &routes, (who.as_ref(), &person, within));
        let entries_ms = began.elapsed().as_millis();
        let mut found: Vec<Found> = Vec::new();
        self.identity_matches(
            workspace_id,
            caller,
            who.as_ref(),
            needle,
            &docs,
            &searched,
            &mut found,
        );
        // Words rank what holds them. With no words, a search narrowed to a place or a time
        // lists what it holds there, the newest first, and one narrowed to nothing finds
        // nothing.
        let ranked: Vec<usize> = if !needle.trim().is_empty() {
            crate::search::search(&docs, needle, None, docs.len())
                .into_iter()
                .map(|hit| hit.index)
                .collect()
        } else if within.is_everything() {
            Vec::new()
        } else {
            let mut newest: Vec<usize> = (0..docs.len()).collect();
            newest.sort_by_key(|at| {
                std::cmp::Reverse(searched.get(*at).and_then(|entry| entry.as_ref()?.when))
            });
            newest
        };
        for at in ranked {
            if found.len() >= MOST_FOUND {
                break;
            }
            let (Some(doc), Some(what)) = (docs.get(at), searched.get(at)) else {
                continue;
            };
            let readable = self.readable_cached(workspace_id, who.as_ref(), &doc.route);
            if let Some(entry) = what
                && !readable
                    .as_ref()
                    .is_some_and(|readable| readable.reads(&doc.route, &entry.resource))
            {
                continue;
            }
            if what.is_none() && !matches!(readable, Some(Readable::Whole)) {
                continue;
            }
            let resource = match what {
                Some(entry) => self.listed_resource(&doc.route, entry).or_else(|| {
                    self.named_resource(workspace_id, caller, who.as_ref(), &doc.route, entry)
                }),
                None => self.leading_resource(workspace_id, caller, who.as_ref(), &doc.route),
            };
            if let Some(resource) = resource
                && !found.iter().any(|held| held.resource == resource.resource)
            {
                found.push(resource);
            }
        }
        let named_ms = began.elapsed().as_millis();
        // And the files of the workspace's folder, by their paths there, for a caller who may
        // read them.
        if within.is_everything()
            && self.reads_files_cached(workspace_id, who.as_ref(), &cluster_id)
        {
            found.extend(self.found_files(workspace_id, &cluster_id, needle, MOST_FOUND));
        }
        tracing::debug!(
            needle,
            routes = routes.len(),
            docs = docs.len(),
            found = found.len(),
            refresh_ms,
            entries_ms,
            named_ms,
            total_ms = began.elapsed().as_millis(),
            "workspace actions: a search answered"
        );
        Ok(found)
    }

    /// How much of the resource at `route` a search answers `who` with: all of it for the
    /// person whose device this service is on (no `who`), and for anyone else what the
    /// workspace lets them read now, an agent held to the rules its turn carries. `None` for a
    /// resource the workspace keeps from them.
    ///
    /// The listing is not what decides this. It also names what a caller may change and not
    /// read (an agent refused every read of an app still posts to it), and a search answers
    /// with what a resource says.
    fn readable(
        &self,
        workspace_id: WorkspaceId,
        who: Option<&Bounded>,
        route: &DocumentRoute,
    ) -> Option<Readable> {
        let Some(who) = who else {
            return Some(Readable::Whole);
        };
        let permissions = self.permissions_at(workspace_id, who, route)?;
        let app = TreePath::root().app(route.app_id.as_str());
        Some(
            if permissions.answer(&app, ActionKind::Read) == Answer::Allow {
                Readable::Whole
            } else {
                Readable::Items(Box::new(permissions))
            },
        )
    }

    /// Whether a search answers `who` with the files of the workspace `cluster`: whoever may
    /// read what its Files app keeps there, asked once and kept until a write lands.
    fn reads_files(&self, workspace_id: WorkspaceId, who: Option<&Bounded>, cluster: &str) -> bool {
        who.is_none_or(|who| {
            self.permissions_across(workspace_id, who, cluster)
                .is_some_and(|permissions| {
                    let files = TreePath::root().app(fs_protocol::tree::FILES_APP);
                    permissions.answer(&files, ActionKind::Read) == Answer::Allow
                })
        })
    }

    /// [`Self::reads_files`] from what the workspace kept while no write landed there.
    fn reads_files_cached(
        &mut self,
        workspace_id: WorkspaceId,
        who: Option<&Bounded>,
        cluster: &str,
    ) -> bool {
        let Some(who) = who else {
            return true;
        };
        let key = Reader::of(who);
        if let Some(access) = self.access(workspace_id, &key)
            && let Some(known) = access.files
        {
            return known;
        }
        let answer = self.reads_files(workspace_id, Some(who), cluster);
        self.shared
            .search
            .permissions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(access) = self.access(workspace_id, &key) {
            access.files = Some(answer);
        }
        answer
    }

    /// [`Self::readable`] from what the workspace kept for the caller while no write landed
    /// there, else read now and kept: a second find over the same workspace pays nothing for it.
    fn readable_cached(
        &mut self,
        workspace_id: WorkspaceId,
        who: Option<&Bounded>,
        route: &DocumentRoute,
    ) -> Option<Readable> {
        let Some(who) = who else {
            return Some(Readable::Whole);
        };
        let key = Reader::of(who);
        if let Some(access) = self.access(workspace_id, &key)
            && let Some(known) = access.routes.get(route)
        {
            return known.clone();
        }
        let answer = self.readable(workspace_id, Some(who), route);
        self.shared
            .search
            .permissions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(access) = self.access(workspace_id, &key) {
            access.routes.insert(route.clone(), answer.clone());
        }
        answer
    }

    /// What the workspace kept of `key`'s access, fresh for now.
    fn access(
        &mut self,
        workspace_id: WorkspaceId,
        key: &Reader,
    ) -> Option<&mut super::search_access::SearchAccess> {
        let workspace = self.workspaces.get_mut(&workspace_id)?;
        let access = workspace.readers.entry(key.clone()).or_default();
        access.fresh();
        Some(access)
    }

    /// What a search ranks for `person` of the documents at `routes` the replica holds: every
    /// resource their apps tell search about, with what it says, and each document whose app
    /// tells it nothing, whole. Beside each document, the entry it was made of, or `None` for
    /// one ranked whole.
    ///
    /// What the caller may read is not decided here: an entry the caller may not read is ranked
    /// and dropped once it is one of the answers ([`Self::readable_cached`]), so a workspace of
    /// a hundred documents does not pay a hundred permission reads to rank one query.
    fn searchable(
        &mut self,
        workspace_id: WorkspaceId,
        routes: &[DocumentRoute],
        (who, person, within): (
            Option<&Bounded>,
            &ui::permissions::policy::PersonId,
            &Within,
        ),
    ) -> (Vec<crate::search::SearchDoc>, Vec<Option<IndexEntry>>) {
        let mut docs = Vec::new();
        let mut searched = Vec::new();
        for route in routes {
            // What the app publishes for search is read once per change where the replica's
            // documents are followed; where none is followed, the document is read whole.
            let (entries, changed) = self.entries_of(workspace_id, route);
            let entries = entries.unwrap_or_default();
            let held = !entries.is_empty();
            for entry in entries.iter() {
                if !placed_for(person, entry) || !inside(within, entry) {
                    continue;
                }
                let text = entry
                    .subtitle
                    .iter()
                    .chain(entry.fields.iter().map(|(text, _)| text))
                    .map(String::as_str)
                    .collect::<Vec<&str>>()
                    .join("\n");
                docs.push(crate::search::SearchDoc {
                    route: route.clone(),
                    title: Some(entry.title.clone()),
                    text: text.into(),
                    changed,
                    open: false,
                });
                searched.push(Some(entry.clone()));
            }
            // What its app tells search nothing about is ranked whole, so only for a caller
            // who may read all of it.
            if held || !within.is_everything() {
                continue;
            }
            if !matches!(
                self.readable_cached(workspace_id, who, route),
                Some(Readable::Whole)
            ) {
                continue;
            }
            let Some(text) = self.plain_text(workspace_id, route) else {
                continue;
            };
            docs.push(crate::search::SearchDoc {
                route: route.clone(),
                title: None,
                text,
                changed,
                open: false,
            });
            searched.push(None);
        }
        (docs, searched)
    }

    /// What the document at `route` publishes to search and when it last changed, opened only
    /// when the index has not read it yet.
    fn entries_of(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
    ) -> (Option<Arc<[IndexEntry]>>, Option<u64>) {
        let readings = &mut self.titles;
        let counted = &self.shared.search;
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return (None, None);
        };
        let Some(kept) = workspace.documents.as_mut() else {
            return (None, None);
        };
        let read = |bytes: Vec<u8>| readings.entries(&route.app_id, bytes);
        // The index keeps what each document's app publishes, read as it landed
        // ([`Worker::warm_entries`]): a query answers from it without opening the document,
        // and opens only one that arrived since its last warm pass.
        let (entries, opened) = if kept.indexed(route) {
            (kept.kept_entries(route), false)
        } else {
            kept.entries(route, read)
        };
        if opened {
            counted
                .opened
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        (entries, kept.changed(route))
    }

    /// The document at `route` whole, as search ranks one whose app tells it nothing.
    fn plain_text(&mut self, workspace_id: WorkspaceId, route: &DocumentRoute) -> Option<Arc<str>> {
        self.workspaces
            .get_mut(&workspace_id)?
            .documents
            .as_mut()?
            .text(route, Reading::Plain)
    }

    /// The resource `entry` is about, in the record at `route`, as `caller`'s listing names it:
    /// its own row, asked for among the rows that mention its id under each resource its id
    /// extends (a note under its notebook), the nearest first, then at the record's top. `None`
    /// for a resource the caller lists nothing of.
    /// The path `entry` itself names, as its app publishes it: the resource a caller addresses
    /// it by, which the app's own search index holds ([`IndexEntry::resource`]). `None` for an
    /// entry that names an action on another resource, which the app's listing names: naming
    /// that one mounts and reads the document.
    fn listed_resource(&self, route: &DocumentRoute, entry: &IndexEntry) -> Option<Found> {
        // An action on another resource names it instead: that resource is what a caller
        // addresses, and only the app's listing names it.
        if entry
            .action
            .as_ref()
            .is_some_and(|action| action.resource != entry.resource)
        {
            return None;
        }
        Some(Found {
            resource: entry.resource.clone(),
            kind: self
                .catalogue
                .get(&route.app_id)
                .map(|app| app.resource_type.as_str().to_owned())
                .unwrap_or_default(),
            label: entry.title.clone(),
            open: entry.action.as_ref().map(|action| action.action.clone()),
            arguments: serde_json::Map::new(),
            app: Some(route.app_id.clone()),
            provider: Some(record_provider(route)),
            when: entry.when,
        })
    }

    /// What the document at `route` lists at its top level, as `caller` names its resources: read
    /// on a find and kept until a write lands (per caller, for one served within bounds), so a
    /// second find names what it finds without mounting the document again.
    fn route_listing(
        &mut self,
        workspace_id: WorkspaceId,
        who: Option<&Bounded>,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Option<Vec<CatalogueRow>> {
        if let Some(who) = who {
            let key = Reader::of(who);
            if let Some(access) = self.access(workspace_id, &key)
                && let Some(rows) = access.listings.get(route)
            {
                return Some(rows.clone());
            }
            let rows = self.page_rows(workspace_id, caller, route)?;
            if let Some(access) = self.access(workspace_id, &key) {
                access.listings.insert(route.clone(), rows.clone());
            }
            return Some(rows);
        }
        if let Some(rows) = self
            .workspaces
            .get(&workspace_id)
            .and_then(|workspace| workspace.listings.get(route))
        {
            return Some(rows.clone());
        }
        let rows = self.page_rows(workspace_id, caller, route)?;
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.listings.insert(route.clone(), rows.clone());
        }
        Some(rows)
    }

    /// The rows the document at `route` lists at its top level, as `caller` reads it now.
    fn page_rows(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Option<Vec<CatalogueRow>> {
        let query = CatalogueQuery {
            provider: Some(record_provider(route)),
            max_results: NAMING_ROWS,
            ..CatalogueQuery::default()
        };
        self.catalogue_page(workspace_id, caller, &query)
            .map(|page| page.rows)
            .ok()
    }

    /// [`Self::resolve_named`] from what the workspace kept of it while no write landed there.
    fn named_resource(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        who: Option<&Bounded>,
        route: &DocumentRoute,
        entry: &IndexEntry,
    ) -> Option<Found> {
        let key = (route.clone(), entry.resource.clone());
        if let Some(who) = who {
            let reader = Reader::of(who);
            if let Some(access) = self.access(workspace_id, &reader)
                && let Some(known) = access.named.get(&key)
            {
                return known.clone();
            }
            let answer = self.resolve_named(workspace_id, caller, Some(who), route, entry);
            if let Some(access) = self.access(workspace_id, &reader) {
                access.named.insert(key, answer.clone());
            }
            return answer;
        }
        if let Some(workspace) = self.workspaces.get(&workspace_id)
            && let Some(known) = workspace.named.get(&key)
        {
            return known.clone();
        }
        let answer = self.resolve_named(workspace_id, caller, None, route, entry);
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.named.insert(key, answer.clone());
        }
        answer
    }

    /// The resource `entry` is about, in the document at `route`, as `caller`'s listing names
    /// it: its own row, asked for among the rows that mention its id under each resource its id
    /// extends (a note under its notebook), the nearest first, then at the document's top.
    /// `None` for a resource the caller lists nothing of.
    fn resolve_named(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        who: Option<&Bounded>,
        route: &DocumentRoute,
        entry: &IndexEntry,
    ) -> Option<Found> {
        let id = entry.resource.as_str();
        // What the document lists from its top, one read kept for every hit in it until a write
        // lands: a listing mounts the document and reads it out, and a search that named twenty
        // hits in one notebook paid for the notebook twenty times.
        if let Some(row) = self
            .route_listing(workspace_id, who, caller, route)
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.resource.id == id)
        {
            return Some(Found {
                resource: row.resource.id.clone(),
                kind: row.resource.kind.clone(),
                label: row.resource.label,
                open: None,
                arguments: serde_json::Map::new(),
                app: Some(route.app_id.clone()),
                provider: Some(record_provider(route)),
                when: entry.when,
            });
        }
        for parent in naming_parents(route, id) {
            let query = CatalogueQuery {
                provider: Some(record_provider(route)),
                parent_resource: parent,
                needle: entry.resource.clone(),
                max_results: NAMING_ROWS,
                ..CatalogueQuery::default()
            };
            let Ok(page) = self.catalogue_page(workspace_id, caller, &query) else {
                continue;
            };
            if let Some(row) = page.rows.into_iter().find(|row| row.resource.id == id) {
                return Some(Found {
                    resource: row.resource.id,
                    kind: row.resource.kind,
                    label: row.resource.label,
                    open: None,
                    arguments: serde_json::Map::new(),
                    app: Some(route.app_id.clone()),
                    provider: query.provider,
                    when: entry.when,
                });
            }
        }
        // An app that keeps its items in parts beside its own document lists them under a
        // container its top level names: each container is one page read for the row it holds.
        let mut query = CatalogueQuery {
            provider: Some(record_provider(route)),
            needle: entry.resource.clone(),
            max_results: NAMING_ROWS,
            ..CatalogueQuery::default()
        };
        let root = self.catalogue_page(workspace_id, caller, &query);
        for container in root
            .ok()?
            .rows
            .into_iter()
            .filter(|row| row.has_children)
            .map(|row| row.resource.id)
            .take(NAMING_CONTAINERS)
        {
            query.parent_resource = Some(container);
            let Ok(page) = self.catalogue_page(workspace_id, caller, &query) else {
                continue;
            };
            if let Some(row) = page.rows.into_iter().find(|row| row.resource.id == id) {
                return Some(Found {
                    resource: row.resource.id,
                    kind: row.resource.kind,
                    label: row.resource.label,
                    open: None,
                    arguments: serde_json::Map::new(),
                    app: Some(route.app_id.clone()),
                    provider: query.provider,
                    when: entry.when,
                });
            }
        }
        None
    }

    /// The resource the listing of the record at `route` leads with for `caller`, as a search
    /// answers it: `None` for a record the caller lists nothing of.
    fn leading_resource(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        who: Option<&Bounded>,
        route: &DocumentRoute,
    ) -> Option<Found> {
        let row = self
            .route_listing(workspace_id, who, caller, route)?
            .into_iter()
            .next()?;
        Some(Found {
            resource: row.resource.id,
            kind: row.resource.kind,
            label: row.resource.label,
            open: None,
            arguments: serde_json::Map::new(),
            app: Some(route.app_id.clone()),
            provider: Some(record_provider(route)),
            when: None,
        })
    }

    /// What the document `address` names holds, as `caller` may read it now: one document of
    /// the workspace, as every reader of it is answered
    /// ([`tilo_actions::remote::ResourceReads`]), decided by the same rules a listing and a read
    /// of an app's resource are ([`Self::readable`]). `Absent` for one the workspace has never
    /// written: the service holds its documents, so one it holds none of is not on its way.
    pub(super) fn document_read(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionCallError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        let Some(cluster_id) = state.workspace.clone() else {
            return Ok(tilo_actions::remote::ResourceCopy::Absent);
        };
        let route = DocumentRoute {
            app_id: address.app.clone(),
            cluster_id,
            document: address.record.clone(),
        };
        let who = self.bounded(caller);
        if self.readable(workspace_id, who.as_ref(), &route).is_none() {
            return Err(ActionCallError::Refused {
                reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
            });
        }
        match state
            .engine
            .record(&route.cluster_id, &route.app_id, &route.document)
        {
            Ok(Some(bytes)) => Ok(tilo_actions::remote::ResourceCopy::Document(bytes)),
            Ok(None) => Ok(tilo_actions::remote::ResourceCopy::Absent),
            Err(error) => Err(ActionCallError::Unavailable { reason: error }),
        }
    }

    /// The changes to what `workspace_id` holds after `after`, for `caller`: the person whose
    /// device this service is on reads every change of the workspace, and anyone else it admits
    /// reads the changes of what the workspace lets them read now, an agent held to the rules
    /// its turn carries ([`Self::readable`]).
    pub(super) fn changes(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        after: Option<&str>,
    ) -> Result<ChangePage, ActionCallError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionCallError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        // Nothing written in the workspace yet: no records, and its first ones are news.
        let routes = match state.workspace.clone() {
            Some(cluster_id) => self.documents(Some(workspace_id), &state, &cluster_id),
            None => Vec::new(),
        };
        let records: Vec<DocumentKey> = routes
            .iter()
            .map(|route| (route.app_id.clone(), route.document.clone()))
            .collect();
        let cluster_id = state.workspace.clone().unwrap_or_default();
        let engine = std::sync::Arc::clone(&state.engine);
        let log = &mut self.membership(workspace_id)?.changes;
        log.take_in(
            &records,
            |(app, record)| {
                engine
                    .document_revision(&cluster_id, app, record)
                    .ok()
                    .flatten()
            },
            |(app, record)| engine.record(&cluster_id, app, record).ok().flatten(),
            &ui::history_source::sentence::ThreadWords,
        );
        let page = log.page(after);
        // What is said in a direct conversation is told to the people it is between
        // ([`super::direct`]).
        let page = self.told(caller, &cluster_id, &state, page);
        Ok(self.read_by(workspace_id, caller, &cluster_id, page))
    }

    /// `page` of the feed as `caller` may read it: without the changes of what the workspace
    /// does not let them read, by the rule a search is answered by ([`Self::readable`]). A
    /// document of which they read only the items their permissions name is left out whole: a
    /// change does not say which item it is of. The page's cursor stands, so whoever follows
    /// the feed reads on behind what was left out.
    fn read_by(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        cluster_id: &str,
        mut page: ChangePage,
    ) -> ChangePage {
        let mut read: std::collections::HashMap<DocumentKey, bool> =
            std::collections::HashMap::new();
        page.changes.retain(|change| {
            let key: DocumentKey = (change.app.clone(), change.record.clone());
            if let Some(reads) = read.get(&key) {
                return *reads;
            }
            let route = DocumentRoute {
                app_id: key.0.clone(),
                cluster_id: cluster_id.to_owned(),
                document: key.1.clone(),
            };
            let reads = self.reads_whole(workspace_id, caller, &route);
            read.insert(key, reads);
            reads
        });
        page
    }

    /// Whether `caller` may read all of what is kept at `route`, by the rule a search is
    /// answered by ([`Self::readable`]): what the feed of changes tells them of, and what a
    /// write must land in to wake their wait for a change.
    pub(super) fn reads_whole(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> bool {
        let who = self.bounded(caller);
        matches!(
            self.readable(workspace_id, who.as_ref(), route),
            Some(Readable::Whole)
        )
    }
}

/// How long a read of one document waits on the service's thread for its answer.
const DOCUMENT_READ_WAIT: Duration = Duration::from_secs(30);

impl tilo_actions::remote::ResourceReads for WorkspaceActions {
    /// What the document `address` names holds, as `caller` may read it now: asked of the
    /// service's own thread, which is where the workspace's documents are held, and waited for
    /// here.
    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        let Some(workspace_id) = caller.workspace().and_then(WorkspaceId::parse) else {
            return Err(ActionCallError::NotPermitted);
        };
        let (done, answer) = pending();
        if self
            .submit(Job::ReadDocument {
                workspace_id,
                caller: caller.clone(),
                address: address.clone(),
                done,
            })
            .is_err()
        {
            return Err(ActionCallError::Unavailable {
                reason: "the workspace's service is not running".into(),
            });
        }
        match answer.wait_timeout(DOCUMENT_READ_WAIT) {
            Ok(Some(result)) => result,
            _ => Err(ActionCallError::Unavailable {
                reason: "the workspace's service did not answer the read in time".into(),
            }),
        }
    }

    /// Every write to a document wakes a reader waiting past it
    /// ([`tilo_actions::remote::ResourceWake`]): the service holds it, and the read past a
    /// revision is answered again once the write lands.
    fn watch(
        &self,
        _: &Caller,
        _: &tilo_actions::remote::ResourceAddress,
        _: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        Ok(())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(document: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "workspace".into(),
            document: document.into(),
        }
    }

    /// A chain's part is searched as the chain's document: its items are
    /// published by the chain, under `chain:` and its id, and are asked for
    /// there first.
    #[test]
    fn a_chain_parts_item_is_asked_for_under_the_chain_first() {
        let part = route("derived:part.0@derived:pages@derived:default@tab:0000000000000000");
        assert_eq!(
            naming_parents(&part, "notes.pages/pages/note"),
            [
                Some("chain:derived:pages@derived:default@tab:0000000000000000".to_owned()),
                Some("notes.pages/pages".to_owned()),
                Some("notes.pages".to_owned()),
                None,
            ]
        );
    }

    /// A document of its own lists its items under themselves, as it always has.
    #[test]
    fn a_documents_own_item_is_asked_for_under_its_collection() {
        let document = route("derived:messages@derived:default@tab:0000000000000000");
        assert_eq!(
            naming_parents(&document, "chat.messages/messages/m1"),
            [
                Some("chat.messages/messages".to_owned()),
                Some("chat.messages".to_owned()),
                None,
            ]
        );
    }
}
