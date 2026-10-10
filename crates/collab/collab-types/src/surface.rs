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
use tilo_resource::surface::Surface;

use crate::claim::{ClaimedAction, UnitPath};
use crate::receipt::{ReceiptSurface, Record, SurfaceSource};

/// The item step standing for any item in a footprint.
pub const ANY_ITEM: &str = "*";

/// An app's surface file: every resource kind it publishes.
#[derive(Clone, Debug, Deserialize)]
pub struct SurfaceFile {
    pub resources: Vec<Surface>,
}

impl SurfaceFile {
    /// Parse a `surface.toml`.
    ///
    /// # Errors
    /// Bytes that are not a surface file.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        toml::from_str(text).map_err(|error| error.to_string())
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

/// The dotted field path of `path` (`comments.*.text`), items as `*`.
fn dotted(path: &[PathStep]) -> String {
    path.iter()
        .map(|step| match step {
            PathStep::Field(key) | PathStep::Key(key) => key.clone(),
            PathStep::Item(_) | PathStep::Cell(..) => ANY_ITEM.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl KindSurface {
    /// Every unit of the resource, items as [`ANY_ITEM`], and the record.
    fn every_unit(&self) -> Vec<UnitPath> {
        let mut units = vec![Vec::new()];
        for field in self.0.fields.iter().filter(|field| field.unit) {
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
            units.push(unit);
        }
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
        let fields = &self.0.fields;
        if path.is_empty() {
            return fields.iter().any(|field| !field.readonly);
        }
        let dotted = dotted(path);
        fields
            .iter()
            .any(|field| field.path == dotted && !field.readonly)
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
}

type ReadContent = dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync;

/// Surfaces read from a content store by the hash each op names, parsed
/// once per hash.
pub struct ContentSurfaces {
    read: Box<ReadContent>,
    parsed: Mutex<BTreeMap<[u8; 32], Arc<SurfaceFile>>>,
}

impl ContentSurfaces {
    /// Surfaces whose bytes `read` returns by lowercase hex hash.
    pub fn new(read: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self {
            read: Box::new(read),
            parsed: Mutex::new(BTreeMap::new()),
        }
    }

    fn file(&self, hash: &[u8; 32]) -> Option<Arc<SurfaceFile>> {
        if let Some(file) = self.parsed.lock().ok()?.get(hash) {
            return Some(Arc::clone(file));
        }
        let hex = hex::encode(hash);
        let bytes = (self.read)(&hex)?;
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
        _: &Record<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        let file = self.file(named?)?;
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
path = "created"
readonly = true
stored = "value"
title = "Created"
type = "u64"

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
            ContentSurfaces::new(move |wanted| (wanted == hex).then(|| FILE.as_bytes().to_vec())),
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
            surface.verb("comment"),
            Some((ActionKind::Undoable, Some("comments".into())))
        );
        assert!(surface.editable(&[PathStep::Field("body".into())]));
        assert!(!surface.editable(&[PathStep::Field("created".into())]));
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

        let forged = ContentSurfaces::new(|_| Some(b"[[resources]]".to_vec()));
        assert!(forged.surface(&record, None, Some(&hash)).is_none());
    }
}
