//! What this page's person is doing, as the host it joined reads it: their own steps in the active
//! workspace's records since a moment — titles only, never content.
//!
//! The step's action is named without the item it touched, so a row's or a paragraph's words never
//! leave the page, and a part of a window's tree is named only by the place its app recorded as
//! shared (a post's channel), never by a private one.

use std::collections::{HashMap, HashSet};

use tilo_desktop_model::{DesktopTree, TabId, TileContent, Workspace, WorkspaceSlot};
use tilo_sort::CompactSort as _;
use ui::automerge::{AutoCommit, transaction::Transactable as _};
use ui::record::RecordCheckout;

/// The most of its own steps a page reports as what it is doing.
const MAX_OWN_ACTIONS: usize = 16;
/// The bound, in characters, of each title a page reports.
const MAX_OWN_TITLE_CHARS: usize = 96;

/// One step this page's person took, as "who is here" reports it: the titles of what they did, the
/// record and the app, never the content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OwnAction {
    /// The step's history entry id, 32 hex.
    pub id: String,
    /// What the step did, e.g. "Edited the text".
    pub action: String,
    /// The record's title.
    pub resource: String,
    /// The app's name.
    pub app: String,
    pub at_ms: u64,
}

/// What `author` did in the records the active workspace's windows show since `since_ms` (Unix
/// milliseconds), oldest first and at most [`MAX_OWN_ACTIONS`].
pub(crate) fn own_recent_actions(
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
    author: &str,
    since_ms: u64,
) -> Vec<OwnAction> {
    let titles: HashMap<TabId, &str> = tree
        .tabs()
        .map(|tab| (tab.id, tab.title.as_str()))
        .collect();
    let mut authors = ui::AuthorDirectory::new();
    // Named by principal, as `author` is: whose steps they are is all
    // this asks.
    crate::page_authors::register(&mut authors, &[], &|_| None);
    authors.set_reader(author);
    let mut actions = Vec::new();
    let bound = bound(shared, tree, tree.active);
    // A window's record and the records of its trees the app keeps open
    // beside it: a chat's posts are in the part of its messages they went
    // to, not in the chat's own record. Each tree record counts for the
    // first window of its app.
    let mut read: Vec<(TabId, String, ui::record::RecordId)> = bound
        .iter()
        .map(|(tab, app, checkout)| (*tab, app.clone(), checkout.id.clone()))
        .collect();
    let mut apps_seen = HashSet::new();
    for (tab, app, _) in &bound {
        if !apps_seen.insert(app.clone()) {
            continue;
        }
        for record in tree_records(shared, app) {
            if !read.iter().any(|(_, a, id)| a == app && *id == record) {
                read.push((*tab, app.clone(), record));
            }
        }
    }
    for (tab, app, record) in read {
        let Some(mut doc) = read_record(shared, &app, &record, |doc| {
            (doc.pending_ops() == 0).then(|| doc.clone())
        })
        .flatten() else {
            continue;
        };
        let entries = ui::HistorySource::new()
            .without_record_genesis()
            .with_authors(authors.clone())
            .with_max_entries(MAX_OWN_ACTIONS)
            .entries(&mut doc);
        let app_name = crate::desktop::known_application(&app).unwrap_or_default();
        // A record of a window's tree is part of what that window shows,
        // and is named as the window is.
        let in_tree = record.tree().is_some();
        for entry in entries {
            let mine = entry.author.relation == ui::widget::AuthorRelation::Reader
                || entry.author.name == author;
            let at_ms = entry
                .timestamp
                .duration_since(ui::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            if !mine || at_ms < since_ms || !is_doing(&entry) {
                continue;
            }
            let (action, resource) = match &entry.context {
                Some(context) => (
                    ui::history_source::sentence::step_title(context),
                    if in_tree {
                        shared_part(context)
                    } else {
                        context.resource.as_ref().map(|r| r.label.clone())
                    },
                ),
                None => (None, None),
            };
            let action = bounded_title(&action.unwrap_or_else(|| "Edited".to_owned()));
            if action.is_empty() {
                continue;
            }
            let resource = resource
                .filter(|label| !label.trim().is_empty())
                .or_else(|| titles.get(&tab).map(|title| (*title).to_owned()))
                .unwrap_or_default();
            actions.push(OwnAction {
                id: hex::encode(entry.id.0),
                action,
                resource: bounded_title(&resource),
                app: bounded_title(app_name),
                at_ms,
            });
        }
    }
    actions.compact_sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.cmp(&b.id)));
    actions.dedup_by(|a, b| a.id == b.id);
    // Typing commits a step every second or so: a run of the same action on
    // the same resource is one thing the person did, named by its newest step.
    actions.reverse();
    actions.dedup_by(|older, newer| {
        older.action == newer.action && older.resource == newer.resource && older.app == newer.app
    });
    actions.reverse();
    if actions.len() > MAX_OWN_ACTIONS {
        actions.drain(..actions.len().saturating_sub(MAX_OWN_ACTIONS));
    }
    actions
}

