//! Paging a provider that answers in one piece.
//!
//! A provider on the default [`SearchProvider::query_page`] answers with
//! [`SearchProvider::query`]: every row at once. Pages of it come two ways.
//! A direct caller (a provider's own `resolve`, the launcher) gets
//! [`unpaged_page`]: each page asks `query()` again and continues only while
//! the rows are the ones the cursor was cut from. A listing the registry
//! pages gets [`HeldSnapshots`]: `query()` once, on the listing's first page,
//! held for as long as its cursor, so the listing reads one consistent
//! snapshot however often the provider's state moves between two pages.
//! Changes show up on the next listing.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use super::{SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider, SearchResult};

/// What a registry-held cursor's token starts with.
const HELD: &str = "held:";

/// How many snapshots are held at once, beyond their first page. A listing
/// that pages to its end lets its snapshot go; this bounds the ones callers
/// left behind (a palette closed mid-list), oldest first.
const HELD_SNAPSHOTS: usize = 64;

/// The rows of `rows` a page starting at `offset` holds under `budget`: at
/// least one, so a page always moves on, and never past the byte budget
/// after the first.
fn page_end(rows: &[SearchResult], offset: usize, budget: SearchBudget) -> usize {
    let mut end = offset;
    let mut bytes = 0usize;
    while let Some(row) = rows.get(end) {
        let size = super::estimated_result_bytes(row);
        if end > offset
            && (end.saturating_sub(offset) >= budget.max_results.max(1)
                || bytes.saturating_add(size) > budget.max_bytes)
        {
            break;
        }
        bytes = bytes.saturating_add(size);
        end = end.saturating_add(1);
    }
    end
}

/// An identity for one answer: its rows' addresses, in order.
fn fingerprint(rows: &[SearchResult]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    for row in rows {
        row.provider_id.hash(&mut hasher);
        row.id.hash(&mut hasher);
    }
    hasher.finish()
}

/// One page of `rows`, a provider's whole answer, at `cursor`. The cursor
/// names an offset into one answer; a continuation is answered while the
/// rows are still that answer and is stale once they are not.
pub(crate) fn unpaged_page(
    revision: String,
    mut rows: Vec<SearchResult>,
    cursor: Option<&SearchCursor>,
    budget: SearchBudget,
) -> SearchPage {
    let identity = fingerprint(&rows);
    let offset = match cursor {
        None => 0,
        Some(cursor) => match cursor
            .token
            .split_once('.')
            .and_then(|(offset, listing)| {
                Some((
                    offset.parse::<usize>().ok()?,
                    u64::from_str_radix(listing, 16).ok()?,
                ))
            })
            .filter(|(offset, listing)| *listing == identity && *offset <= rows.len())
        {
            Some((offset, _)) => offset,
            None => return SearchPage::stale(revision),
        },
    };
    let end = page_end(&rows, offset, budget);
    let more = end < rows.len();
    rows.truncate(end);
    rows.drain(..offset);
    SearchPage::continuing(
        revision,
        rows,
        more.then(|| format!("{end}.{identity:016x}")),
    )
}

struct Held {
    id: u64,
    provider: String,
    revision: String,
    rows: Rc<Vec<SearchResult>>,
}

/// The snapshots listings page through, one per provider answering in one
/// piece, each held from its listing's first page until its last.
#[derive(Default)]
pub(super) struct HeldSnapshots {
    next: Cell<u64>,
    held: RefCell<VecDeque<Held>>,
}

impl HeldSnapshots {
    /// One page of `provider` for a caller that may follow its cursor
    /// (`hold`) or will not. A provider paging its own index is asked
    /// directly; one answering in one piece is paged from the snapshot its
    /// listing's first page took.
    pub(super) fn page(
        &self,
        provider: &dyn SearchProvider,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
        hold: bool,
    ) -> SearchPage {
        let held_cursor = cursor.and_then(|cursor| cursor.token.strip_prefix(HELD));
        if provider.pages_itself() || (held_cursor.is_none() && (cursor.is_some() || !hold)) {
            return provider.query_page(ctx, cursor, budget);
        }
        let (id, revision, rows, offset) = match held_cursor {
            None => {
                let revision = provider.search_revision(&ctx);
                let rows = Rc::new(provider.query(ctx));
                let id = self.next.get();
                self.next.set(id.wrapping_add(1));
                (id, revision, rows, 0)
            }
            Some(token) => {
                let Some((id, offset)) = token.split_once(':').and_then(|(id, offset)| {
                    Some((id.parse::<u64>().ok()?, offset.parse::<usize>().ok()?))
                }) else {
                    return SearchPage::stale(provider.search_revision(&ctx));
                };
                let found = self
                    .held
                    .borrow()
                    .iter()
                    .find(|snapshot| snapshot.id == id && snapshot.provider == provider.id())
                    .filter(|snapshot| offset <= snapshot.rows.len())
                    .map(|snapshot| (snapshot.revision.clone(), Rc::clone(&snapshot.rows)));
                let Some((revision, rows)) = found else {
                    return SearchPage::stale(provider.search_revision(&ctx));
                };
                (id, revision, rows, offset)
            }
        };
        let end = page_end(&rows, offset, budget);
        let page = rows.get(offset..end).unwrap_or_default().to_vec();
        let mut snapshots = self.held.borrow_mut();
        if end >= rows.len() {
            snapshots.retain(|snapshot| snapshot.id != id);
            return SearchPage::exhausted(revision, page);
        }
        if !snapshots.iter().any(|snapshot| snapshot.id == id) {
            if snapshots.len() >= HELD_SNAPSHOTS {
                snapshots.pop_front();
            }
            snapshots.push_back(Held {
                id,
                provider: provider.id().to_owned(),
                revision: revision.clone(),
                rows,
            });
        }
        drop(snapshots);
        SearchPage::continuing(revision, page, Some(format!("{HELD}{id}:{end}")))
    }
}
