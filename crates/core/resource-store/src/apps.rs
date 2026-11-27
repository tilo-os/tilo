//! The apps a workspace runs: one resource of the system's own kind per app, naming the
//! version the workspace runs and who installed it.
//!
//! The kind is the system's, declared once for every workspace's store ([`schema`]). Installing
//! an app writes its record, uninstalling removes it, and undoing either takes it back, as any
//! change is taken back. Every member reads it; writing it takes the workspace's `install-app`
//! permission, which the workspace's admission of a member's write asks of its author.

use std::collections::BTreeMap;

use crate::Store;
use crate::schema::{Expected, Field, FieldKind, Schema, Type, Verb, Writes};
use collab_types::store::{Cell, Value};

/// The kind of an app the workspace runs.
pub const KIND: &str = "workspace.apps";

/// The app the records are written as: the workspace's own, which no package is.
pub const APP: &str = "tilo.workspace";

/// The verb that installs an app, or moves it to another version.
pub const INSTALL: &str = "install-app";

/// The verb that uninstalls an app.
pub const UNINSTALL: &str = "uninstall-app";

/// The fields of an app's record: its id, the release the workspace runs, who installed it and
/// when (ns since the epoch).
pub const ID: &str = "app";
pub const VERSION: &str = "version";
pub const SURFACE: &str = "surface";
pub const PACKAGE: &str = "package";
pub const PUBLISHER: &str = "publisher";
pub const FILE: &str = "file";
pub const FLATPAK: &str = "flatpak";
pub const DEV: &str = "dev";
pub const PINNED: &str = "pinned";
pub const INSTALLED_BY: &str = "installed_by";
pub const INSTALLED_AT: &str = "installed_at";

/// Ties a record's id to the app it names ([`id_of`]).
const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x2f8b_93c1_7a44_4d0e_9a61_6170_7073_7631);

/// The id of the record of `app`: one per app, so every device writing it writes the same one.
#[must_use]
pub fn id_of(app: &str) -> String {
    format!(
        "app-{}",
        uuid::Uuid::new_v5(&NAMESPACE, app.as_bytes()).simple()
    )
}

/// An app's record as the store holds it, at one of its versions.
pub type Fields = BTreeMap<String, Cell>;

/// Each app's record by id: the fields it holds now (`None` once uninstalled), and the fields each
/// of its versions held, oldest first ([`read`]).
pub type Records = BTreeMap<String, (Option<Fields>, Vec<Fields>)>;

/// The record of each app the store holds or held, by id: the fields it holds now (`None` for an
/// app uninstalled since), and the fields each of its versions held, oldest first. What the
/// workspace runs, and every release it ran each app with.
///
/// # Errors
/// A document holding one does not read.
pub fn read(store: &mut Store) -> Result<Records, String> {
    let mut ids: Vec<String> = store.ids_of(KIND).map(str::to_owned).collect();
    ids.extend(store.removed(KIND, None, &|_| true));
    ids.sort_unstable();
    ids.dedup();
    let mut apps = BTreeMap::new();
    for id in ids {
        let versions = store
            .read_versions(KIND, &id, None)
            .map_err(|error| error.to_string())?;
        let ran = versions
            .iter()
            .filter_map(|version| store.at_change(KIND, &id, &version.change, true))
            .filter_map(|(held, _)| held.map(|held| held.fields))
            .collect();
        let now = store
            .get(KIND, &id, &|_| true)
            .map(|held| held.fields.clone());
        apps.insert(id, (now, ran));
    }
    Ok(apps)
}

/// Whether the store ever held a record of an app the workspace runs, removed since or not: a
/// workspace whose apps were brought over to it, or that kept them in it from the start.
#[must_use]
pub fn ever_written(store: &Store) -> bool {
    store.index().documents().any(holds_records)
}

/// Whether the store document `doc` is one the records of the apps the workspace runs are kept
/// in.
#[must_use]
pub fn holds_records(doc: &str) -> bool {
    doc.split_once('/')
        .and_then(|(_, owner)| owner.rsplit_once('#'))
        .is_some_and(|(owner, _)| owner == KIND)
}

/// The text `field` holds in `fields`; empty when it holds none.
#[must_use]
pub fn text(fields: &Fields, field: &str) -> String {
    match fields.get(field).and_then(Cell::one) {
        Some(Value::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

/// Whether `field` holds `true` in `fields`.
#[must_use]
pub fn flag(fields: &Fields, field: &str) -> bool {
    matches!(
        fields.get(field).and_then(Cell::one),
        Some(Value::Bool(true))
    )
}

/// The number `field` holds in `fields`; zero when it holds none.
#[must_use]
pub fn number(fields: &Fields, field: &str) -> i64 {
    match fields.get(field).and_then(Cell::one) {
        Some(Value::Int(number)) => *number,
        _ => 0,
    }
}

/// The app kind and its verbs, as every workspace's store declares them.
#[must_use]
pub fn schema() -> Schema {
    let field = |name: &str, value: Expected, optional: bool| Field {
        name: name.to_owned(),
        kind: FieldKind::Scalar,
        value,
        optional,
    };
    let fields = vec![
        field(ID, Expected::Text, false),
        field(VERSION, Expected::Text, false),
        field(SURFACE, Expected::Text, true),
        field(PACKAGE, Expected::Text, true),
        field(PUBLISHER, Expected::Text, true),
        field(FILE, Expected::Text, true),
        field(FLATPAK, Expected::Text, true),
        field(DEV, Expected::Bool, true),
        field(PINNED, Expected::Bool, true),
        field(INSTALLED_BY, Expected::Text, false),
        field(INSTALLED_AT, Expected::Int, true),
    ];
    let writes = || {
        BTreeMap::from([(
            KIND.to_owned(),
            Writes {
                fields: fields.iter().map(|field| field.name.clone()).collect(),
                sets_only: false,
            },
        )])
    };
    Schema {
        kind: KIND.to_owned(),
        types: vec![Type {
            name: KIND.to_owned(),
            fields: fields.clone(),
            nested: Vec::new(),
            live: None,
            founding: Vec::new(),
            origin: None,
            version: 1,
        }],
        verbs: [INSTALL, UNINSTALL]
            .into_iter()
            .map(|name| Verb {
                name: name.to_owned(),
                writes: writes(),
                ..Verb::default()
            })
            .collect(),
        readers: BTreeMap::new(),
        owners: BTreeMap::new(),
    }
}
