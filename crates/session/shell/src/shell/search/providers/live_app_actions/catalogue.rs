//! One inert, bounded prefix for ordinary scoped machine discovery.
//!
//! Raw transport replies remain limited to 64 pages. This additional prefix is
//! limited to the existing headless response's 16 MiB / 16,384 rows, using
//! conservative serialized metadata accounting (not exact allocator usage).
//! Its frontier retains the existing bounded DFS cursor; it owns no
//! callbacks or permission decisions. Pending observations expose no growing
//! prefix, so repeated polls do not clone every previously received descriptor.
use super::{ActionDirectoryRoot, ID_SEPARATOR, LiveAppActionsProvider};
use crate::shell::search::{
    ActionDiscoveryScope, QueryIncompleteReason, ResourceKind, SearchBudget, SearchContext,
    SearchCursor, SearchPage, SearchProvider, SearchResult,
};
use std::collections::HashMap;
use tilo_actions::{ActionDescriptor, ResourceSemanticIdentity};

#[derive(PartialEq)]
struct Context {
    discovery: ActionDiscoveryScope,
    scope: String,
    query: String,
    programmatic: bool,
    wanted: Vec<ResourceKind>,
    actor: String,
    surfaces: u64,
    roots: u64,
}

struct DirectoryEntry {
    root: ActionDirectoryRoot,
    revision: String,
    descriptor: ActionDescriptor,
    actor: String,
    needle: String,
    cursor: Vec<u8>,
    parent: String,
}

struct Entry {
    id: String,
    directory: Option<Box<DirectoryEntry>>,
    signature: [u8; 32],
    bytes: usize,
}

pub(super) struct Assembly {
    context: Context,
    epoch: String,
    frontier: Option<SearchCursor>,
    entries: Vec<Entry>,
    addresses: HashMap<String, usize>,
    revisions: HashMap<u64, String>,
    pub(super) bytes: usize,
    complete: bool,
}

impl Assembly {
    fn new(context: Context) -> Self {
        Self {
            context,
            epoch: uuid::Uuid::new_v4().to_string(),
            frontier: None,
            entries: Vec::new(),
            addresses: HashMap::new(),
            revisions: HashMap::new(),
            bytes: 0,
            complete: false,
        }
    }

    fn append(&mut self, entry: Entry) -> Result<(), QueryIncompleteReason> {
        if let Some(index) = self.addresses.get(&entry.id) {
            let before = self
                .entries
                .get(*index)
                .ok_or(QueryIncompleteReason::Unavailable)?;
            return if before.signature == entry.signature {
                Ok(())
            } else {
                Err(QueryIncompleteReason::StaleCursor)
            };
        }
        if self.entries.len() >= super::super::super::HEADLESS_MAX_RESULTS
            || entry.bytes > super::super::super::HEADLESS_MAX_BYTES.saturating_sub(self.bytes)
        {
            return Err(QueryIncompleteReason::BudgetExhausted);
        }
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.addresses.insert(entry.id.clone(), self.entries.len());
        self.entries.push(entry);
        Ok(())
    }
}

