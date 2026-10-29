//! An application's action directory, without the toolkit: the
//! [`ActionDirectorySnapshot`] a publication of resources and actions builds,
//! the bounded pages of it the broker asks for (a level, a named resource, or
//! the actions a search names) and the exact row it resolves before an
//! invocation.
//!
//! The toolkit's app framework (ui-core) builds its snapshot here and pages
//! it, extending it with what only its widgets can say; an app host that
//! publishes whole resource trees, as `tilolang-host` does, answers its
//! connection's directory with a [`PublishedDirectory`] alone.
mod effective;
mod snapshot;

pub use snapshot::{ActionDirectorySnapshot, DirectoryNode, exported_directory_action};

use runtime_collab::action_directory_row_wire_bytes;
pub use runtime_collab::{
    ActionDirectoryPage, ActionDirectoryQuery, ActionDirectoryResolution, ActionDirectoryRow,
};
use tilo_actions::ActionAddress;

/// The most rows one page carries.
pub const MAX_RESULTS: usize = 128;
/// The most candidates one page inspects.
pub const MAX_WORK: usize = 512;
/// The most bytes one page's rows take on the wire.
pub const MAX_BYTES: usize = 64 * 1024;
/// The deepest level a directory discloses.
pub const MAX_DEPTH: u16 = 16;
/// What a page takes on the wire besides its rows.
pub const PAGE_WIRE_BYTES: usize = 512;

/// The page that answers nothing: the directory cannot be read now.
#[must_use]
pub fn unavailable_page(query: &ActionDirectoryQuery) -> ActionDirectoryPage {
    ActionDirectoryPage {
        revision: String::new(),
        rows: Vec::new(),
        next_cursor: Vec::new(),
        exhausted: false,
        stale_cursor: !query.revision.is_empty(),
        budget_exhausted: true,
    }
}

/// The page that says the caller's revision or cursor is no longer current.
#[must_use]
pub fn stale_page(revision: String) -> ActionDirectoryPage {
    ActionDirectoryPage {
        revision,
        rows: Vec::new(),
        next_cursor: Vec::new(),
        exhausted: false,
        stale_cursor: true,
        budget_exhausted: false,
    }
}

fn clamp(value: u32, max: usize) -> usize {
    usize::try_from(value).unwrap_or(max).clamp(1, max)
}

/// Whether a resource of `kind` labelled `label` is the one `name` names: by
/// its kind exactly, or by its label ignoring ASCII case.
#[must_use]
pub fn is_named(kind: &str, label: &str, name: &str) -> bool {
    kind == name || label.eq_ignore_ascii_case(name)
}

/// The resource rows of `snapshot` that `name` names: at its top level for an
/// empty `parent`, else on `parent`'s level. `None` when the snapshot does
/// not hold `parent`.
#[must_use]
pub fn named_rows(
    snapshot: &ActionDirectorySnapshot,
    parent: &str,
    name: &str,
) -> Option<Vec<ActionDirectoryRow>> {
    let rows = if parent.is_empty() {
        &snapshot.roots
    } else {
        &snapshot.nodes.get(parent)?.rows
    };
    Some(
        rows.iter()
            .filter(|row| {
                row.action.is_none() && is_named(&row.resource_kind, &row.resource_label, name)
            })
            .cloned()
            .collect(),
    )
}

/// The one page a named lookup answers with: every row `found` holds, up to
/// the query's budgets, or none when they do not fit.
#[must_use]
pub fn named_page(
    query: &ActionDirectoryQuery,
    revision: String,
    found: Vec<ActionDirectoryRow>,
) -> ActionDirectoryPage {
    let max_results = clamp(query.max_results, MAX_RESULTS);
    let max_bytes = clamp(query.max_bytes, MAX_BYTES);
    let mut rows = Vec::new();
    let mut bytes = PAGE_WIRE_BYTES.saturating_add(revision.len());
    for mut row in found.into_iter().take(max_results) {
        row.has_children &= query.max_depth > 0;
        bytes = bytes.saturating_add(action_directory_row_wire_bytes(&row));
        if bytes > max_bytes {
            return ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: false,
                budget_exhausted: true,
            };
        }
        rows.push(row);
    }
    ActionDirectoryPage {
        revision,
        rows,
        next_cursor: Vec::new(),
        exhausted: true,
        stale_cursor: false,
        budget_exhausted: false,
    }
}

/// Whether the action `row` carries is one a search for `needle` (already
/// lowercase) finds: by its id, label, description, parameters' names and
/// descriptions, or its resource's label.
#[must_use]
pub fn action_row_matches(row: &ActionDirectoryRow, needle: &str) -> bool {
    let Some(action) = &row.descriptor else {
        return false;
    };
    let contains = |text: &str| text.to_lowercase().contains(needle);
    contains(&action.action)
        || contains(&action.action_label)
        || action.description.as_deref().is_some_and(contains)
        || contains(&action.resource_label)
        || action
            .params
            .iter()
            .any(|param| contains(&param.name) || contains(&param.description))
}

/// The directory of a host that publishes its whole resource tree at once:
/// the snapshot of what it last published, under a revision that changes
/// whenever that does. Pages continue from a cursor that names the revision,
/// the request and the offset, so it holds nothing per caller.
#[derive(Default)]
pub struct PublishedDirectory {
    published: Option<(String, ActionDirectorySnapshot)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cursor {
    revision: String,
    needle: String,
    parent: String,
    offset: usize,
}

impl PublishedDirectory {
    /// Take `resources`, whose actions `descriptors` describe, as what the
    /// directory answers from. Its revision moves only when the directory
    /// does.
    pub fn publish(
        &mut self,
        resources: &[tilo_actions::ResourceSpec],
        descriptors: &[tilo_actions::ActionDescriptor],
    ) {
        let snapshot = ActionDirectorySnapshot::build(resources, descriptors, |_| None);
        if self
            .published
            .as_ref()
            .is_some_and(|(_, held)| *held == snapshot)
        {
            return;
        }
        self.published = Some((format!("v1:{}", uuid::Uuid::new_v4()), snapshot));
    }

