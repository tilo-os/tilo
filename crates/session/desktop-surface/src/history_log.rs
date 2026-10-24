//! History for a desktop kept as one shared record.
//!
//! A host that saves (or replicates) its desktop as a shared desktop record
//! ([`tilo_desktop_model::shared_record`]) stamps each change of it with
//! when it was made and who made it ([`record`]). The history pane reads
//! room documents, so [`room_history`] replays such a log into one: each
//! change that moved a window of the room becomes one change of the room's
//! projection, by the same actor at the same time, which the pane
//! summarises ("opened", "closed", "renamed") and replays exactly as it does
//! a native room's.
use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ROOT};
use tilo_automerge::WriteTyped as _;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::projection::{EmptyStrips, project_owned_contents};
use tilo_desktop_model::{RoomId, Workspace, shared_record};

/// A change's message names its author after this prefix.
const AUTHOR_PREFIX: &str = "author:";
/// The newest changes a room history replays; older ones stay in the log.
pub const MAX_REPLAYED_CHANGES: usize = 600;

/// The desktop a host value carries: `{ tree, records?, authors? }`, or a
/// bare tree.
fn desktop_of(value: &serde_json::Value) -> Option<SharedDesktop> {
    if value.get("tree").is_some() {
        return serde_json::from_value(value.clone()).ok();
    }
    Some(SharedDesktop {
        tree: tilo_desktop_model::DesktopTree::from_json(value.clone()).ok()?,
        ..SharedDesktop::default()
    })
}

/// Write `value` to `log` as one change by `author` at `time` (seconds since
/// the epoch) when what the record keeps of it differs from what the log
/// holds; which window is focused is not kept, so moving focus records
/// nothing. Returns whether it wrote.
pub fn record(log: &mut AutoCommit, value: &serde_json::Value, author: &str, time: i64) -> bool {
    let Some(desktop) = desktop_of(value) else {
        return false;
    };
    match shared_record::write(log, &desktop) {
        Ok(true) => {
            log.commit_with(
                CommitOptions::default()
                    .with_message(format!("{AUTHOR_PREFIX}{author}"))
                    .with_time(time),
            );
            true
        }
        Ok(false) => false,
        Err(_) => {
            log.rollback();
            false
        }
    }
}

/// Save a changed log through the host's store. A refusal keeps it pending,
/// including when the next attempt records no additional change.
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

