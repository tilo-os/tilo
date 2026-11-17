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
//! A caller is answered only with what it may read, decided before anything is ranked:
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

use tilo_actions::remote::{ChangePage, Found, Within};
use ui::history_source::feed::DocumentKey;
use ui::search::index_publication::IndexEntry;

use std::collections::HashMap;

use super::bounded::Bounded;
use super::{
    ActionCallError, ActionKind, Answer, Caller, CatalogueQuery, CatalogueRow, DocumentRoute,
    Needs, TreePath, Worker, WorkspaceId, record_provider,
};
use crate::workspace_documents::Reading;

/// The most records one search answers with.
const MOST_FOUND: usize = 20;

/// The most rows of a record's listing read to name one resource a search found in it.
const NAMING_ROWS: u32 = 64;

/// How much of one resource a search's caller may read.
enum Readable {
    /// All of it.
    Whole,
    /// Only the items their permissions name, each decided for itself.
    Items(Box<ui::permissions::PermissionContext>),
}

impl Readable {
    /// Whether the caller may read `resource`, which the resource at `route` holds.
    fn reads(&self, route: &DocumentRoute, resource: &str) -> bool {
        match self {
            Self::Whole => true,
            Self::Items(permissions) => item_node(route, resource)
                .is_some_and(|node| permissions.answer(&node, ActionKind::Read) == Answer::Allow),
        }
    }
}

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

/// Where the workspace decides the item `resource` names as `<kind>/<field>/<item>` in the
/// resource at `route`: what a grant on that item alone names. `None` for an id of another
/// shape, which no such grant names.
fn item_node(route: &DocumentRoute, resource: &str) -> Option<TreePath> {
    let (collection, item) = resource.rsplit_once('/')?;
    let (kind, field) = collection.split_once('/')?;
    (!kind.is_empty() && !field.is_empty() && !field.contains('/') && !item.is_empty()).then(|| {
        TreePath::root()
            .app(route.app_id.as_str())
            .kind(kind)
            .resource(route.document.as_str())
            .item(item)
    })
}