    /// Answer nothing until the next publication.
    pub fn withdraw(&mut self) {
        self.published = None;
    }

    /// The revision the directory answers at, while it answers.
    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        self.published
            .as_ref()
            .map(|(revision, _)| revision.as_str())
    }

    /// The page `query` asks for.
    #[must_use]
    #[allow(
        clippy::indexing_slicing,
        reason = "every index is the loop's offset, checked against the candidates' length"
    )]
    pub fn page(&self, query: &ActionDirectoryQuery) -> ActionDirectoryPage {
        let Some((revision, snapshot)) = &self.published else {
            return unavailable_page(query);
        };
        if !query.revision.is_empty() && query.revision != *revision {
            return stale_page(revision.clone());
        }
        if !query.resource_name.is_empty() {
            let found = named_rows(snapshot, &query.parent_resource, &query.resource_name)
                .unwrap_or_default();
            return named_page(query, revision.clone(), found);
        }
        let offset = if query.cursor.is_empty() {
            0
        } else {
            match serde_json::from_slice::<Cursor>(&query.cursor) {
                Ok(cursor)
                    if cursor.revision == *revision
                        && cursor.needle == query.needle
                        && cursor.parent == query.parent_resource =>
                {
                    cursor.offset
                }
                _ => return stale_page(revision.clone()),
            }
        };
        let max_results = clamp(query.max_results, MAX_RESULTS);
        let max_work = clamp(query.max_work, MAX_WORK);
        let max_bytes = clamp(query.max_bytes, MAX_BYTES);
        let max_depth = query.max_depth.min(MAX_DEPTH);
        let needle = query.needle.to_lowercase();
        // The candidates, by offset: a level's rows, or for a search every
        // action in tree order, of which the matching ones answer.
        let level = if !needle.is_empty() {
            None
        } else if query.parent_resource.is_empty() {
            Some((snapshot.roots.as_slice(), None))
        } else {
            match snapshot.nodes.get(&query.parent_resource) {
                Some(node) => Some((node.rows.as_slice(), Some(node.depth))),
                None => Some((&[][..], None)),
            }
        };
        let total = level.map_or(snapshot.order.len(), |(rows, _)| rows.len());
        let mut rows = Vec::new();
        let mut bytes = PAGE_WIRE_BYTES.saturating_add(revision.len());
        let mut offset = offset;
        let mut inspected = 0usize;
        let mut byte_exhausted = false;
        while offset < total && rows.len() < max_results && inspected < max_work {
            let row = match level {
                Some((level, depth)) => {
                    let mut row = level[offset].clone();
                    row.has_children &= max_depth > 0
                        && depth
                            .is_none_or(|depth| depth.saturating_add(1) < usize::from(MAX_DEPTH));
                    Some(row)
                }
                None => snapshot
                    .actions
                    .get(&snapshot.order[offset])
                    .filter(|row| action_row_matches(row, &needle))
                    .cloned(),
            };
            if let Some(row) = row {
                let next = bytes.saturating_add(action_directory_row_wire_bytes(&row));
                if next > max_bytes {
                    byte_exhausted = true;
                    break;
                }
                bytes = next;
                rows.push(row);
            }
            offset = offset.saturating_add(1);
            inspected = inspected.saturating_add(1);
        }
        let exhausted = offset >= total;
        // Keep the cursor before a row that did not fit when earlier rows
        // did: a caller that raises its byte budget resumes at that row.
        let next_cursor = if exhausted || (byte_exhausted && rows.is_empty()) {
            Vec::new()
        } else {
            serde_json::to_vec(&Cursor {
                revision: revision.clone(),
                needle: query.needle.clone(),
                parent: query.parent_resource.clone(),
                offset,
            })
            .unwrap_or_default()
        };
        ActionDirectoryPage {
            revision: revision.clone(),
            rows,
            next_cursor,
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }

    /// The row of `resource`'s action `action`, at `revision`, or at the
    /// current revision for an empty one.
    #[must_use]
    pub fn resolve(
        &self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> ActionDirectoryResolution {
        let Some((current, snapshot)) = &self.published else {
            return ActionDirectoryResolution::Unavailable;
        };
        if !revision.is_empty() && revision != current {
            return ActionDirectoryResolution::StaleRevision;
        }
        snapshot
            .actions
            .get(&ActionAddress::new(resource, action))
            .map_or(ActionDirectoryResolution::NotFound, |row| {
                ActionDirectoryResolution::Found {
                    row: Box::new(row.clone()),
                    revision: Some(current.clone()),
                }
            })
    }

    /// Answer `request`. A host that publishes this way pins no revision
    /// for local input, so there is no local mutation for it to explain.
    pub fn answer(&self, request: crate::ActionDirectoryRequest) {
        match request {
            crate::ActionDirectoryRequest::ResolveLocalDocumentAction { reply, .. } => {
                let _ = reply.send(ActionDirectoryResolution::StaleRevision);
            }
            crate::ActionDirectoryRequest::Query { query, reply } => {
                if !reply.is_closed() {
                    let _ = reply.send(self.page(&query));
                }
            }
            crate::ActionDirectoryRequest::Resolve {
                revision,
                resource,
                action,
                reply,
            } => {
                let _ = reply.send(self.resolve(&revision, &resource, &action));
            }
        }
    }
}

#[cfg(test)]
#[path = "action_directory_tests.rs"]
mod tests;
