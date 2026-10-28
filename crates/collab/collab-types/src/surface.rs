//! An app's public surface as a receiver reads it: the `surface.toml` its
//! package ships, fetched by content hash.
//!
//! A signed op names the surface its changes are decided against
//! ([`crate::SignedOp::surface`]). A receiver reads the bytes from its
//! content store by that hash (`tilo-content`: they reach a device from the
//! registry or its peers as any content does), parses them once, and
//! decides each change's claim against the resource the record is
//! ([`crate::receipt::ReceiptSurface`]). Until the bytes are here the op is
//! held.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tilo_action_policy::ActionKind;
use tilo_actions::PathStep;
use tilo_actions::surface::Surface;

use crate::claim::{ClaimedAction, UnitPath};
use crate::receipt::{ReceiptSurface, Record, SurfaceSource};

/// The item step standing for any item in a footprint.
pub const ANY_ITEM: &str = "*";

/// An app's surface file: every resource kind it publishes.
#[derive(Clone, Debug, Deserialize)]
pub struct SurfaceFile {
    pub resources: Vec<Surface>,
    /// The resource kinds of other apps this app reads (`reads = [kind]`):
    /// a declared, read-only, live join to their records. The app changes
    /// them only by invoking their actions.
    #[serde(default)]
    pub reads: Vec<String>,
}

impl SurfaceFile {
    /// Parse a `surface.toml`.
    ///
    /// # Errors
    /// Bytes that are not a surface file.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        // Through a JSON value: the surface types are then read by the
        // JSON deserializer the rest of the runtime already compiles, not by
        // a second, TOML-specific copy of their whole deserializer.
        let value: serde_json::Value = toml::from_str(text).map_err(|error| error.to_string())?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// Whether this app declares that it reads a resource kind of the app
    /// whose surface is `other`.
    #[must_use]
    pub fn reads_any_of(&self, other: &Self) -> bool {
        other
            .resources
            .iter()
            .any(|surface| self.reads.contains(&surface.kind))
    }

    /// The resource of kind `kind`, or the app's first (its own record)
    /// when no kind is known.
    #[must_use]
    pub fn resource(&self, kind: Option<&str>) -> Option<&Surface> {
        match kind {
            Some(kind) => self.resources.iter().find(|surface| surface.kind == kind),
            None => self.resources.first(),
        }
    }
}

/// One resource kind of a surface file, as receipt decides against it.
#[derive(Clone, Debug)]
pub struct KindSurface(pub Surface);

/// The unit a unit field is, items as [`ANY_ITEM`].
fn unit_of(field: &tilo_actions::surface::FieldSurface) -> UnitPath {
    let mut unit = Vec::new();
    for key in field.path.split('.') {
        unit.push(if key == ANY_ITEM {
            PathStep::Item(ANY_ITEM.to_owned())
        } else {
            PathStep::Field(key.to_owned())
        });
    }
    if matches!(field.stored.as_str(), "collection" | "keyed") {
        unit.push(PathStep::Item(ANY_ITEM.to_owned()));
    }
    unit
}

impl KindSurface {
    /// Every unit of the resource, items as [`ANY_ITEM`], and the record.
    fn every_unit(&self) -> Vec<UnitPath> {
        let mut units = vec![Vec::new()];
        units.extend(self.0.fields.iter().filter(|field| field.unit).map(unit_of));
        units
    }
}

