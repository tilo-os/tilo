//! History for a desktop kept as one document.
//! A host that keeps its desktop as a shared desktop record ([`tilo_desktop_model::shared_resource`])
//! stamps each change of it with when it was made and who made it ([`record`]); so does a host that
//! keeps the layout's own document ([`tilo_desktop_model::desktop_doc`]).
use std::collections::BTreeMap;

use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_resource;

/// The desktop a host value carries: `{ tree, records?, authors? }` , or a bare tree.
fn desktop_of(value: &serde_json::Value) -> Option<SharedDesktop> {
    if value.get("tree").is_some() {
        return serde_json::from_value(value.clone()).ok();
    }
    Some(SharedDesktop {
        tree: tilo_desktop_model::Layout::from_json(value.clone()).ok()?,
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
    match shared_resource::write(log, &desktop) {
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
/// shell's history says who did it and when.
pub fn commit(doc: &mut AutoCommit, author: &str, time: i64) {
    shared_resource::commit_change(doc, author, time, None);
}

/// The author a change of a desktop log names.
pub fn change_author(message: Option<&str>) -> Option<String> {
    shared_resource::change_author(message)
}

/// The name of the workspace in front on a desktop a host value carries, as a person reads it: the
/// name it was given, or the word for a workspace nobody has named.
pub fn workspace_name(value: &serde_json::Value) -> Option<String> {
    desktop_of(value)?
        .tree
        .active_workspace()
        .map(|workspace| crate::names::workspace(&workspace.name))
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

/// The name and colour presence shows for an author (the actor a desktop change names), when
/// the host knows them.
pub type PersonOf<'a> = &'a dyn Fn(&str) -> Option<(String, (u8, u8, u8))>;

/// Who each actor of the layout's document `doc` writes for, by actor (hex): the names the document
/// holds ([`desktop_doc::author_names`]) and those its changes name, so a pane names every change
/// of the layout by its person.
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

/// Name `authors` ' actors in `directory` , as one member per author. An author `person` knows
/// reads as presence shows them, by the same name and in the same colour as their avatar; a change
/// names its author by their actor (a key, in a workspace), never something to show.
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
    use tilo_desktop_model::{IdMinter, Layout, Tab, TileContent};

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

    fn stored(tree: &Layout) -> serde_json::Value {
        serde_json::json!({ "tree": tree, "records": [] })
    }

    tilo_test_env::scenario! {
        fn the_history_log_names_each_change_by_its_author;

        phase a_rename_by_someone_else_is_noticed_with_their_name;
        // Someone else renaming the shared workspace is news, named in words with who did it; the
        // first desktop received and a rename of one's own are not.
        {
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

        phase an_author_presence_knows_reads_by_their_name_and_colour;
        // A workspace's changes name their author by actor (a key); History shows them as presence
        // does, by name and colour, not by the key's first letter in a colour of its own.
        {
            let key = "b46342a2fe9dd341355944fb4be947d6";
            let pink = (0xee, 0x83, 0xaf);
            let authors = BTreeMap::from([("aa".to_owned(), key.to_owned())]);
            let mut directory = ui::AuthorDirectory::new();
            register_authors(&mut directory, &authors, &|actor| {
                (actor == key).then(|| ("Eager Dolphin".to_owned(), pink))
            });
            let author = directory.resolve("aa");
            assert_eq!(author.name, "Eager Dolphin");
            assert_eq!(author.rgb(), pink);
            assert_eq!(ui::widget::avatar::initials_for_name(&author.name), "ED");

            let mut unknown = ui::AuthorDirectory::new();
            register_authors(&mut unknown, &authors, &|_| None);
            assert_eq!(unknown.resolve("aa").name, key, "nobody to name them by");
        }

        phase a_merged_shell_lists_both_peers_changes_by_name;
        // Two peers each change the shell and merge: each reads the one shell with both changes, and the
        // pane lists each change by the person who made it.
        {
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
            for shell in [doc, peer] {
                let mut snapshot = ActivitySnapshot::of_desktop(tree.clone(), shell);
                let authors = authors_of(&mut snapshot.doc);
                let mut pane = WorkspaceActivityPane::new();
                register_authors(pane.authors_mut(), &authors, &|_| None);
                pane.begin_view(snapshot.active);
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

        phase switching_tabs_does_not_write_a_desktop_activity_change;
        {
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
    }
}
