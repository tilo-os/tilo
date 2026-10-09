//! Feeding the index from the action host, and opening what a host search
//! hands back through the same door the palette and automation use.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueLimits, CataloguePage, CatalogueQuery,
    InvokeArgument, InvokeRequest, ReceiptStatus, workspace_provider_room,
};
use tilo_actions::{ActionFailure, InvocationId};

use crate::entry::{EntryId, SearchEntry, opening};
use crate::index::{Changes, ResourceIndex};

/// Rows asked for per catalogue page.
const PAGE: u32 = 128;
/// Entries one walk collects at most; a host search is not an archive.
const MAX_ENTRIES: usize = 4096;
/// Pages one publisher is asked for at most in one walk.
const MAX_PAGES: usize = 64;
/// How deep a walk follows a publisher's resource tree.
const MAX_DEPTH: usize = 8;
/// How long one answer from the action host may take.
const ANSWER: Duration = Duration::from_secs(5);
/// Per-page work bounds, the ones the MCP projection asks for.
const LIMITS: CatalogueLimits = CatalogueLimits {
    max_bytes: Some(64 * 1024),
    max_work: Some(512),
    max_depth: Some(16),
};

/// Why a host search's result did not open.
#[derive(Debug, PartialEq)]
pub enum OpenError {
    /// This runtime does not offer that resource to its owner (any more).
    NotOffered,
    /// The action host refused or could not answer.
    Host(ActionHostError),
    /// The opening action ran and failed.
    Failed(ActionFailure),
}

impl From<ActionHostError> for OpenError {
    fn from(error: ActionHostError) -> Self {
        Self::Host(error)
    }
}

/// The resources the action host offers this runtime's owner, as one index
/// every host search reads.
///
/// Only what the catalogue offers `caller` is ever indexed, so a resource
/// the owner may not see, an app that closed without keeping its actions
/// launchable, or a revoked grant never reaches a host search, and leaves
/// it at the next [`Self::refresh`]. Opening re-resolves the action as
/// `caller` and invokes it through the host, so authority is checked when
/// the person acts, not when the host indexed.
pub struct OsSearch {
    host: Arc<dyn ActionHost + Send + Sync>,
    caller: Caller,
    index: Mutex<ResourceIndex>,
}

impl OsSearch {
    pub fn new(host: Arc<dyn ActionHost + Send + Sync>, caller: Caller) -> Self {
        Self {
            host,
            caller,
            index: Mutex::new(ResourceIndex::default()),
        }
    }

