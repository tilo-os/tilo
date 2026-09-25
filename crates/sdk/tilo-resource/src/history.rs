//! Version history of any resource, derived from its document's change graph:
//! who changed it, when, through which action — and what it looked like then.
//!
//! Nothing here is specific to a resource type. The attribution comes from
//! the executor (the change's actor is the author, its message carries the
//! action's [`ActionContext`]); materialising a past version is
//! [`Resource::read`] on the document forked at that version's heads.

use std::collections::{BinaryHeap, HashMap};

use automerge::ChangeHash;
use tilo_actions::ActionContext;

use crate::model::{Doc, ModelError};
use crate::resource::Resource;

/// One change on a resource's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub hash: ChangeHash,
    /// Hex of the authoring actor.
    pub actor: String,
    /// Milliseconds since the Unix epoch, or 0 when the change carries no time.
    pub time_ms: i64,
    /// What the change did, when it was made through an action.
    pub context: Option<ActionContext>,
    /// The raw change message.
    pub message: Option<String>,
}

impl Version {
    /// A one-line human description: the action's label when known.
    pub fn summary(&self) -> Option<String> {
        self.context
            .as_ref()
            .and_then(ActionContext::summary)
            .or_else(|| self.message.clone())
    }
}

fn version_of(meta: &automerge::ChangeMetadata<'_>) -> Version {
    let message = meta.message.as_deref().map(str::to_owned);
    Version {
        hash: meta.hash,
        actor: meta.actor.to_hex_string(),
        time_ms: meta.timestamp.saturating_mul(1000),
        context: message
            .as_deref()
            .and_then(ActionContext::from_change_message),
        message,
    }
}

/// Every change in causal order, oldest first.
///
/// Reads every change's metadata (never its operations). For a long history
/// shown a screen at a time, [`page`] reads only the page.
pub fn versions(doc: &mut Doc) -> Vec<Version> {
    doc.get_changes_meta(&[]).iter().map(version_of).collect()
}

/// Where a newest-first walk of the history stopped: hand it back to [`page`]
/// for the next older page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The newest changes not listed yet, by operation counter: every change
    /// they depend on is older than all of them.
    frontier: Vec<(Rank, ChangeHash)>,
}

/// One page of history, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub versions: Vec<Version>,
    /// The rest of the history, or `None` when this page reached the start.
    pub older: Option<Cursor>,
}

/// Up to `limit` versions, newest first, starting at the document's heads
/// or where `from` stopped.
///
/// A page reads only the changes it lists and their direct dependencies, so
/// a participant who joins a document with years of history sees its latest
/// changes at once, and older ones load as they scroll back. Every change is
/// listed once, after every change that depends on it: a change's operation
/// counters are greater than those of anything it depends on, so taking the
/// frontier's greatest counter first is a reverse causal order.
pub fn page(doc: &mut Doc, from: Option<&Cursor>, limit: usize) -> Page {
    let mut walk = Walk::default();
    let mut queue: BinaryHeap<(Rank, ChangeHash)> = if let Some(cursor) = from {
        cursor.frontier.iter().copied().collect()
    } else {
        let heads = doc.get_heads();
        heads
            .into_iter()
            .filter_map(|hash| walk.rank(doc, hash).map(|rank| (rank, hash)))
            .collect()
    };
    let mut versions = Vec::new();
    let mut listed = std::collections::HashSet::new();
    while versions.len() < limit {
        let Some((_, hash)) = queue.pop() else { break };
        // A change reached along two paths is queued once per path.
        if !listed.insert(hash) || walk.rank(doc, hash).is_none() {
            continue;
        }
        let Some((_, deps, version)) = walk.metas.get(&hash).cloned() else {
            continue;
        };
        for dep in deps {
            if let Some(rank) = walk.rank(doc, dep) {
                queue.push((rank, dep));
            }
        }
        versions.push(version);
    }
    let mut frontier: Vec<(Rank, ChangeHash)> = queue.into_vec();
    frontier.sort_unstable();
    frontier.dedup();
    frontier.retain(|(_, hash)| !listed.contains(hash));
    Page {
        versions,
        older: (!frontier.is_empty()).then_some(Cursor { frontier }),
    }
}

/// A change's place in the newest-first walk: its last operation counter,
/// then its first. An empty change ends at its dependency's last counter but
/// starts after it, so the start breaks that tie.
type Rank = (u64, u64);

/// The change metadata one [`page`] call has read.
#[derive(Default)]
struct Walk {
    metas: HashMap<ChangeHash, (Rank, Vec<ChangeHash>, Version)>,
}

impl Walk {
    fn rank(&mut self, doc: &mut Doc, hash: ChangeHash) -> Option<Rank> {
        if let Some((rank, _, _)) = self.metas.get(&hash) {
            return Some(*rank);
        }
        let meta = doc.get_change_meta_by_hash(&hash)?;
        let rank = (meta.max_op, meta.start_op);
        let entry = (rank, meta.deps.clone(), version_of(&meta));
        self.metas.insert(hash, entry);
        Some(rank)
    }
}

/// The resource as it was at `heads`.
///
/// # Errors
/// Returns [`ModelError`] when that version does not hold this resource.
pub fn read_at<R: Resource>(doc: &mut Doc, heads: &[ChangeHash]) -> Result<R::State, ModelError> {
    let past = doc.fork_at(heads)?;
    R::read(&past)
}
