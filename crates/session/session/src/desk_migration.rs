//! The desk older sessions kept, read once into the desk's one document.
//!
//! A session used to keep the desk twice: as a tree file
//! (`tilo/desk.json`), and beside it as a log of whole-desk snapshots, one per
//! change, whose changes the activity pane listed
//! (`tilo/desk-history.automerge`). Both are the desk this device had. The
//! first start on this build reads them into the document that replaces them
//! ([`crate::desk_doc`]): every change in the log becomes a change of the
//! document, keeping the author and the time it was made, and the tree file's
//! desk becomes the newest change, since it holds what the log never did — how
//! wide each window is, which one is in front, which display it is on. The old
//! files are then removed.
//!
//! Nothing is lost: the desk a person left is the desk they come back to, and
//! who changed what before is still in its history. A tree file a newer build
//! wrote is not read at all: it stays where it is, nothing is written beside
//! it, and the session says where it is ([`StateNotice`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use automerge::transaction::{CommitOptions, Transactable as _};
use tilo_desktop_model::{DesktopTree, STATE_SCHEMA_VERSION, desktop_doc, shared_record};
use tilo_desktop_surface::history_log;

use crate::activity::now_seconds;
use crate::desk_doc::StateNotice;

/// The tree the desk used to be kept as.
const TREE_FILE: &str = "desk.json";
/// The log of whole-desk snapshots kept beside it.
const LOG_FILE: &str = "desk-history.automerge";

fn tree_at(state: &Path) -> PathBuf {
    state.join("tilo").join(TREE_FILE)
}

fn log_at(state: &Path) -> PathBuf {
    state.join("tilo").join(LOG_FILE)
}

/// The desk the older files under `state` held.
pub(crate) enum Adopted {
    /// The desk as the one document it is kept as now.
    Document(Box<AutoCommit>),
    /// This device kept no desk before.
    Nothing,
    /// The desk is there and this build does not open it, with what the
    /// person is told about it.
    Refused(StateNotice),
}

/// The desk the older files under `state` hold, as the one document `state`
/// now keeps at `document`, written by `actor`; `Nothing` when this device
/// has neither file.
pub(crate) fn adopt(state: &Path, document: &Path, actor: automerge::ActorId) -> Adopted {
    let mut log = read_log(state);
    let tree = match read_tree(state) {
        Tree::None => None,
        Tree::Desk(tree) => Some(*tree),
        Tree::Refused(notice) => return Adopted::Refused(notice),
    };
    if log.is_none() && tree.is_none() {
        return Adopted::Nothing;
    }
    let mut doc = AutoCommit::new();
    crate::desk_doc::stamp_schema(&mut doc);
    if let Some(log) = &mut log {
        // Who the changes are by, in the one change that opens the document:
        // a name written later would be a change of the desk of its own.
        let names = names_in(log);
        if !names.is_empty() {
            desktop_doc::set_author_names(&mut doc, &names);
        }
        for change in log.get_changes(&[]) {
            let Some(desktop) = shared_record::read(log, Some(&[change.hash()])) else {
                continue;
            };
            doc.set_actor(actor_of(&change));
            desktop_doc::seed(&mut doc, &desktop.tree);
            doc.commit_with(CommitOptions::default().with_time(change.timestamp()));
        }
    }
    if let Some(tree) = tree {
        // What the tree file holds that the log never did — how wide a window
        // is, which is in front, which display it is on — is what the person
        // here last left, so the newest change is theirs.
        doc.set_actor(actor);
        desktop_doc::seed(&mut doc, &crate::desk_doc::relaunchable(&tree));
        if doc.pending_ops() > 0 {
            doc.commit_with(CommitOptions::default().with_time(now_seconds()));
        }
    }
    if doc.pending_ops() > 0 {
        doc.commit_with(CommitOptions::default().with_time(now_seconds()));
    }
    // The desk moves into its one document before the files it came from go:
    // a session that dies before the write lands migrates again.
    let bytes = doc.save();
    if let Err(error) = tilo_files::native::atomic::write_private(document, &bytes) {
        tracing::warn!(%error, "the desk is not kept");
    } else {
        forget(state, log.is_some());
    }
    Adopted::Document(Box::new(doc))
}

/// Stop keeping the files the desk came from: it is in its one document now.
/// A log this build could not read stays: nothing was read out of it, and a
/// build that can read it may still find the person's history there.
pub(crate) fn forget(state: &Path, log_read: bool) {
    let mut gone = vec![tree_at(state)];
    if log_read {
        gone.push(log_at(state));
    }
    for path in gone {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "an old desk file stays");
            }
        }
    }
}

