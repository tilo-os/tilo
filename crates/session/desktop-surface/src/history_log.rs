//! History for a desktop kept as one document, and the older logs it came from.
//! A host that keeps its desktop as a shared desktop record ([`tilo_desktop_model::shared_record`])
//! stamps each change of it with when it was made and who made it ([`record`]); so does a host that
//! keeps the desk's own document ([`tilo_desktop_model::desktop_doc`]). A desk an older build
//! logged is read into that document once ([`desk_document`]), keeping the author and the time of
//! every change it made, so the pane lists the same history it listed before.
use std::collections::BTreeMap;

use automerge::AutoCommit;
use automerge::transaction::{CommitOptions, Transactable as _};
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_record;

/// A change written before changes named their action names its author after this prefix.
const AUTHOR_PREFIX: &str = "author:";

/// The desktop a host value carries: `{ tree, records?, authors? }` , or a bare tree.
fn desktop_of(value: &serde_json::Value) -> Option<SharedDesktop> {
    if value.get("tree").is_some() {
        return serde_json::from_value(value.clone()).ok();
    }
    Some(SharedDesktop {
        tree: tilo_desktop_model::DesktopTree::from_json(value.clone()).ok()?,
        ..SharedDesktop::default()
    })
}

/// Write `value` to `log` as one change by `author` at `time` (seconds since the epoch) when what
/// the record keeps of it differs from what the log holds; which window is focused is not kept, so
/// moving focus records nothing. Returns whether it wrote.
pub fn record(log: &mut AutoCommit, value: &serde_json::Value, author: &str, time: i64) -> bool {
    let Some(desktop) = desktop_of(value) else {
        return false;
    };
    match shared_record::write(log, &desktop) {
        Ok(true) => {
            commit(log, author, time);
            true
        }
        Ok(false) => false,
        Err(_) => {
            log.rollback();
            false
        }
    }
}

/// Commit what `doc` holds as one change, by `author` at `time` (seconds since the epoch), so the
/// desk's history says who did it and when.
pub fn commit(doc: &mut AutoCommit, author: &str, time: i64) {
    shared_record::commit_change(doc, author, time, None);
}

/// The author a change of a desktop log names.
pub fn change_author(message: Option<&str>) -> Option<String> {
    shared_record::change_author(message).or_else(|| {
        message?
            .strip_prefix(AUTHOR_PREFIX)
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
    })
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
        change_author(change.message())
    })
}

/// The shared workspace renamed by a change merged into `log` , which took its desktop from
/// `before` to `after` : who renamed it, and to what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRename {
    pub author: String,
    pub name: String,
}

impl WorkspaceRename {
    /// The rename `log` 's newest change made, if the workspace in front is named differently in
    /// `after` than in `before` . The first desktop a host receives (`before` is `None` ) renames
    /// nothing.
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

    /// The notice for the person `me` , naming the author as `name_of` does: none for a rename they
    /// made themselves.
    pub fn notice_for(
        &self,
        me: &str,
        name_of: impl FnOnce(&str) -> String,
    ) -> Option<crate::notifications::Notification> {
        (self.author != me).then(|| {
            let title = crate::i18n::t_args(
                "workspace-notice-renamed",
                &[
                    ("name", name_of(&self.author).into()),
                    ("workspace", self.name.clone().into()),
                ],
            );
            crate::notifications::Notification::new(title, "")
        })
    }
}

/// The desk an older build logged, read into the desk's one document
/// ([`tilo_desktop_model::desktop_doc`]): every change of `log` becomes a change of the document
/// (in causal order), by the author it named and at the time it was made, and `stored` — what the
/// older build kept beside the log, which the log never recorded — becomes the newest change, by
/// `actor` at `time` . A change that moved nothing of the desk adds nothing.
/// The names the old changes are known by go in with them, so the desktop's history names the same
/// people it did before.
pub fn desk_document(
    log: &mut AutoCommit,
    stored: Option<&SharedDesktop>,
    actor: automerge::ActorId,
    time: i64,
) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let names = names_in(log);
    // The names go in with the first change they name: written before one,
    // they would be a change of the desk of their own, at no time at all.
    let mut wrote_names = false;
    for change in log.get_changes(&[]) {
        let Some(desktop) = shared_record::read_as_kept(log, Some(&[change.hash()])) else {
            continue;
        };
        doc.set_actor(actor_of(&change));
        if !wrote_names && !names.is_empty() {
            desktop_doc::set_author_names(&mut doc, &names);
            wrote_names = true;
        }
        desktop_doc::write_shared(&mut doc, &desktop.tree, &desktop.records, &desktop.authors);
        doc.commit_with(CommitOptions::default().with_time(change.timestamp()));
    }
    if let Some(stored) = stored {
        doc.set_actor(actor);
        desktop_doc::write_shared(&mut doc, &stored.tree, &stored.records, &stored.authors);
        if doc.pending_ops() > 0 {
            doc.commit_with(CommitOptions::default().with_time(time));
        }
    }
    if doc.pending_ops() > 0 {
        doc.commit_with(CommitOptions::default().with_time(time));
    }
    doc
}