impl ReceiptSurface for KindSurface {
    fn kind(&self) -> &str {
        &self.0.kind
    }

    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        self.0
            .verbs
            .iter()
            .find(|verb| verb.id == action)
            .map(|verb| (verb.kind, verb.field.clone()))
    }

    fn editable(&self, path: &[PathStep]) -> bool {
        if path.is_empty() {
            return self.0.fields.iter().any(|field| !field.readonly);
        }
        // An edit inside a field (an outbox entry settled, a map's key)
        // edits the field it reaches, as the app offers it.
        self.0.editable(path)
    }

    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath> {
        match action {
            ClaimedAction::Verb { action, item } => {
                self.0.footprint(&action.action, item.as_deref())
            }
            ClaimedAction::Edit { path, .. } if path.is_empty() => self.every_unit(),
            ClaimedAction::Edit { path, .. } => vec![self.0.edit_unit(path)],
            ClaimedAction::Group(members) => members
                .iter()
                .flat_map(|member| self.footprint(member))
                .collect(),
            // Taking back or bringing back a step, a record's first content
            // and a migration may touch any unit; what decides them is the
            // grant on their action.
            ClaimedAction::History(_) | ClaimedAction::Framework(_) => self.every_unit(),
        }
    }

    fn outboxes(&self) -> Vec<UnitPath> {
        self.0
            .fields
            .iter()
            .filter(|field| field.unit && tilo_actions::surface::is_outbox_type(&field.ty))
            .map(unit_of)
            .collect()
    }
}