/// The actor a change of the old log is attributed to in the document: the
/// author it named, under one actor per author — the log wrote every change
/// under one actor and named the author in the change itself, and the pane
/// names an actor by the identity it reads.
fn actor_of(change: &automerge::Change) -> automerge::ActorId {
    history_log::change_author(change.message()).map_or_else(
        || change.actor_id().clone(),
        |author| automerge::ActorId::from(author.as_bytes()),
    )
}

/// The names the old log's changes are by, by actor (hex).
fn names_in(log: &mut AutoCommit) -> BTreeMap<String, String> {
    log.get_changes(&[])
        .into_iter()
        .filter_map(|change| {
            let author = history_log::change_author(change.message())?;
            Some((
                automerge::ActorId::from(author.as_bytes()).to_hex_string(),
                author,
            ))
        })
        .collect()
}

fn read_log(state: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(log_at(state)).ok()?;
    let options = automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore);
    AutoCommit::load_with_options(&bytes, options)
        .map_err(|error| tracing::debug!(%error, "the old desk log is not this build's"))
        .ok()
}

/// What the old tree file holds.
enum Tree {
    /// The desk the person left, ready to be read into the document.
    Desk(Box<DesktopTree>),
    /// There is no tree file.
    None,
    /// The file is there and this build does not open it.
    Refused(StateNotice),
}

/// The desk the tree file under `state` holds, if there is one this build
/// understands.
fn read_tree(state: &Path) -> Tree {
    let path = tree_at(state);
    let refused = |error: String| {
        Tree::Refused(StateNotice::Unreadable {
            path: path.clone(),
            error,
        })
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Tree::None,
        Err(error) => return refused(error.to_string()),
    };
    let tree: DesktopTree = match serde_json::from_str(&text) {
        Ok(tree) => tree,
        Err(error) => {
            // A newer layout means the desk is intact and this build is the
            // wrong one; a file that does not parse at all may be damage.
            return Tree::Refused(match version_in(&text) {
                Some(found) if found > STATE_SCHEMA_VERSION => StateNotice::NewerSchema {
                    path,
                    found,
                    understood: STATE_SCHEMA_VERSION,
                },
                _ => StateNotice::Unreadable {
                    path,
                    error: error.to_string(),
                },
            });
        }
    };
    if tree.schema_version > STATE_SCHEMA_VERSION {
        return Tree::Refused(StateNotice::NewerSchema {
            path,
            found: tree.schema_version,
            understood: STATE_SCHEMA_VERSION,
        });
    }
    // A desk an older build wrote has the shape this one reads, and the
    // document being written brings it up to date.
    Tree::Desk(Box::new(tree))
}

/// The version `text` says it was written by, read without the desk having to
/// be deserialized: what tells a newer build's desk from a damaged one.
fn version_in(text: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    u32::try_from(value.get("schema_version")?.as_u64()?).ok()
}

#[cfg(test)]
mod tests {
    //! A desk older sessions kept still opens, and the files it came from give
    //! way to the one document.

    use super::*;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use tilo_desktop_surface::seed_tree;

    /// What the desk was called before it was renamed.
    const FIRST_NAME: &str = "Studio";
    /// What the newer session renamed it to.
    const LAST_NAME: &str = "Plans";
    /// When each change was made, seconds since the epoch.
    const FIRST_TIME: i64 = 1_700_000_000;
    const LAST_TIME: i64 = 1_700_000_100;

    /// The desk two older sessions left under `state`: the tree file the last
    /// session wrote, and the log of the changes that made it, each named by
    /// who made it.
    fn older_sessions(state: &Path) -> DesktopTree {
        let mut minter = IdMinter::new(7);
        let notes = Tab {
            id: minter.mint(),
            title: "Notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
        };
        let studio = seed_tree(&mut minter, FIRST_NAME, vec![notes]);
        let mut plans = studio.clone();
        plans.workspaces[0].name = LAST_NAME.into();
        let mut log = AutoCommit::new();
        for (tree, author, time) in [(&studio, "ada", FIRST_TIME), (&plans, "grace", LAST_TIME)] {
            let value = serde_json::to_value(tree).expect("a desk is json");
            assert!(
                history_log::record(&mut log, &value, author, time),
                "each change moves the desk"
            );
        }
        let dir = state.join("tilo");
        std::fs::create_dir_all(&dir).expect("the state directory");
        std::fs::write(
            dir.join(TREE_FILE),
            serde_json::to_string(&plans).expect("a desk is json"),
        )
        .expect("the tree file is written");
        std::fs::write(dir.join(LOG_FILE), log.save()).expect("the log is written");
        plans
    }