/// The actor a change of an old log is attributed to in the document: the author it named, under
/// one actor per author — the log wrote every change under one actor and named the author in the
/// change itself, and the pane names an actor by the identity it reads.
fn actor_of(change: &automerge::Change) -> automerge::ActorId {
    change_author(change.message()).map_or_else(
        || change.actor_id().clone(),
        |author| automerge::ActorId::from(author.as_bytes()),
    )
}

/// The names an old log's changes are by, by actor (hex).
fn names_in(log: &mut AutoCommit) -> BTreeMap<String, String> {
    log.get_changes(&[])
        .into_iter()
        .filter_map(|change| {
            let author = change_author(change.message())?;
            Some((
                automerge::ActorId::from(author.as_bytes()).to_hex_string(),
                author,
            ))
        })
        .collect()
}

/// The name and colour presence shows for an author (the principal a desktop change names), when
/// the host knows them.
pub type PersonOf<'a> = &'a dyn Fn(&str) -> Option<(String, (u8, u8, u8))>;

/// Who each actor of the desk's document `doc` writes for, by actor (hex): the names the document
/// holds ([`desktop_doc::author_names`]) and those its changes name, so a pane names every change
/// of the desk by its person.
pub fn authors_of(doc: &mut AutoCommit) -> BTreeMap<String, String> {
    let mut authors = desktop_doc::author_names(doc);
    for change in doc.get_changes(&[]) {
        let Some(author) = change_author(change.message()) else {
            continue;
        };
        authors
            .entry(change.actor_id().to_hex_string())
            .or_insert(author);
    }
    authors
}