impl Worker {
    /// What `needle` finds among the records of `workspace_id`, best first, as `caller` lists
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
        // What the caller may read is decided before anything is ranked: nothing they may not
        // read weighs in what they are answered.
        let who = self.bounded(caller);
        let person = who
            .as_ref()
            .map_or_else(|| self.member.clone(), |who| who.person.clone());
        let routes: Vec<(DocumentRoute, Readable)> = self
            .documents(&state, &cluster_id)
            .into_iter()
            .filter_map(|route| {
                let readable = self.readable(workspace_id, who.as_ref(), &route)?;
                Some((route, readable))
            })
            .take(crate::search::MOST_SEARCHED)
            .collect();
        let (docs, searched) = self.searchable(workspace_id, &routes, (&person, within));
        let entries_ms = began.elapsed().as_millis();
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
        let mut found: Vec<Found> = Vec::new();
        // One listing of a document answers for every hit in it: a listing mounts the document
        // and reads it out, and a search that named twenty hits in one notebook paid for the
        // notebook twenty times.
        let mut listings: HashMap<DocumentRoute, Vec<CatalogueRow>> = HashMap::new();
        for at in ranked {
            if found.len() >= MOST_FOUND {
                break;
            }
            let (Some(doc), Some(what)) = (docs.get(at), searched.get(at)) else {
                continue;
            };
            let resource = match what {
                Some(entry) => self.listed_resource(&doc.route, entry).or_else(|| {
                    self.named_resource(workspace_id, caller, &doc.route, entry, &mut listings)
                }),
                None => self.leading_resource(workspace_id, caller, &doc.route),
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
        if within.is_everything() && self.reads_files(workspace_id, who.as_ref(), &cluster_id) {
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
    /// read what its Files app keeps there.
    fn reads_files(&self, workspace_id: WorkspaceId, who: Option<&Bounded>, cluster: &str) -> bool {
        who.is_none_or(|who| {
            self.permissions_across(workspace_id, who, cluster)
                .is_some_and(|permissions| {
                    let files = TreePath::root().app(fs_protocol::tree::FILES_APP);
                    permissions.answer(&files, ActionKind::Read) == Answer::Allow
                })
        })
    }

    /// What a search ranks for `person` of the records at `routes` the replica holds, each as
    /// far as its caller may read it: every resource their apps tell search about, with what it
    /// says, and each record whose app tells it nothing, whole. Beside each document, the entry
    /// it was made of, or `None` for one ranked whole.
    fn searchable(
        &mut self,
        workspace_id: WorkspaceId,
        routes: &[(DocumentRoute, Readable)],
        (person, within): (&ui::permissions::policy::PersonId, &Within),
    ) -> (Vec<crate::search::SearchDoc>, Vec<Option<IndexEntry>>) {
        let mut docs = Vec::new();
        let mut searched = Vec::new();
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return (docs, searched);
        };
        let engine = std::sync::Arc::clone(workspace.peer.engine());
        let mut kept = workspace.documents.as_mut();
        let readings = &mut self.titles;
        let counted = &self.shared.search;
        for (route, readable) in routes {
            let mut read = |bytes: Vec<u8>| readings.entries(&route.app_id, bytes);
            // What the app publishes for search is read once per change where the replica's
            // documents are followed; where none is followed, the document is read whole.
            let (entries, changed) = if let Some(kept) = kept.as_deref_mut() {
                // The index keeps what each document's app publishes, read as it landed
                // ([`Worker::warm_entries`]): a query answers from it without opening the
                // document, and opens only one that arrived since its last warm pass.
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
            } else {
                let bytes = engine
                    .record(&route.cluster_id, &route.app_id, &route.document)
                    .ok()
                    .flatten();
                let entries = bytes.and_then(|bytes| read(bytes).ok()).map(Into::into);
                (entries, None)
            };
            let entries = entries.unwrap_or_default();
            for entry in entries.iter() {
                if !readable.reads(route, &entry.resource)
                    || !placed_for(person, entry)
                    || !inside(within, entry)
                {
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
            if !entries.is_empty()
                || !matches!(readable, Readable::Whole)
                || !within.is_everything()
            {
                continue;
            }
            let text = if let Some(kept) = kept.as_deref_mut() {
                kept.text(route, Reading::Plain)
            } else {
                engine
                    .record(&route.cluster_id, &route.app_id, &route.document)
                    .ok()
                    .flatten()
                    .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                    .map(|doc| crate::search::plain_text(&doc).into())
            };
            let Some(text) = text else {
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

    /// The resource `entry` is about, in the document at `route`, as `caller`'s listing names
    /// it: its own row, asked for among the rows that mention its id under each resource its id
    /// extends (a note under its notebook), the nearest first, then at the document's top.
    /// `None` for a resource the caller lists nothing of.
    fn named_resource(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        entry: &IndexEntry,
        listings: &mut HashMap<DocumentRoute, Vec<CatalogueRow>>,
    ) -> Option<Found> {
        let id = entry.resource.as_str();
        // What the document lists from its top, asked once for every hit in it.
        if !listings.contains_key(route) {
            let query = CatalogueQuery {
                provider: Some(record_provider(route)),
                max_results: NAMING_ROWS,
                ..CatalogueQuery::default()
            };
            let rows = self
                .catalogue_page(workspace_id, caller, &query)
                .map(|page| page.rows)
                .unwrap_or_default();
            listings.insert(route.clone(), rows);
        }
        if let Some(row) = listings
            .get(route)
            .and_then(|rows| rows.iter().find(|row| row.resource.id == id))
        {
            return Some(Found {
                resource: row.resource.id.clone(),
                kind: row.resource.kind.clone(),
                label: row.resource.label.clone(),
                open: None,
                arguments: serde_json::Map::new(),
                app: Some(route.app_id.clone()),
                provider: Some(record_provider(route)),
                when: entry.when,
            });
        }
        let holders = id
            .match_indices('/')
            .map(|(at, _)| id.get(..at).map(str::to_owned))
            .rev()
            .chain(std::iter::once(None));
        for parent in holders {
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
                    provider: Some(record_provider(route)),
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
        route: &DocumentRoute,
    ) -> Option<Found> {
        let query = CatalogueQuery {
            provider: Some(record_provider(route)),
            max_results: 1,
            ..CatalogueQuery::default()
        };
        let page = self.catalogue_page(workspace_id, caller, &query).ok()?;
        let row = page.rows.into_iter().next()?;
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
            Some(cluster_id) => self.documents(&state, &cluster_id),
            None => Vec::new(),
        };
        let records: Vec<DocumentKey> = routes
            .iter()
            .map(|route| (route.app_id.clone(), route.document.clone()))
            .collect();
        let cluster_id = state.workspace.clone().unwrap_or_default();
        let engine = std::sync::Arc::clone(&state.engine);
        let page = {
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
            log.page(after)
        };
        let Some(who) = self.bounded(caller) else {
            return Ok(page);
        };
        // Decided before anything is answered, as a search is: a change to a resource the
        // workspace keeps from the caller, or lets them read only parts of, is not theirs to
        // see.
        let readable: Vec<DocumentRoute> = routes
            .into_iter()
            .filter(|route| {
                matches!(
                    self.readable(workspace_id, Some(&who), route),
                    Some(Readable::Whole)
                )
            })
            .collect();
        let changes = page
            .changes
            .into_iter()
            .filter(|change| {
                readable
                    .iter()
                    .any(|route| route.app_id == change.app && route.document == change.record)
            })
            .collect();
        Ok(ChangePage {
            changes,
            cursor: page.cursor,
        })
    }
}
