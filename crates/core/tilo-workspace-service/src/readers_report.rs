//! What a workspace holds where its types' rules do not place it, counted without reading any
//! record's content: what an operator reads of a server's stored workspaces.
//!
//! A type that names its readers (`(readers: author until sent)`) seals each of its records to
//! those readers' key group. A record of such a kind held in the document every member reads —
//! one written before its type named its readers — reaches every member's device, and the
//! workspace moves it into its rule's document when it opens. What this counts is how much such
//! work a state directory has waiting, and how many members each workspace reaches, and nothing
//! else. `tilo-runtime --readers-report <workspaces> <surfaces>` prints it from a server's state
//! directory with its daemon stopped.

use std::collections::BTreeMap;
use std::path::Path;

use tilo_resource_store::Store;

/// What one workspace holds: how many members it has, and how many records of each kind sit in
/// a document every member reads while their type's rule names fewer readers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceReport {
    /// Its members: people and agents.
    pub members: usize,
    /// Per kind, how many of its records are held where their rule does not place them.
    pub misplaced: BTreeMap<String, usize>,
}

impl WorkspaceReport {
    /// The report of `store`, as it stands with the types its rules were declared by, and
    /// `members` people and agents of the workspace. Reading it writes nothing.
    #[must_use]
    pub fn of(store: &Store, members: usize) -> Self {
        let mut misplaced: BTreeMap<String, usize> = BTreeMap::new();
        for (kind, _) in store.misplaced_records() {
            let count = misplaced.entry(kind).or_default();
            *count = count.saturating_add(1);
        }
        Self { members, misplaced }
    }

    /// The lines an operator reads, given each workspace's name: counts only, never a record's
    /// content or its id.
    #[must_use]
    pub fn lines(&self, workspace: &str) -> Vec<String> {
        let held: usize = self.misplaced.values().sum();
        let mut lines = vec![format!(
            "{workspace}: {} members, {held} records held where their readers are not",
            self.members
        )];
        for (kind, count) in &self.misplaced {
            lines.push(format!("  {kind}: {count}"));
        }
        lines
    }
}

/// What each workspace kept under the workspaces directory `workspaces` holds, read from its
/// stored documents with no workspace opened and no record moved: each with the name
/// its governance gives it. `surfaces` is a directory of apps' compiled surfaces — the apps
/// payload a build lays out, or a device's kept surfaces — which name each kind's readers.
///
/// # Errors
/// A directory cannot be read, or a workspace in it is of an older layout that opening would
/// rewrite, which a report never does.
pub fn read_state(
    workspaces: &Path,
    surfaces: &Path,
) -> Result<Vec<(String, WorkspaceReport)>, String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(workspaces)
        .map_err(|error| format!("{}: {error}", workspaces.display()))?
        .flatten()
        .map(|entry| entry.path())
        // A workspace's record is `<id>.workspace`; the files beside it, which say what it
        // filed and whether it was released, and the directories of its parts and store, have
        // other names.
        .filter(|path| path.is_file() && path.extension().is_some_and(|kind| kind == "workspace"))
        .collect();
    files.sort();
    let surfaces = surfaces_of(surfaces)?;
    let mut reports = Vec::with_capacity(files.len());
    for path in files {
        reports.push(crate::workspaces::kept_report(&path, &surfaces)?);
    }
    Ok(reports)
}

/// The app surfaces kept under `dir`, each in the canonical compiled form: in the directory
/// itself, or in an app's own directory under it (`<app>/surface.json`, as a build lays them
/// out). Files that are no surface are passed over.
fn surfaces_of(dir: &Path) -> Result<Vec<tilo_actions::surface::Surface>, String> {
    let mut paths = Vec::new();
    surface_files(dir, 0, &mut paths)?;
    let mut surfaces = Vec::new();
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(file) = collab_types::surface::SurfaceFile::compiled(&bytes) else {
            continue;
        };
        surfaces.extend(file.resources);
    }
    Ok(surfaces)
}

/// The files under `dir`, and under the directories a couple of levels below it, that may be
/// app surfaces.
fn surface_files(
    dir: &Path,
    depth: usize,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 2 {
                surface_files(&path, depth.saturating_add(1), out)?;
            }
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// The types and rules the surfaces in `surfaces` declare, as a store declares them before it
/// places a write: each given to `declare`.
pub(crate) fn declare_surfaces(
    store: &mut Store,
    surfaces: &[tilo_actions::surface::Surface],
) {
    for surface in surfaces {
        store.declare(tilo_resource_store::Schema::from_surface(surface));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::store::wire::Command;
    use collab_types::store::{Cell, Resource, Value};
    use std::collections::BTreeMap;
    use tilo_actions::surface::ReaderRule;
    use tilo_resource_store::{Field, FieldKind, Schema, Type};

    const DRAFT: &str = "test.readers.Draft";

    /// A store that declares a draft whose type names its readers, holding two drafts of
    /// `ada` where every member reads them, as a workspace written before its type did.
    fn misplaced_drafts() -> Store {
        let mut store = Store::new();
        store.declare(Schema {
            types: vec![Type {
                name: DRAFT.to_owned(),
                fields: vec![
                    Field {
                        name: "subject".to_owned(),
                        kind: FieldKind::Scalar,
                        value: tilo_resource_store::Expected::Any,
                        optional: true,
                    },
                    Field {
                        name: "sent".to_owned(),
                        kind: FieldKind::Scalar,
                        value: tilo_resource_store::Expected::Any,
                        optional: true,
                    },
                ],
                nested: Vec::new(),
                live: None,
                founding: Vec::new(),
            }],
            ..Schema::default()
        });
        let stamp = tilo_resource_store::Stamp {
            verb: "type".to_owned(),
            invoker: "ada".to_owned(),
            ..tilo_resource_store::Stamp::default()
        };
        for id in ["one", "two", "three"] {
            let sent = id == "three";
            tilo_resource_store::wire::write(
                &mut store,
                Command::Put {
                    kind: DRAFT.to_owned(),
                    id: id.to_owned(),
                    resource: Resource {
                        readers: collab_types::EVERY_MEMBER.to_owned(),
                        parent: None,
                        order: None,
                        fields: BTreeMap::from([
                            ("subject".to_owned(), Cell::One(Value::from("Quarterly plan"))),
                            ("sent".to_owned(), Cell::One(Value::Bool(sent))),
                        ]),
                    },
                    stamp: stamp.clone(),
                },
            )
            .expect("the draft is written");
        }
        // Its type names its readers now: the two unsent drafts sit where the rule does not
        // place them, and the sent one is every member's, as its rule says.
        store.declare(Schema {
            types: Vec::new(),
            readers: BTreeMap::from([(
                DRAFT.to_owned(),
                ReaderRule::Author {
                    until: Some("sent".to_owned()),
                },
            )]),
            ..Schema::default()
        });
        store
    }

    #[test]
    fn the_report_counts_misplaced_records_by_kind_and_members() {
        let store = misplaced_drafts();
        let report = WorkspaceReport::of(&store, 3);
        assert_eq!(report.members, 3);
        assert_eq!(report.misplaced.len(), 1, "one kind holds them");
        assert_eq!(
            report.misplaced.get(DRAFT),
            Some(&2),
            "the two unsent drafts are held where every member reads them"
        );
        assert_eq!(
            report.lines("Mail"),
            vec![
                "Mail: 3 members, 2 records held where their readers are not".to_owned(),
                format!("  {DRAFT}: 2"),
            ]
        );
    }
}