/// Reads content by lowercase hex hash, from the record's room and app.
type ReadContent = dyn Fn(&Record<'_>, &str) -> Option<Vec<u8>> + Send + Sync;

/// Surfaces read from a content store by the hash each op names, parsed
/// once per hash.
pub struct ContentSurfaces {
    read: Box<ReadContent>,
    parsed: Mutex<BTreeMap<[u8; 32], Arc<SurfaceFile>>>,
}

impl ContentSurfaces {
    /// Surfaces whose bytes `read` returns by lowercase hex hash, wherever
    /// the record's room and app keep content.
    pub fn new(
        read: impl Fn(&Record<'_>, &str) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Box::new(read),
            parsed: Mutex::new(BTreeMap::new()),
        }
    }

    fn file(&self, record: &Record<'_>, hash: &[u8; 32]) -> Option<Arc<SurfaceFile>> {
        if let Some(file) = self.parsed.lock().ok()?.get(hash) {
            return Some(Arc::clone(file));
        }
        let hex = hex::encode(hash);
        let bytes = (self.read)(record, &hex)?;
        // Only the bytes the hash names are the surface.
        if blake3::hash(&bytes).as_bytes() != hash {
            return None;
        }
        let file = Arc::new(SurfaceFile::parse(&bytes).ok()?);
        self.parsed.lock().ok()?.insert(*hash, Arc::clone(&file));
        Some(file)
    }
}

impl SurfaceSource for ContentSurfaces {
    fn surface(
        &self,
        record: &Record<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        let file = self.file(record, named?)?;
        let surface = file.resource(kind)?.clone();
        Some(Arc::new(KindSurface(surface)))
    }
}

/// Whether the footprint unit `allowed` covers the written unit `unit`:
/// the same steps, an [`ANY_ITEM`] item standing for any item.
#[must_use]
pub fn covers(allowed: &[PathStep], unit: &[PathStep]) -> bool {
    allowed.len() == unit.len()
        && allowed
            .iter()
            .zip(unit)
            .all(|(allowed, unit)| match (allowed, unit) {
                (PathStep::Item(any), PathStep::Item(_)) if any == ANY_ITEM => true,
                (allowed, unit) => allowed == unit,
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionKey;

    const FILE: &str = r#"
[[resources]]
kind = "notes.page"
label = "Page"
version = 1

[[resources.fields]]
owner = "page"
path = "body"
stored = "value"
title = "Body"
type = "String"

[[resources.fields]]
owner = "page"
path = "comments"
stored = "collection"
title = "Comments"
type = "Collection<Comment>"
unit = true

[[resources.fields]]
owner = "page"
path = "shares"
title = "Shares"
type = "tilo_resource::Outbox<Share>"
unit = true

[[resources.fields]]
owner = "page"
path = "drafts"
title = "Drafts"
type = "Vec<Outbox>"

[[resources.fields]]
owner = "page"
path = "created"
readonly = true
stored = "value"
title = "Created"
type = "u64"

[[resources.fields]]
owner = "page"
path = "log"
readonly = true
stored = "collection"
title = "Log"
type = "Collection<Entry>"
unit = true

[[resources.fields]]
owner = "entry"
path = "log.*.text"
readonly = true
type = "String"

[[resources.fields]]
owner = "entry"
path = "log.*.reactions"
readonly = true
type = "ui::tilo_resource::Reactions"

[[resources.verbs]]
id = "comment"
kind = "undoable"
label = "Comment"
field = "comments"
params = []
writes = ["record", "comments.*"]
"#;

    fn surfaces() -> (ContentSurfaces, [u8; 32]) {
        let hash = *blake3::hash(FILE.as_bytes()).as_bytes();
        let hex = hex::encode(hash);
        (
            ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| FILE.as_bytes().to_vec())
            }),
            hash,
        )
    }

    /// A surface fetched by its hash decides kinds, editable fields and
    /// footprints; other bytes under that hash are not it.
    #[test]
    fn a_surface_named_by_hash_decides_claims() {
        let (source, hash) = surfaces();
        let record = Record {
            room: "r",
            app: "notes",
            record: "p1",
        };
        assert!(
            source.surface(&record, None, None).is_none(),
            "named nothing"
        );
        assert!(
            source.surface(&record, None, Some(&[0; 32])).is_none(),
            "not here"
        );
        let surface = source
            .surface(&record, Some("notes.page"), Some(&hash))
            .unwrap();
        assert_eq!(surface.kind(), "notes.page");
        assert_eq!(
            surface.outboxes(),
            vec![vec![PathStep::Field("shares".into())]],
            "its outbox, and no other field"
        );
        assert_eq!(
            surface.verb("comment"),
            Some((ActionKind::Undoable, Some("comments".into())))
        );
        assert!(surface.editable(&[PathStep::Field("body".into())]));
        assert!(!surface.editable(&[PathStep::Field("created".into())]));
        assert!(
            surface.editable(&[
                PathStep::Field("comments".into()),
                PathStep::Item("c1".into()),
            ]),
            "an item of an editable collection is editable"
        );
        assert!(
            !surface.editable(&[PathStep::Field("created".into()), PathStep::Key("x".into()),])
        );
        let entry = |field: &str| {
            [
                PathStep::Field("log".into()),
                PathStep::Item("e1".into()),
                PathStep::Field(field.into()),
            ]
        };
        assert!(
            surface.editable(&entry("reactions")),
            "a read-only item still takes reactions"
        );
        assert!(!surface.editable(&entry("text")));
        let comment = ClaimedAction::Verb {
            action: ActionKey::new("notes", "notes.page", "comment"),
            item: Some("c1".into()),
        };
        let footprint = surface.footprint(&comment);
        let c1 = [
            PathStep::Field("comments".into()),
            PathStep::Item("c1".into()),
        ];
        let c2 = [
            PathStep::Field("comments".into()),
            PathStep::Item("c2".into()),
        ];
        assert!(footprint.iter().any(|unit| covers(unit, &c1)));
        assert!(!footprint.iter().any(|unit| covers(unit, &c2)));
        let whole = surface.footprint(&ClaimedAction::whole_record_edit(String::new()));
        assert!(
            whole.iter().any(|unit| covers(unit, &c2)),
            "a whole-record edit writes any unit"
        );

        let forged = ContentSurfaces::new(|_, _| Some(b"[[resources]]".to_vec()));
        assert!(forged.surface(&record, None, Some(&hash)).is_none());
    }

    #[test]
    fn every_committed_surface_parses() {
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let mut parsed = 0;
        for app in std::fs::read_dir(apps).unwrap() {
            let file = app.unwrap().path().join("surface.toml");
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            let surface = SurfaceFile::parse(&bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            assert!(surface.resource(None).is_some(), "{}", file.display());
            parsed += 1;
        }
        assert!(parsed > 10, "found the apps' surfaces");
    }
}