fn bounded_title(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_OWN_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The part of a window's tree a step went to, named where everyone the
/// window is shared with sees it too: the place its app recorded on the
/// step ([`tilo_actions::PLACE_FACET`], a post's channel), or a channel
/// it was done to, as `#general`. A private part (a direct conversation)
/// is never named; the window's title stands in for it.
fn shared_part(context: &tilo_actions::ActionContext) -> Option<String> {
    if let Some(place) = context
        .facets
        .iter()
        .find(|facet| facet.key == tilo_actions::PLACE_FACET)
        .map(|facet| facet.value.trim())
        .filter(|place| !place.is_empty())
    {
        return Some(place.to_owned());
    }
    let resource = context.resource.as_ref()?;
    let label = resource.label.trim();
    if label.is_empty() || !resource.kind.ends_with(".channel") {
        return None;
    }
    Some(if label.starts_with('#') {
        label.to_owned()
    } else {
        format!("#{label}")
    })
}

/// Whether `entry` is something its author did, as workspace activity
/// counts it: housekeeping an app does on its own (introducing the person
/// to a chat under the name they arrived with, a read marker) and content
/// from outside are not what someone is doing.
fn is_doing(entry: &ui::widget::HistoryEntry) -> bool {
    entry
        .context
        .as_ref()
        .is_none_or(tilo_actions::ActionContext::is_activity)
}

/// `workspace`'s windows and the records they show: the first window of
/// each record, its app and the record.
fn bound(
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
    workspace: WorkspaceSlot,
) -> Vec<(TabId, String, RecordCheckout)> {
    let apps: HashMap<TabId, String> = tree
        .workspaces
        .iter()
        .filter(|w| w.slot == workspace)
        .flat_map(Workspace::tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.clone())),
            _ => None,
        })
        .collect();
    let mut seen = HashSet::new();
    let mut bound: Vec<(TabId, String, RecordCheckout)> = shared
        .bound_records()
        .into_iter()
        .filter_map(|(tab, checkout)| Some((tab, apps.get(&tab)?.clone(), checkout)))
        .filter(|(_, app, checkout)| seen.insert((app.clone(), checkout.id.clone())))
        .collect();
    bound.compact_sort_by(|a, b| (&a.1, a.2.id.as_str()).cmp(&(&b.1, b.2.id.as_str())));
    bound
}

fn read_record<R>(
    shared: &crate::desktop::Shared,
    app: &str,
    record: &ui::record::RecordId,
    read: impl FnOnce(&AutoCommit) -> R,
) -> Option<R> {
    let stores = shared.stores.borrow();
    let store = stores
        .iter()
        .find(|store| store.package_and_home().0 == app)?;
    store.read(record, read)
}

/// The tree records ([`ui::record::RecordId::tree`]) `app`'s store has open.
fn tree_records(shared: &crate::desktop::Shared, app: &str) -> Vec<ui::record::RecordId> {
    let stores = shared.stores.borrow();
    stores
        .iter()
        .find(|store| store.package_and_home().0 == app)
        .map(ui::record::RecordStore::open_tree_records)
        .unwrap_or_default()
        .into_iter()
        .map(|checkout| checkout.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{is_doing, shared_part};
    use tilo_actions::{ActionContext, ChangeKind};
    use ui::widget::{ArtifactKind, EntryKind, HistoryAuthor, HistoryEntry, HistoryEntryId};

    fn entry(context: Option<ActionContext>) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([0; 16]),
            author: HistoryAuthor::new("Ada", 120.0, "deadbeef"),
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::Note,
            timestamp: ui::time::UNIX_EPOCH,
            summary: String::new(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context,
        }
    }

    /// What a person is doing, as the people here are told it, is their
    /// work: an app introducing them to a chat under the name they arrived
    /// with is its housekeeping, not something they did.
    #[test]
    fn housekeeping_and_outside_content_are_not_what_someone_is_doing() {
        assert!(is_doing(&entry(None)));
        assert!(is_doing(&entry(Some(ActionContext::default()))));
        for kind in [ChangeKind::Housekeeping, ChangeKind::External] {
            assert!(
                !is_doing(&entry(Some(ActionContext::default().with_change(kind)))),
                "{kind:?}"
            );
        }
    }

    /// A step in a window's tree is named by the place its app recorded
    /// (a post's channel), else a channel it was done to; anything else
    /// (a direct conversation) is not named at all.
    #[test]
    fn a_step_is_named_by_the_shared_place_it_went_to() {
        let post = ActionContext::for_resource("chain", "chat.messages", "Messages").with_facet(
            tilo_actions::ActionFacet::new(tilo_actions::PLACE_FACET, "#general"),
        );
        assert_eq!(shared_part(&post).as_deref(), Some("#general"));
        let channel = ActionContext::for_resource("c1", "chat.channel", "design");
        assert_eq!(shared_part(&channel).as_deref(), Some("#design"));
        let direct = ActionContext::for_resource("chain", "chat.messages", "Messages");
        assert_eq!(shared_part(&direct), None);
    }
}
