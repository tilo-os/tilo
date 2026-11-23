//! What a find's caller may read, kept while the workspace stays as it was.
//!
//! Deciding a caller's access is a read the device answers per document, and a workspace of a
//! hundred notes paid it a hundred times before ranking a query: the answer was what the caller
//! waited on. So a find ranks first, asks what the caller may read only for the answers it
//! returns, and keeps each answer per caller until a write lands in the workspace. A grant can
//! also lapse with time, which lands nothing, so what is kept is read again after
//! [`FRESHNESS`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::Found;

use super::bounded::Bounded;
use super::{ActionKind, Answer, DocumentRoute, TreePath};

/// How long what a caller may read answers a find before it is read again: a grant can lapse
/// with time and not with a write, and nothing then says so here.
pub(super) const FRESHNESS: Duration = Duration::from_secs(30);

/// How much of one resource a search's caller may read.
#[derive(Clone)]
pub(super) enum Readable {
    /// All of it.
    Whole,
    /// Only the items their permissions name, each decided for itself.
    Items(Box<ui::permissions::PermissionContext>),
}

impl Readable {
    /// Whether the caller may read `resource`, which the resource at `route` holds.
    pub(super) fn reads(&self, route: &DocumentRoute, resource: &str) -> bool {
        match self {
            Self::Whole => true,
            Self::Items(permissions) => item_node(route, resource)
                .is_some_and(|node| permissions.answer(&node, ActionKind::Read) == Answer::Allow),
        }
    }
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

/// Who an answer kept here is about: the person on the device they speak through, and the agent
/// acting for them, as the workspace's policy decides them. A turn neither changes who they are
/// nor what they may read of a document the workspace has not changed; an agent inside a turn and
/// the same agent outside one act on different grants, so the two are told apart. So are turns
/// held to different rules, and a turn that answers one person through another's agent, which
/// both people bound: each may read what the others may not.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Reader {
    person: ui::permissions::policy::PersonId,
    agent: Option<String>,
    on_turn: bool,
    user: Option<[u8; 32]>,
    device: Option<[u8; 32]>,
    people: Vec<String>,
    granted: Vec<ui::permissions::policy::Grant>,
}

impl Reader {
    pub(super) fn of(who: &Bounded) -> Self {
        let acting = who.caller.acting();
        Self {
            person: who.person.clone(),
            agent: acting.agent_id().map(str::to_owned),
            on_turn: who.caller.turn().is_some(),
            user: who.caller.user_public_key().copied(),
            device: who.caller.device_public_key().copied(),
            people: acting.people().into_iter().map(str::to_owned).collect(),
            granted: who.caller.granted(&[]),
        }
    }
}

/// What one caller may read of a workspace, as its policy last decided it.
#[derive(Default)]
pub(super) struct SearchAccess {
    read_at: Option<Instant>,
    /// Each document's answer; `None` for one the caller may not read at all.
    pub(super) routes: HashMap<DocumentRoute, Option<Readable>>,
    /// What a document's app lists at its top level, as a find last read it: what names a hit
    /// the app anchors on another resource (`#page` in a notebook).
    pub(super) listings: HashMap<DocumentRoute, Vec<super::CatalogueRow>>,
    /// The resource a hit at a document was named as, by the resource id the hit names.
    pub(super) named: HashMap<(DocumentRoute, String), Option<Found>>,
    /// What a document whose app this device holds no compiled-in kind for publishes to search
    /// through the host, read as this caller's person reads it: a tilolang app's record, whose
    /// entries differ by who reads them (a direct conversation's messages). `None` for a record
    /// the host reads no entries of.
    pub(super) published:
        HashMap<DocumentRoute, Option<Arc<[ui::search::index_publication::IndexEntry]>>>,
    /// Whether the caller may read the workspace's folder.
    pub(super) files: Option<bool>,
}

impl SearchAccess {
    /// Make what is kept answer for `now`, dropping it once it is older than [`FRESHNESS`].
    pub(super) fn fresh(&mut self) {
        let fresh = self.read_at.is_some_and(|at| at.elapsed() < FRESHNESS);
        if !fresh {
            self.read_at = Some(Instant::now());
            self.routes.clear();
            self.listings.clear();
            self.named.clear();
            self.published.clear();
            self.files = None;
        }
    }
}
