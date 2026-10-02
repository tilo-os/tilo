//! History for a desktop that is not kept as a room document.
//!
//! A host that saves (or replicates) its desktop as one JSON value keeps its
//! history in an Automerge document whose every change replaces that value
//! under [`DESKTOP_KEY`], stamped with when it was made and who made it
//! ([`record`]). The history pane reads room documents, so
//! [`room_history`] replays such a log into one: each change that moved a
//! window of the room becomes one change of the room's projection, by the
//! same actor at the same time, which the pane summarises ("opened",
//! "closed", "renamed") and replays exactly as it does a native room's.
use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ROOT, ReadDoc as _, ScalarValue, Value};
use tilo_desktop_model::projection::{EmptyStrips, project_owned_contents};
use tilo_desktop_model::{DesktopTree, RoomId, Workspace};

/// The key every change of a desktop log writes the desktop under.
pub const DESKTOP_KEY: &str = "desktop";
/// A change's message names its author after this prefix.
const AUTHOR_PREFIX: &str = "author:";
/// The newest changes a room history replays; older ones stay in the log.
pub const MAX_REPLAYED_CHANGES: usize = 600;

/// The desktop a log value carries: a bare tree, or `{ "tree": … }` with
/// the host's own fields beside it.
fn tree_of(value: &serde_json::Value) -> Option<DesktopTree> {
    let tree = value.get("tree").unwrap_or(value);
    serde_json::from_value(tree.clone()).ok()
}