impl LiveAppActionsProvider {
    fn catalogue_context(&self, ctx: &SearchContext<'_>) -> Option<Context> {
        let scope = ctx.action_scope?;
        let room = ctx
            .chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == scope.room)?
            .room;
        Some(Context {
            discovery: scope.clone(),
            scope: Self::surface_scope(ctx),
            query: ctx.needle.trim().to_lowercase(),
            programmatic: ctx.include_programmatic,
            wanted: ctx.wanted.to_vec(),
            actor: ctx.chrome.local_user_for_room(&room),
            surfaces: self.broker.surfaces_generation(),
            roots: self
                .broker
                .state()
                .action_directory_root_page(None, 0)
                .generation,
        })
    }

    fn compacted_source_current(&self, assembly: &Assembly) -> bool {
        assembly.context.surfaces == self.broker.surfaces_generation()
            && assembly.context.roots
                == self
                    .broker
                    .state()
                    .action_directory_root_page(None, 0)
                    .generation
            && self.directory_query.borrow().as_deref() == Some(assembly.context.query.as_str())
            && self.directory_failures.borrow().is_empty()
            && self.directory_invalidations.borrow().is_empty()
    }

    pub(super) fn compacted_directory_action(
        &self,
        peer: u64,
        actor: &str,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Option<(ActionDescriptor, String, Vec<u8>, String)> {
        let retained = self.catalogue.borrow();
        let assembly = retained.as_ref()?;
        if !self.compacted_source_current(assembly) || assembly.context.actor != actor {
            return None;
        }
        let id = format!(
            "directory{ID_SEPARATOR}{peer}{ID_SEPARATOR}{revision}{ID_SEPARATOR}{resource}{ID_SEPARATOR}{action}"
        );
        let entry = assembly.entries.get(*assembly.addresses.get(&id)?)?;
        let directory = entry.directory.as_ref()?;
        (directory.root.peer_id == peer
            && directory.actor == actor
            && directory.revision == revision)
            .then(|| {
                (
                    directory.descriptor.clone(),
                    directory.needle.clone(),
                    directory.cursor.clone(),
                    directory.parent.clone(),
                )
            })
    }

    pub(super) fn compacted_semantic_identity(
        &self,
        peer: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Option<ResourceSemanticIdentity> {
        let retained = self.catalogue.borrow();
        let assembly = retained.as_ref()?;
        let descriptor = self
            .compacted_directory_action(peer, &assembly.context.actor, revision, resource, action)?
            .0;
        descriptor
            .semantic_identity
            .filter(|identity| identity.is_valid() && identity.kind == descriptor.resource_kind)
    }

    fn compact_entry(&self, result: SearchResult, ctx: &SearchContext<'_>) -> Option<Entry> {
        let mut fields = result.id.split(ID_SEPARATOR);
        if fields.next()? != "directory" {
            // Local rows remain keyed by their exact published address. Their
            // normal resolve path revalidates current contracts and actor.
            let metadata = serde_json::to_vec(&(
                &result.id,
                &result.params,
                &result.contract,
                &result.title,
                &result.subtitle,
                &result.kind.sort_key(),
                &result.operation_kind,
            ))
            .ok()?;
            let wire = metadata
                .len()
                .saturating_add(crate::shell::search::estimated_result_bytes(&result));
            return Some(Entry {
                bytes: wire.saturating_mul(2).saturating_add(512),
                id: result.id,
                signature: *blake3::hash(&metadata).as_bytes(),
                directory: None,
            });
        }
        let peer = fields.next()?.parse::<u64>().ok()?;
        let revision = fields.next()?.to_owned();
        let _resource = fields.next()?;
        let _operation = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        let root = self
            .broker
            .state()
            .action_directory_root_page(peer.checked_sub(1), 1)
            .roots
            .into_iter()
            .find(|root| root.peer_id == peer)?;
        if !ctx
            .action_scope?
            .matches_route(&root.app_id, &root.cluster_id, &root.record_id)
        {
            return None;
        }
        let actor = self.catalogue_context(ctx)?.actor;
        let (descriptor, needle, cursor, parent) =
            self.compact_declaration(&result, &root, &revision, &actor)?;
        let metadata = serde_json::to_vec(&(
            &descriptor,
            root.peer_id,
            &root.app_id,
            &root.cluster_id,
            &root.record_id,
            &revision,
            &actor,
        ))
        .ok()?;
        let bytes = metadata
            .len()
            .saturating_add(root.app_id.len())
            .saturating_add(root.cluster_id.len())
            .saturating_add(root.record_id.len())
            .saturating_add(actor.len())
            .saturating_add(needle.len())
            .saturating_add(cursor.len())
            .saturating_add(parent.len())
            .saturating_add(result.id.len())
            .saturating_add(revision.len())
            .saturating_mul(2)
            .saturating_add(512);
        Some(Entry {
            id: result.id,
            bytes,
            signature: *blake3::hash(&metadata).as_bytes(),
            directory: Some(Box::new(DirectoryEntry {
                root,
                revision,
                descriptor,
                actor,
                needle,
                cursor,
                parent,
            })),
        })
    }

    fn compact_declaration(
        &self,
        result: &SearchResult,
        root: &ActionDirectoryRoot,
        revision: &str,
        actor: &str,
    ) -> Option<(ActionDescriptor, String, Vec<u8>, String)> {
        let retained = self.auxiliary_snapshot.borrow();
        let snapshot = retained.as_ref()?;
        let declaration = snapshot.entries.iter().find_map(|entry| {
            let super::AuxiliaryEntry::Directory {
                root: source,
                revision: version,
                descriptor,
                actor: owner,
                cursor,
                parent_resource,
            } = entry
            else {
                return None;
            };
            if source != root
                || version != revision
                || owner != actor
                || format!("directory{ID_SEPARATOR}{}{ID_SEPARATOR}{version}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
                    source.peer_id, descriptor.address.resource, descriptor.address.action) != result.id
            {
                return None;
            }
            let rendered = self.directory_action_result(
                source,
                version,
                descriptor,
                &snapshot.query,
                owner,
                cursor,
                parent_resource,
            );
            (rendered.title == result.title
                && rendered.subtitle == result.subtitle
                && rendered.params == result.params
                && rendered.contract == result.contract
                && rendered.operation_kind == result.operation_kind)
                .then(|| {
                    (
                        descriptor.clone(),
                        snapshot.query.clone(),
                        cursor.clone(),
                        parent_resource.clone(),
                    )
                })
        })?;
        drop(retained);
        Some(declaration)
    }

    fn validate_frontier(
        &self,
        assembly: &mut Assembly,
        ctx: &SearchContext<'_>,
    ) -> Result<(), QueryIncompleteReason> {
        if self.catalogue_context(ctx).as_ref() != Some(&assembly.context) {
            return Err(QueryIncompleteReason::StaleCursor);
        }
        if let Some(reason) = self.directory_failures.borrow().values().copied().next() {
            return Err(reason);
        }
        // A first reply changes only the local projection epoch. Rebase the
        // owned local frontier only after every actual ready SDK revision agrees
        // with its pinned owner. Never change the remote one-use cursor bytes.
        for ((peer, actor, needle, _, _), page) in self.directory_pages.borrow().iter() {
            if actor != &assembly.context.actor || needle != &assembly.context.query {
                continue;
            }
            let root = self
                .broker
                .state()
                .action_directory_root_page(peer.checked_sub(1), 1)
                .roots
                .into_iter()
                .find(|root| root.peer_id == *peer);
            if !root.is_some_and(|root| {
                assembly.context.discovery.matches_route(
                    &root.app_id,
                    &root.cluster_id,
                    &root.record_id,
                )
            }) {
                continue;
            }
            if page.revision.is_empty() {
                return Err(QueryIncompleteReason::Unavailable);
            }
            if let Some(before) = assembly.revisions.get(peer) {
                if before != &page.revision {
                    return Err(QueryIncompleteReason::StaleCursor);
                }
            } else {
                let bytes = page.revision.len().saturating_mul(2).saturating_add(128);
                if assembly.revisions.len() >= super::super::super::HEADLESS_MAX_RESULTS
                    || bytes
                        > super::super::super::HEADLESS_MAX_BYTES.saturating_sub(assembly.bytes)
                {
                    return Err(QueryIncompleteReason::BudgetExhausted);
                }
                assembly.bytes = assembly.bytes.saturating_add(bytes);
                assembly.revisions.insert(*peer, page.revision.clone());
            }
        }
        Ok(())
    }

    fn advance_catalogue(
        &self,
        assembly: &mut Assembly,
        ctx: SearchContext<'_>,
    ) -> Result<(), QueryIncompleteReason> {
        // Bound one synchronous observation even when many ready source pages
        // contain filtered rows. Unfinished real transport is retained for the
        // next identical public observation, rather than restarted at the root.
        for _ in 0..32 {
            self.poll_directory_pages();
            self.validate_frontier(assembly, &ctx)?;
            if let Some(cursor) = &mut assembly.frontier {
                cursor.revision = self.search_revision(&ctx);
            }
            let before = assembly.frontier.clone();
            let page = self.query_page(ctx, before.as_ref(), SearchBudget::PALETTE);
            if page.stale_cursor {
                return Err(QueryIncompleteReason::StaleCursor);
            }
            self.validate_frontier(assembly, &ctx)?;
            let empty = page.results.is_empty();
            for result in page.results {
                let entry = self
                    .compact_entry(result, &ctx)
                    .ok_or(QueryIncompleteReason::Unavailable)?;
                assembly.append(entry)?;
            }
            if let Some(reason) = self.query_incomplete_reason(&ctx)
                && reason != QueryIncompleteReason::Pending
            {
                return Err(reason);
            }
            assembly.frontier = page.next_cursor;
            if page.exhausted {
                assembly.complete = true;
                return Ok(());
            }
            if assembly.frontier.is_none() {
                return Err(QueryIncompleteReason::BudgetExhausted);
            }
            if empty && (assembly.frontier == before || !self.directory_pending.borrow().is_empty())
            {
                return Ok(());
            }
        }
        Ok(())
    }

    fn reset_compacted_chain(&self, assembly: &Assembly) {
        let belongs = |key: &super::DirectoryCacheKey| {
            if key.1 != assembly.context.actor || key.2 != assembly.context.query {
                return false;
            }
            assembly.revisions.contains_key(&key.0)
                || self
                    .broker
                    .state()
                    .action_directory_root_page(key.0.checked_sub(1), 1)
                    .roots
                    .into_iter()
                    .any(|root| {
                        root.peer_id == key.0
                            && assembly.context.discovery.matches_route(
                                &root.app_id,
                                &root.cluster_id,
                                &root.record_id,
                            )
                    })
        };
        self.directory_pages
            .borrow_mut()
            .retain(|key, _| !belongs(key));
        self.directory_pending
            .borrow_mut()
            .retain(|key, _| !belongs(key));
        self.directory_ancestor_pins
            .borrow_mut()
            .retain(|key| !belongs(key));
        self.auxiliary_snapshot.borrow_mut().take();
        self.directory_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn assembled_page(
        &self,
        assembly: &Assembly,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let offset = match cursor {
            None => 0,
            Some(cursor) if cursor.revision == assembly.epoch => {
                let Some(offset) = cursor
                    .token
                    .strip_prefix("catalogue:")
                    .and_then(|token| token.parse::<usize>().ok())
                else {
                    return SearchPage::stale(&assembly.epoch);
                };
                offset
            }
            Some(_) => return SearchPage::stale(&assembly.epoch),
        };
        let mut results = Vec::new();
        let mut bytes = 0usize;
        let mut next = offset;
        for entry in assembly.entries.iter().skip(offset) {
            if results.len() >= budget.max_results
                || results.len() >= budget.max_work
                || entry.bytes > budget.max_bytes.saturating_sub(bytes)
            {
                break;
            }
            let result = if let Some(directory) = &entry.directory {
                Some(self.directory_action_result(
                    &directory.root,
                    &directory.revision,
                    &directory.descriptor,
                    &directory.needle,
                    &directory.actor,
                    &directory.cursor,
                    &directory.parent,
                ))
            } else {
                self.resolve(&entry.id, ctx)
            };
            let Some(result) = result else {
                return SearchPage::stale(&assembly.epoch);
            };
            bytes = bytes.saturating_add(entry.bytes);
            next = next.saturating_add(1);
            results.push(result);
        }
        let exhausted = next >= assembly.entries.len();
        SearchPage {
            revision: assembly.epoch.clone(),
            results,
            next_cursor: (!exhausted && next > offset).then(|| SearchCursor {
                revision: assembly.epoch.clone(),
                token: format!("catalogue:{next}"),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }

    fn incomplete_local_page(
        &self,
        ctx: SearchContext<'_>,
        budget: SearchBudget,
        revision: &str,
        reason: QueryIncompleteReason,
    ) -> SearchPage {
        if reason != QueryIncompleteReason::Unavailable {
            return pending_page(revision);
        }
        // An unavailable remote directory does not erase independently current
        // published local declarations. Use the ordinary local projection and
        // its current authority checks, never the folded remote prefix. The
        // caller retains the actual failure, so these rows cannot certify a
        // complete machine catalogue or make acceptance invoke an action.
        let local = Self::live_page(
            self.search_revision(&ctx),
            Vec::new(),
            "local",
            0,
            &ctx.needle.trim().to_lowercase(),
            &Self::surface_scope(&ctx),
        );
        let page = self.query_page(ctx, local.next_cursor.as_ref(), budget);
        SearchPage::exhausted(revision, page.results)
    }

    pub(super) fn compacted_catalogue_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        self.apply_directory_invalidations();
        self.poll_directory_pages();
        let Some(context) = self.catalogue_context(&ctx) else {
            self.catalogue_failure
                .set(Some(QueryIncompleteReason::Unavailable));
            return pending_page("unavailable");
        };
        let previous = self.catalogue.borrow_mut().take();
        let mut assembly = match previous {
            Some(assembly) if assembly.context == context => assembly,
            Some(assembly) => {
                self.reset_compacted_chain(&assembly);
                Assembly::new(context)
            }
            None => Assembly::new(context),
        };
        if let Err(reason) = self.validate_frontier(&mut assembly, &ctx) {
            self.reset_compacted_chain(&assembly);
            self.catalogue_failure.set(Some(reason));
            return self.incomplete_local_page(ctx, budget, &assembly.epoch, reason);
        }
        if !assembly.complete
            && let Err(reason) = self.advance_catalogue(&mut assembly, ctx)
        {
            self.reset_compacted_chain(&assembly);
            self.catalogue_failure.set(Some(reason));
            return self.incomplete_local_page(ctx, budget, &assembly.epoch, reason);
        }
        let result = if assembly.complete {
            self.assembled_page(&assembly, ctx, cursor, budget)
        } else {
            self.catalogue_failure
                .set(Some(QueryIncompleteReason::Pending));
            pending_page(&assembly.epoch)
        };
        if let Err(reason) = self.validate_frontier(&mut assembly, &ctx) {
            self.reset_compacted_chain(&assembly);
            self.catalogue_failure.set(Some(reason));
            return self.incomplete_local_page(ctx, budget, &assembly.epoch, reason);
        }
        *self.catalogue.borrow_mut() = Some(assembly);
        result
    }
}

fn pending_page(revision: &str) -> SearchPage {
    SearchPage {
        revision: revision.into(),
        results: Vec::new(),
        next_cursor: None,
        exhausted: false,
        stale_cursor: false,
        budget_exhausted: false,
    }
}