    fn index(&self) -> MutexGuard<'_, ResourceIndex> {
        self.index.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Walk the catalogue and make what it offers the index.
    ///
    /// # Errors
    ///
    /// Any unanswered page leaves the index as it was: a partial walk
    /// would read as every unwalked resource having been revoked.
    pub async fn refresh(&self) -> Result<Changes, ActionHostError> {
        let providers = self
            .ask(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            })
            .await?
            .providers;
        let mut entries = Vec::new();
        for provider in providers {
            // The workspace's own rows open applications, which the host
            // already lists; a host search here finds what is inside them.
            if workspace_provider_room(&provider.provider).is_some() {
                continue;
            }
            let budget = MAX_ENTRIES.saturating_sub(entries.len());
            if budget == 0 {
                break;
            }
            entries.extend(self.walk(&provider.provider, budget).await?);
        }
        Ok(self.index().replace(entries))
    }

    /// Up to `budget` of one publisher's resources, breadth first, each
    /// with its best opening action.
    async fn walk(
        &self,
        provider: &str,
        budget: usize,
    ) -> Result<Vec<SearchEntry>, ActionHostError> {
        let mut found: Vec<(u8, SearchEntry)> = Vec::new();
        let mut positions: HashMap<EntryId, usize> = HashMap::new();
        let mut parents = VecDeque::from([(None::<String>, 0_usize)]);
        let mut seen = HashSet::new();
        let mut pages = 0_usize;
        'walk: while let Some((parent, depth)) = parents.pop_front() {
            let mut next: Option<CataloguePage> = None;
            loop {
                if pages >= MAX_PAGES || found.len() >= budget {
                    break 'walk;
                }
                let page = self
                    .ask(CatalogueQuery {
                        provider: Some(provider.to_owned()),
                        parent_resource: parent.clone(),
                        revision: next.as_ref().map(|page| page.revision.clone()),
                        cursor: next.map(|page| page.next_cursor).unwrap_or_default(),
                        max_results: PAGE,
                        limits: Some(LIMITS),
                        ..CatalogueQuery::default()
                    })
                    .await?;
                pages = pages.saturating_add(1);
                if page.stale_cursor {
                    return Err(ActionHostError::Unavailable {
                        reason: "the catalogue changed during the walk".into(),
                    });
                }
                for row in &page.rows {
                    if row.has_children && depth < MAX_DEPTH && seen.insert(row.resource.id.clone())
                    {
                        parents.push_back((Some(row.resource.id.clone()), depth.saturating_add(1)));
                    }
                    let Some((rank, entry)) = opening(row) else {
                        continue;
                    };
                    match positions.entry(entry.id) {
                        Entry::Occupied(at) => {
                            if let Some(held) = found.get_mut(*at.get())
                                && held.0 < rank
                            {
                                *held = (rank, entry);
                            }
                        }
                        Entry::Vacant(at) => {
                            at.insert(found.len());
                            found.push((rank, entry));
                        }
                    }
                }
                if page.next_cursor.is_empty() {
                    break;
                }
                next = Some(page);
            }
        }
        found.truncate(budget);
        Ok(found.into_iter().map(|(_, entry)| entry).collect())
    }

    /// The best `limit` indexed entries for `query`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchEntry> {
        self.index()
            .search(query, limit)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The indexed entry a host search names by `id`.
    pub fn entry(&self, id: &str) -> Option<SearchEntry> {
        let id = id.parse().ok()?;
        self.index().get(id).cloned()
    }

    /// Open the resource a host search names by `id`, as this runtime's
    /// owner, through the action host.
    ///
    /// # Errors
    ///
    /// [`OpenError::NotOffered`] for an id not indexed, or a resource the
    /// catalogue no longer offers with a quiet opening action; otherwise
    /// the host's refusal or the action's failure.
    pub async fn open(&self, id: &str) -> Result<(), OpenError> {
        let entry = self.entry(id).ok_or(OpenError::NotOffered)?;
        let revision = self
            .ask(CatalogueQuery {
                provider: Some(entry.target.provider.clone()),
                max_results: 1,
                limits: Some(LIMITS),
                ..CatalogueQuery::default()
            })
            .await?
            .revision;
        let resolved = self
            .ask(CatalogueQuery::resolve(&entry.target, revision))
            .await?;
        let still_opens = !resolved.stale_cursor
            && resolved
                .rows
                .first()
                .and_then(opening)
                .is_some_and(|(_, current)| current.id == entry.id);
        if !still_opens {
            return Err(OpenError::NotOffered);
        }
        let receipt = answer(self.host.invoke(
            &self.caller,
            InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: entry.target,
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some(resolved.revision),
                fresh_binding: None,
                wait_ms: None,
            },
        ))
        .await??;
        match receipt.status {
            ReceiptStatus::Accepted | ReceiptStatus::Completed { .. } => Ok(()),
            ReceiptStatus::Rejected { error } => Err(OpenError::Host(error)),
            ReceiptStatus::Failed { error } => Err(OpenError::Failed(error)),
            ReceiptStatus::Cancelled { .. } => Err(OpenError::NotOffered),
        }
    }

    async fn ask(&self, query: CatalogueQuery) -> Result<CataloguePage, ActionHostError> {
        answer(self.host.query(&self.caller, query)).await?
    }
}

/// A host answer within [`ANSWER`], or why there is none.
async fn answer<T>(pending: tilo_actions::remote::Pending<T>) -> Result<T, ActionHostError> {
    match tokio::time::timeout(ANSWER, pending).await {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(ActionHostError::Unavailable {
            reason: "the action host dropped the answer".into(),
        }),
        Err(_) => Err(ActionHostError::Unavailable {
            reason: "the action host did not answer in time".into(),
        }),
    }
}