/// The name of the workspace in front on a desktop a host value carries,
/// when it has one.
pub fn workspace_name(value: &serde_json::Value) -> Option<String> {
    desktop_of(value)?
        .tree
        .active_workspace()
        .map(|workspace| workspace.name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// The author the newest change of `log` names.
pub fn latest_author(log: &AutoCommit) -> Option<String> {
    log.base_heads().iter().find_map(|head| {
        let change = automerge::ReadDoc::get_change_by_hash(log, head)?;
        change_author(change.message()).map(str::to_owned)
    })
}

/// The shared workspace renamed by a change merged into `log`, which took
/// its desktop from `before` to `after`: who renamed it, and to what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRename {
    pub author: String,
    pub name: String,
}

impl WorkspaceRename {
    /// The rename `log`'s newest change made, if the workspace in front is
    /// named differently in `after` than in `before`. The first desktop a
    /// host receives (`before` is `None`) renames nothing.
    pub fn merged(
        log: &AutoCommit,
        before: Option<&serde_json::Value>,
        after: &serde_json::Value,
    ) -> Option<Self> {
        let before = workspace_name(before?)?;
        let name = workspace_name(after).filter(|after| *after != before)?;
        Some(Self {
            author: latest_author(log)?,
            name,
        })
    }

    /// The notice for the person `me`, naming the author as `name_of` does:
    /// none for a rename they made themselves.
    pub fn notice_for(
        &self,
        me: &str,
        name_of: impl FnOnce(&str) -> String,
    ) -> Option<crate::notifications::Notification> {
        (self.author != me).then(|| {
            let title = crate::i18n::t_args(
                "room-notice-renamed",
                &[
                    ("name", name_of(&self.author).into()),
                    ("workspace", self.name.clone().into()),
                ],
            );
            crate::notifications::Notification::new(title, "")
        })
    }
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
        let Some(tree) = shared_record::read(&log, Some(&[change.hash()])).map(|d| d.tree) else {
            continue;
        };
        let Some(workspace) = tree.workspaces.iter().find(|w| w.room() == room) else {
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
        let _ = doc.put_str_if_changed(
            &ROOT,
            tilo_desktop_model::projection::ROOM_NAME_KEY,
            &workspace.name,
        );
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

/// The name and colour presence shows for an author (the principal a
/// desktop change names), when the host knows them.
pub type PersonOf<'a> = &'a dyn Fn(&str) -> Option<(String, (u8, u8, u8))>;

/// Name `authors`' actors in `directory`, as one participant per author.
/// An author `person` knows reads as presence shows them, by the same
/// name and in the same colour as their avatar; a change names its author
/// by their principal (a key, in a room), never something to show.
pub fn register_authors(
    directory: &mut ui::AuthorDirectory,
    authors: &BTreeMap<String, String>,
    person: PersonOf<'_>,
) {
    for (actor, name) in authors {
        let identity = automerge::ActorId::from(name.as_bytes()).to_hex_string();
        let author = match person(name) {
            Some((label, rgb)) => ui::history_source::author_coloured(
                label,
                ui::history_source::short_id(&identity),
                rgb,
            ),
            None => ui::widget::HistoryAuthor::new(
                name.clone(),
                ui::history_source::stable_hue(&identity),
                ui::history_source::short_id(&identity),
            ),
        };
        directory.register(actor, author);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivitySnapshot, RoomActivityPane};
    use tilo_automerge::ApplyIncoming as _;
    use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TileContent};

    fn app(minter: &mut IdMinter, title: &str) -> Tab {
        Tab {
            id: minter.mint(),
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: format!("so.tilo.{}", title.to_lowercase()),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
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
            focus_history: Vec::new(),
        });
        // Tab opens are View and omitted from the explorer feed; rename so
        // Grace still authors a feed-visible Change.
        tree.workspaces[0].name = "Studio · shared".into();
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

    /// Someone else renaming the shared workspace is news, named in words
    /// with who did it; the first desktop received and a rename of one's
    /// own are not.
    #[test]
    fn a_rename_by_someone_else_is_noticed_with_their_name() {
        let mut minter = IdMinter::new(7);
        let notes = app(&mut minter, "Notes");
        let mut tree = crate::seed_tree(&mut minter, "Studio", vec![notes]);
        let mut log = AutoCommit::new();
        assert!(record(&mut log, &stored(&tree), "ada-root", 1_000));
        let before = stored(&tree);
        tree.workspaces[0].name = "Plans".into();
        let mut peer = log.fork();
        assert!(record(&mut peer, &stored(&tree), "grace-root", 2_000));
        log.merge_incoming(&mut peer).unwrap();
        let after = stored(&tree);

        assert_eq!(
            WorkspaceRename::merged(&log, None, &after),
            None,
            "the first desktop renames nothing"
        );
        assert_eq!(WorkspaceRename::merged(&log, Some(&after), &after), None);
        let rename = WorkspaceRename::merged(&log, Some(&before), &after)
            .expect("the merged change renamed the workspace");
        assert_eq!(
            rename,
            WorkspaceRename {
                author: "grace-root".into(),
                name: "Plans".into(),
            }
        );
        let notice = rename
            .notice_for("ada-root", |user| {
                assert_eq!(user, "grace-root");
                "Grace".into()
            })
            .expect("Ada hears of it");
        assert_eq!(notice.title, "Grace renamed the workspace to Plans");
        assert!(
            rename
                .notice_for("grace-root", |_| "Grace".into())
                .is_none(),
            "Grace renamed it herself"
        );
    }

    #[test]
    fn a_desktop_log_replays_as_the_rooms_history_with_its_authors_and_times() {
        let (log, tree) = log();
        let room = tree.workspaces[0].room();
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
        register_authors(pane.authors_mut(), &history.authors, &|_| None);
        pane.begin_view(snapshot.active.raw());
        pane.refresh(&mut snapshot);
        let entries = pane.entries();
        // Setup (first open) is omitted from the explorer feed; Grace's rename
        // and Ada's close remain as Change rows.
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(
            entries.iter().any(|entry| entry.author.name == "Grace"),
            "a peer's change is theirs: {entries:?}"
        );
        assert_eq!(
            pane.replay_checkpoint_names(),
            vec![
                vec!["Studio".to_owned()],
                vec!["Studio · shared".to_owned()],
                vec!["Studio · shared".to_owned()],
            ],
            "every change replays as the room it changed"
        );
        assert_eq!(pane.replay_unavailable_checkpoints(), 0);
    }

    /// A room's changes name their author by principal (a key); History
    /// shows them as presence does, by name and colour, not by the key's
    /// first letter in a colour of its own.
    #[test]
    fn an_author_presence_knows_reads_by_their_name_and_colour() {
        let key = "b46342a2fe9dd341355944fb4be947d6";
        let pink = (0xee, 0x83, 0xaf);
        let authors = BTreeMap::from([("aa".to_owned(), key.to_owned())]);
        let mut directory = ui::AuthorDirectory::new();
        register_authors(&mut directory, &authors, &|principal| {
            (principal == key).then(|| ("Eager Dolphin".to_owned(), pink))
        });
        let author = directory.resolve("aa");
        assert_eq!(author.name, "Eager Dolphin");
        assert_eq!(author.rgb(), pink);
        assert_eq!(ui::widget::avatar::initials_for_name(&author.name), "ED");

        let mut unknown = ui::AuthorDirectory::new();
        register_authors(&mut unknown, &authors, &|_| None);
        assert_eq!(unknown.resolve("aa").name, key, "nobody to name them by");
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
        let mut history = room_history(log.clone(), tree.workspaces[0].room());
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
            room_history(reloaded, tree.workspaces[0].room())
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
        junk.put(ROOT, "workspaces", "not a desktop").unwrap();
        junk.commit();
        assert_eq!(
            room_history(junk, RoomId::new()).doc.get_changes(&[]).len(),
            0
        );
    }
}