/// Name `authors` ' actors in `directory` , as one participant per author. An author `person` knows
/// reads as presence shows them, by the same name and in the same colour as their avatar; a change
/// names its author by their principal (a key, in a workspace), never something to show.
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
    use crate::activity::{ActivitySnapshot, WorkspaceActivityPane};
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
            view: String::new(),
            pinned: false,
        }
    }

    fn stored(tree: &DesktopTree) -> serde_json::Value {
        serde_json::json!({ "tree": tree, "records": [] })
    }

    /// A desktop opened with Notes, then Sheets opened beside it by someone else, then Notes
    /// closed; focus moved in between.
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
        // Tab opens are View and omitted from the explorer feed; rename so Grace still authors a
        // feed-visible Change.
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

    /// Someone else renaming the shared workspace is news, named in words with who did it; the
    /// first desktop received and a rename of one's own are not.
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

    /// The desk an older build logged opens as the desk's one document, with every change it made
    /// in it, by the same author and at the same time: the pane lists the history the person saw
    /// before.
    #[test]
    fn a_desktop_log_becomes_the_desks_document_with_its_authors_and_times() {
        let (mut log, tree) = log();
        let mut doc = desk_document(
            &mut log,
            None,
            automerge::ActorId::from("ada".as_bytes()),
            4_000,
        );
        let times: Vec<i64> = doc
            .get_changes(&[])
            .iter()
            .map(automerge::Change::timestamp)
            .collect();
        assert_eq!(times, [1_000, 2_000, 3_000]);
        let names: Vec<String> = desktop_doc::author_names(&doc).into_values().collect();
        assert_eq!(
            names,
            ["Ada", "Grace"],
            "who changed it, as they were named"
        );

        // What the document holds is the desk the log last made.
        let mut rebuilt = DesktopTree::default();
        desktop_doc::rebuild(&doc, &mut rebuilt);
        assert_eq!(rebuilt.active, tree.active);
        assert_eq!(rebuilt.workspaces.len(), tree.workspaces.len());
        assert_eq!(rebuilt.workspaces[0].name, tree.workspaces[0].name);
        assert_eq!(rebuilt.workspaces[0].id(), tree.workspaces[0].id());
        let windows = |desk: &DesktopTree| -> Vec<(u64, String)> {
            desk.tabs().map(|tab| (tab.id, tab.title.clone())).collect()
        };
        assert_eq!(windows(&rebuilt), windows(&tree));

        let authors = desktop_doc::author_names(&doc);
        let mut snapshot = ActivitySnapshot::of_desktop(tree, doc);
        let mut pane = WorkspaceActivityPane::new();
        register_authors(pane.authors_mut(), &authors, &|_| None);
        pane.begin_view(snapshot.active.raw());
        pane.refresh(&mut snapshot);
        let entries = pane.entries();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(
            entries.iter().any(|entry| entry.author.name == "Grace"),
            "a peer's change is theirs: {entries:?}"
        );
        assert_eq!(
            pane.entries()
                .iter()
                .filter(|entry| entry.author.name == "Ada")
                .count(),
            1,
            "and the person here's is theirs: {entries:?}"
        );
    }

    /// A workspace's changes name their author by principal (a key); History shows them as presence
    /// does, by name and colour, not by the key's first letter in a colour of its own.
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

    /// Two peers each change the desk and merge: each reads the one desk with both changes, and the
    /// pane lists each change by the person who made it.
    #[test]
    fn a_merged_desk_lists_both_peers_changes_by_name() {
        let mut minter = IdMinter::new(7);
        let notes = app(&mut minter, "Notes");
        let tree = crate::seed_tree(&mut minter, "Studio", vec![notes]);
        let mut doc = AutoCommit::new();
        desktop_doc::seed(&mut doc, &tree);
        commit(&mut doc, "Ada", 1_000);
        let mut peer = doc.fork();

        let mut mine = tree.clone();
        mine.workspaces[0].name = "Studio · mine".into();
        desktop_doc::seed(&mut doc, &mine);
        commit(&mut doc, "Ada", 2_000);

        let mut theirs = tree.clone();
        let sheets = app(&mut minter, "Sheets");
        theirs.workspaces[0].strips[0]
            .window_groups
            .push(tilo_desktop_model::WindowGroup {
                id: minter.mint(),
                active_tab: sheets.id,
                tabs: vec![sheets],
                width: tilo_desktop_model::WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        desktop_doc::seed(&mut peer, &theirs);
        commit(&mut peer, "Grace", 3_000);

        doc.merge_incoming(&mut peer).expect("merged");
        peer.merge_incoming(&mut doc).expect("merged");
        for desk in [doc, peer] {
            let mut snapshot = ActivitySnapshot::of_desktop(tree.clone(), desk);
            let authors = authors_of(&mut snapshot.doc);
            let mut pane = WorkspaceActivityPane::new();
            register_authors(pane.authors_mut(), &authors, &|_| None);
            pane.begin_view(snapshot.active.raw());
            pane.refresh(&mut snapshot);
            let listed: Vec<(String, String)> = pane
                .entries()
                .iter()
                .map(|entry| (entry.author.name.clone(), entry.summary.clone()))
                .collect();
            assert_eq!(listed.len(), 2, "{listed:?}");
            assert!(
                listed
                    .iter()
                    .any(|(author, summary)| author == "Grace" && summary.contains("Sheets")),
                "the window the other peer opened is theirs: {listed:?}"
            );
            assert!(
                listed.iter().any(|entry| entry.0 == "Ada"),
                "the person's own change is theirs: {listed:?}"
            );
        }
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

    /// A log kept away and read back makes the same document: what the pane lists of the desk a
    /// person left is what they come back to.
    #[test]
    fn a_log_saved_and_read_back_makes_the_same_document() {
        let (mut log, _tree) = log();
        let saved = log.save();
        let mut reloaded = AutoCommit::load(&saved).unwrap();
        let mut here = desk_document(
            &mut reloaded,
            None,
            automerge::ActorId::from("ada".as_bytes()),
            4_000,
        );
        assert_eq!(here.get_changes(&[]).len(), 3);
    }

    #[test]
    fn values_that_are_no_desktop_add_nothing() {
        let mut junk = AutoCommit::new();
        junk.put(automerge::ROOT, "workspaces", "not a desktop")
            .unwrap();
        junk.commit();
        let mut doc = desk_document(
            &mut junk,
            None,
            automerge::ActorId::from("ada".as_bytes()),
            1_000,
        );
        assert_eq!(doc.get_changes(&[]).len(), 0);
    }
}