    /// The desk an older session left opens as the one document, with every
    /// window it held, the way it was shown, and who changed it before; the
    /// files it came from are gone.
    #[test]
    fn an_old_desk_opens_as_the_one_document() {
        let state = tempfile::tempdir().expect("a state directory");
        let left = older_sessions(state.path());

        let desk = crate::desk_doc::DeskDoc::load(Some(state.path()), "ada");
        let opened = desk.tree().expect("the desk opens");
        assert_eq!(opened.workspaces[0].name, LAST_NAME);
        assert_eq!(opened.workspaces[0].room(), left.workspaces[0].room());
        assert_eq!(
            opened.workspaces[0].strips[0].window_groups[0].tabs[0].title,
            "Notes"
        );
        // How the desk is shown came with the tree file, which the log never
        // recorded: no window is moved to where neither file said it was.
        assert_eq!(opened.active, left.active);
        assert_eq!(opened.active_group, left.active_group);
        assert_eq!(opened.outputs, left.outputs);
        assert_eq!(
            opened.workspaces[0].strips[0].window_groups[0].width,
            left.workspaces[0].strips[0].window_groups[0].width
        );

        // Who changed the desk before is still part of it.
        let names: Vec<String> = desk.authors().into_values().collect();
        assert!(names.contains(&"grace".to_string()), "{names:?}");
        assert!(names.contains(&"ada".to_string()), "{names:?}");

        // The files it came from give way to the document that replaces them.
        let dir = state.path().join("tilo");
        assert!(!dir.join(TREE_FILE).exists());
        assert!(!dir.join(LOG_FILE).exists());
        assert!(dir.join(crate::desk_doc::FILE).exists());
    }

    /// The desk the document holds opens on the next start, with the change
    /// made here as its newest, by the person here, and the view it was left
    /// in.
    #[test]
    fn the_desk_kept_in_its_document_opens_on_the_next_start() {
        let state = tempfile::tempdir().expect("a state directory");
        older_sessions(state.path());

        let here = crate::desk_doc::DeskDoc::load(Some(state.path()), "ada");
        let left = here.tree().expect("the desk opens");
        let mut renamed = left.clone();
        renamed.workspaces[0].name = FIRST_NAME.into();
        assert!(here.record(&renamed, LAST_TIME + 100));
        assert!(
            !here.record(&renamed, LAST_TIME + 100),
            "a desk the document holds is no change"
        );
        tilo_durable_writer::global().flush();

        let next = crate::desk_doc::DeskDoc::load(Some(state.path()), "ada");
        let opened = next.tree().expect("the desk opens");
        assert_eq!(opened.workspaces[0].name, FIRST_NAME);
        assert_eq!(opened.active, left.active, "the view it was left in");
        assert_eq!(opened.active_group, left.active_group);
        let made_here: Vec<_> = next
            .document()
            .get_changes(&[])
            .into_iter()
            .filter(|change| change.timestamp() == LAST_TIME + 100)
            .collect();
        assert_eq!(made_here.len(), 1, "one change, kept");
        assert_eq!(
            made_here[0].actor_id(),
            &crate::desk_doc::actor_of_person("ada"),
            "the person here made it"
        );
    }

    /// A desk a newer build wrote is not read into the document, not written
    /// over and not moved: the person is told where it still is.
    #[test]
    fn an_old_desk_a_newer_build_wrote_is_left_alone_and_says_where_it_is() {
        let state = tempfile::tempdir().expect("a state directory");
        let left = older_sessions(state.path());
        let path = tree_at(state.path());
        let newer = serde_json::to_string(&left)
            .expect("a desk is json")
            .replace(
                &format!("\"schema_version\":{STATE_SCHEMA_VERSION}"),
                "\"schema_version\":9999",
            );
        std::fs::write(&path, &newer).expect("the tree file is written");

        let desk = crate::desk_doc::DeskDoc::load(Some(state.path()), "ada");
        match desk.opened() {
            crate::desk_doc::Kept::Refused(StateNotice::NewerSchema { found, .. }) => {
                assert_eq!(found, 9999, "the build that wrote it is named");
            }
            crate::desk_doc::Kept::Refused(other) => panic!("{other:?}"),
            crate::desk_doc::Kept::Desk(_) | crate::desk_doc::Kept::None => {
                panic!("a desk from a newer build is not opened");
            }
        }
        assert_eq!(
            std::fs::read_to_string(&path).expect("the desk is still there"),
            newer,
            "nothing was written over it"
        );
        assert!(
            PathBuf::from(format!("{}.not-opened", path.display())).exists(),
            "the reason is left beside the desk, for anyone not looking at the screen"
        );
        assert!(
            !crate::desk_doc::path(state.path()).exists(),
            "and no document was written in its place"
        );
    }
}