/// What a change of the desktop is about: its windows, not which of them is
/// focused, so moving focus records nothing.
fn layout_of(value: &serde_json::Value) -> serde_json::Value {
    let mut value = value.clone();
    let tree = match value.get_mut("tree") {
        Some(tree) => tree,
        None => &mut value,
    };
    if let Some(tree) = tree.as_object_mut() {
        for key in ["active", "active_strip", "active_group"] {
            tree.remove(key);
        }
        if let Some(workspaces) = tree
            .get_mut("workspaces")
            .and_then(serde_json::Value::as_array_mut)
        {
            for workspace in workspaces {
                if let Some(strips) = workspace
                    .get_mut("strips")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for strip in strips {
                        if let Some(groups) = strip
                            .get_mut("window_groups")
                            .and_then(serde_json::Value::as_array_mut)
                        {
                            for group in groups {
                                if let Some(group) = group.as_object_mut() {
                                    group.remove("active_tab");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    value
}

fn text_at(doc: &AutoCommit, heads: Option<&[automerge::ChangeHash]>) -> Option<String> {
    let found = match heads {
        Some(heads) => doc.get_at(ROOT, DESKTOP_KEY, heads),
        None => doc.get(ROOT, DESKTOP_KEY),
    };
    match found.ok()?? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Write `value` to `log` as one change by `author` at `time` (seconds since
/// the epoch) when its windows differ from what the log holds. Returns
/// whether it wrote.
pub fn record(log: &mut AutoCommit, value: &serde_json::Value, author: &str, time: i64) -> bool {
    let current = text_at(log, None).and_then(|text| serde_json::from_str(&text).ok());
    if current.is_some_and(|current| layout_of(&current) == layout_of(value)) {
        return false;
    }
    if log.put(ROOT, DESKTOP_KEY, value.to_string()).is_err() {
        return false;
    }
    log.commit_with(
        CommitOptions::default()
            .with_message(format!("{AUTHOR_PREFIX}{author}"))
            .with_time(time),
    );
    true
}

/// Save a changed log through the host's store. A refusal keeps it pending,
/// including when the next attempt records no additional change.
///
/// # Errors
/// Returns the host's storage error without acknowledging the save.
pub fn save_pending<E>(
    log: &mut AutoCommit,
    pending: &std::cell::Cell<bool>,
    write: impl FnOnce(Vec<u8>) -> Result<(), E>,
) -> Result<(), E> {
    if pending.get() {
        write(log.save())?;
        pending.set(false);
    }
    Ok(())
}

/// The author a change of a desktop log names.
pub fn change_author(message: Option<&str>) -> Option<&str> {
    message?
        .strip_prefix(AUTHOR_PREFIX)
        .filter(|a| !a.is_empty())
}

/// A room document replayed from a desktop log, and who each of its actors
/// writes for.
pub struct RoomHistory {
    pub doc: AutoCommit,
    /// Older desktop changes remain retained but exceed the replay budget.
    pub truncated: bool,
    /// Actor (hex) to the author its changes named.
    pub authors: BTreeMap<String, String>,
}

/// Every salt the room's ids were minted under: each owns its part of the
/// projection.
fn salts(workspace: &Workspace) -> BTreeSet<u16> {
    let mut salts = BTreeSet::new();
    for strip in &workspace.strips {
        salts.insert(tilo_desktop_model::ids::peer_salt_of(strip.id.raw()));
        for group in &strip.window_groups {
            salts.insert(tilo_desktop_model::ids::peer_salt_of(group.id));
            for tab in &group.tabs {
                salts.insert(tilo_desktop_model::ids::peer_salt_of(tab.id));
            }
        }
    }
    salts.remove(&0);
    salts
}

/// Replay `log`'s newest [`MAX_REPLAYED_CHANGES`] changes into `room`'s
/// projection, in causal order. A change that did not move a window of the
/// room adds nothing.
pub fn room_history(mut log: AutoCommit, room: RoomId) -> RoomHistory {
    let mut doc = AutoCommit::new();
    let mut authors = BTreeMap::new();
    let changes = log.get_changes(&[]);
    let skip = changes.len().saturating_sub(MAX_REPLAYED_CHANGES);
    let mut declared = false;
    let mut projected: BTreeSet<u16> = BTreeSet::new();
    for change in changes.into_iter().skip(skip) {
        let Some(tree) = text_at(&log, Some(&[change.hash()]))
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| tree_of(&value))
        else {
            continue;
        };
        let Some(workspace) = tree.workspaces.iter().find(|w| w.room == room) else {
            continue;
        };
        let actor = change.actor_id().clone();
        if let Some(author) = change_author(change.message()) {
            authors
                .entry(actor.to_hex_string())
                .or_insert_with(|| author.to_owned());
        }
        doc.set_actor(actor);
        if !declared {
            let _ = doc.put(
                ROOT,
                tilo_desktop_model::projection::ROOM_ID_KEY,
                room.to_string(),
            );
            declared = true;
        }
        tilo_desktop_model::projection::put_str_if_changed(
            &mut doc,
            &ROOT,
            tilo_desktop_model::projection::ROOM_NAME_KEY,
            &workspace.name,
        );
        let _ = tilo_desktop_model::projection::project_workspace_tasks(&mut doc, &workspace.tasks);
        // A salt whose windows all closed still sweeps them away.
        let now = salts(workspace);
        for &salt in now.union(&projected) {
            let _ =
                project_owned_contents(&mut doc, &workspace.strips, salt, None, EmptyStrips::Skip);
        }
        projected.extend(now);
        let time = change.timestamp();
        doc.commit_with(CommitOptions::default().with_time(time));
    }
    RoomHistory {
        doc,
        authors,
        truncated: skip != 0,
    }
}

/// Name `authors`' actors in `directory`, as one participant per author.
pub fn register_authors(directory: &mut ui::AuthorDirectory, authors: &BTreeMap<String, String>) {
    for (actor, name) in authors {
        let identity = automerge::ActorId::from(name.as_bytes()).to_hex_string();
        directory.register(
            actor,
            ui::widget::HistoryAuthor::new(
                name.clone(),
                ui::history_source::stable_hue(&identity),
                ui::history_source::short_id(&identity),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivitySnapshot, RoomActivityPane};
    use tilo_automerge::ApplyIncoming as _;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};

    fn app(minter: &mut IdMinter, title: &str) -> Tab {
        Tab {
            id: minter.mint(),
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: format!("so.tilo.{}", title.to_lowercase()),
                label: title.into(),
                note: String::new(),
            },
        }
    }

    fn stored(tree: &DesktopTree) -> serde_json::Value {
        serde_json::json!({ "tree": tree, "records": [] })
    }

    /// A desktop opened with Notes, then Sheets opened beside it by
    /// someone else, then Notes closed; focus moved in between.
    fn log() -> (AutoCommit, DesktopTree) {
        let mut minter = IdMinter::new(7);
        let notes = app(&mut minter, "Notes");
        let mut tree = crate::seed_tree(&mut minter, "Studio", vec![notes]);
        let mut log = AutoCommit::new();
        assert!(record(&mut log, &stored(&tree), "Ada", 1_000));
        let sheets = app(&mut minter, "Sheets");
        let strip = &mut tree.workspaces[0].strips[0];
        strip.window_groups.push(tilo_desktop_model::WindowGroup {
            id: minter.mint(),
            active_tab: sheets.id,
            tabs: vec![sheets],
            width: tilo_desktop_model::WindowWidth::Normal,
            maximized: false,
        });
        let mut peer = log.fork();
        assert!(record(&mut peer, &stored(&tree), "Grace", 2_000));
        log.merge_incoming(&mut peer).unwrap();
        let group = tree.workspaces[0].strips[0].window_groups[1].id;
        let strip_id = tree.workspaces[0].strips[0].id;
        tree.set_active_group(strip_id, group);
        assert!(
            !record(&mut log, &stored(&tree), "Ada", 2_500),
            "focus alone is not a change"
        );
        tree.workspaces[0].strips[0].window_groups.remove(0);
        assert!(record(&mut log, &stored(&tree), "Ada", 3_000));
        (log, tree)
    }

    #[test]
    fn a_desktop_log_replays_as_the_rooms_history_with_its_authors_and_times() {
        let (log, tree) = log();
        let room = tree.workspaces[0].room;
        let mut history = room_history(log, room);
        assert_eq!(history.doc.get_changes(&[]).len(), 3);
        let mut names: Vec<_> = history.authors.values().cloned().collect();
        names.sort();
        assert_eq!(names, ["Ada", "Grace"]);
        let times: Vec<i64> = history
            .doc
            .get_changes(&[])
            .iter()
            .map(automerge::Change::timestamp)
            .collect();
        assert_eq!(times, [1_000, 2_000, 3_000]);

        let mut snapshot = ActivitySnapshot::new(tree, history.doc).unwrap();
        let mut pane = RoomActivityPane::new();
        register_authors(pane.authors_mut(), &history.authors);
        pane.begin_view(snapshot.active.raw());
        pane.refresh(&mut snapshot);
        let entries = pane.entries();
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert!(
            entries.iter().any(|entry| entry.author.name == "Grace"),
            "a peer's change is theirs: {entries:?}"
        );
        assert_eq!(
            pane.replay_checkpoint_names(),
            vec![vec!["Studio".to_owned()]; 3],
            "every change replays as the room it changed"
        );
        assert_eq!(pane.replay_unavailable_checkpoints(), 0);
    }

    #[test]
    fn a_bounded_projection_reports_the_retained_tail_it_did_not_replay() {
        let mut minter = IdMinter::new(7);
        let mut tree = crate::seed_tree(&mut minter, "Studio", Vec::new());
        let mut log = AutoCommit::new();
        for i in 0..=MAX_REPLAYED_CHANGES {
            tree.workspaces[0].name = format!("Studio {i}");
            assert!(record(
                &mut log,
                &stored(&tree),
                "Ada",
                i64::try_from(i).unwrap()
            ));
        }
        let mut history = room_history(log.clone(), tree.workspaces[0].room);
        assert!(history.truncated);
        assert_eq!(history.doc.get_changes(&[]).len(), MAX_REPLAYED_CHANGES);
        assert_eq!(log.get_changes(&[]).len(), MAX_REPLAYED_CHANGES + 1);
    }

    #[test]
    fn switching_tabs_does_not_write_a_desktop_activity_change() {
        let mut minter = IdMinter::new(7);
        let first = app(&mut minter, "Notes");
        let second = app(&mut minter, "Sheets");
        let second_id = second.id;
        let mut tree = crate::seed_tree(&mut minter, "Studio", vec![first]);
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(second);
        let mut log = AutoCommit::new();
        assert!(record(&mut log, &stored(&tree), "Ada", 1_000));
        tree.workspaces[0].strips[0].window_groups[0].active_tab = second_id;
        assert!(!record(&mut log, &stored(&tree), "Ada", 2_000));
        assert_eq!(log.get_changes(&[]).len(), 1);
    }

    #[test]
    fn retained_history_preserves_tasks_when_the_browser_opens_the_shared_pane() {
        let (mut log, mut tree) = log();
        let task = tilo_desktop_model::WorkspaceTask {
            id: "task:review".into(),
            title: "Review changes".into(),
            created_by: "Ada".into(),
            assigned_agent: None,
            route: tilo_desktop_model::WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::Queued,
            resource: None,
            created_at: 4_000,
            updated_at: 4_000,
            audit: Vec::new(),
        };
        tree.workspaces[0].tasks.push(task.clone());
        assert!(record(&mut log, &stored(&tree), "Ada", 4_000));
        let history = room_history(log, tree.workspaces[0].room);
        let snapshot = ActivitySnapshot::new(tree, history.doc).unwrap();
        assert_eq!(snapshot.active_workspace().unwrap().tasks, vec![task]);
    }

    #[test]
    fn a_refused_save_retries_the_same_history_without_a_new_change() {
        let (mut log, tree) = log();
        let pending = std::cell::Cell::new(true);
        assert_eq!(
            save_pending(&mut log, &pending, |_| Err("unavailable")),
            Err("unavailable")
        );
        assert!(pending.get());
        assert!(!record(&mut log, &stored(&tree), "Ada", 4_000));
        let mut saved = Vec::new();
        save_pending(&mut log, &pending, |bytes| {
            saved = bytes;
            Ok::<_, &str>(())
        })
        .unwrap();
        assert!(!pending.get());
        let reloaded = AutoCommit::load(&saved).unwrap();
        assert_eq!(
            room_history(reloaded, tree.workspaces[0].room)
                .doc
                .get_changes(&[])
                .len(),
            3
        );
        save_pending(&mut log, &pending, |_| {
            panic!("an acknowledged log is not saved twice")
        })
        .unwrap_or_else(|_: &str| panic!("no write was needed"));
    }

    #[test]
    fn another_room_and_values_that_are_no_desktop_add_nothing() {
        let (log, _) = log();
        assert_eq!(
            room_history(log, RoomId::new()).doc.get_changes(&[]).len(),
            0
        );
        let mut junk = AutoCommit::new();
        junk.put(ROOT, DESKTOP_KEY, "not json").unwrap();
        junk.commit();
        assert_eq!(
            room_history(junk, RoomId::new()).doc.get_changes(&[]).len(),
            0
        );
    }
}
