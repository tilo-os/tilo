//! An app's public surface as a receiver reads it: the `surface.toml` its
//! package ships, kept as content by hash.
//!
//! A signed op names the surface its changes are decided against
//! ([`crate::SignedOp::surface`]). A surface is kept and addressed in one
//! form ([`canonical`]): its JSON with every object's keys in order, so a
//! package's `surface.toml` and a built-in app's compiled form of the same
//! surface have one address. A receiver reads those bytes from its content
//! store by that hash (`tilo-content`: they reach a device from the registry
//! or its peers as any content does), parses them once, and decides each
//! change's claim against the resource it names
//! ([`crate::receipt::ReceiptSurface`]). Until the bytes are here the op is
//! held.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tilo_action_policy::ActionKind;
use tilo_actions::PathStep;
use tilo_actions::surface::Surface;
use tilo_lock::MutexRecover;

use crate::claim::{ClaimedAction, UnitPath};
use crate::receipt::{ReceiptSurface, SurfaceSource, WrittenDocument};

/// The item step for any item in a footprint.
pub const ANY_ITEM: &str = "*";

/// The bytes a surface is kept and addressed by: `value` as JSON, every
/// object's keys in order, so the same surface has one address however it was
/// written.
#[must_use]
pub fn canonical(value: &serde_json::Value) -> String {
    tilo_canonical_json::to_string(value)
}

/// [`canonical`] of `json`, when it is JSON.
#[must_use]
pub fn canonical_json(json: &str) -> Option<String> {
    serde_json::from_str(json)
        .ok()
        .map(|value| canonical(&value))
}

/// An app's surface file: every resource kind it publishes.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SurfaceFile {
    pub resources: Vec<Surface>,
    /// The resource kinds of other apps this app reads (`reads = [kind]`):
    /// a declared, read-only, live join to their records. The app changes
    /// them only by invoking their actions.
    #[serde(default)]
    pub reads: Vec<String>,
}

impl SurfaceFile {
    /// Parse a `surface.toml`, and the bytes it is kept and addressed by
    /// ([`canonical`]).
    ///
    /// # Errors
    /// Bytes that are not a surface file.
    pub fn kept(bytes: &[u8]) -> Result<(Self, String), String> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        // Through a JSON value: the surface types are then read by the
        // JSON deserializer the rest of the runtime already compiles, not by
        // a second, TOML-specific copy of their whole deserializer.
        let value: serde_json::Value = toml::from_str(text).map_err(|error| error.to_string())?;
        let file = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        Ok((file, canonical(&value)))
    }

    /// Parse a `surface.toml`.
    ///
    /// # Errors
    /// Bytes that are not a surface file.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Self::kept(bytes).map(|(file, _)| file)
    }

    /// Read a surface in the JSON form `tilo_resource::committed_surface!`
    /// wrote from its `surface.toml` at build time.
    ///
    /// # Errors
    /// JSON that is not a surface.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
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

    /// The resource `kind` names.
    #[must_use]
    pub fn resource(&self, kind: &str) -> Option<&Surface> {
        self.resources.iter().find(|surface| surface.kind == kind)
    }

    /// The app's own resource, the first it declares: the record an app writes whole and a
    /// change names no kind of (a native edit, an undo, a creation). A change to a resource
    /// store's document names the kinds of the records it writes, and is never resolved here.
    #[must_use]
    pub fn own_resource(&self) -> Option<&Surface> {
        self.resources.first()
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

    fn action_name<'a>(&'a self, action: &str) -> Option<&'a str> {
        self.0
            .verbs
            .iter()
            .find(|verb| verb.id == action)
            .map(tilo_actions::surface::VerbSurface::offered)
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
            .filter(|field| field.unit && field.ty.is_outbox())
            .map(unit_of)
            .collect()
    }
}

/// Reads content by lowercase hex hash, from the record's workspace and app.
type ReadContent = dyn Fn(&WrittenDocument<'_>, &str) -> Option<Vec<u8>> + Send + Sync;

/// Surfaces read from a content store by the hash each op names, parsed
/// once per hash.
pub struct ContentSurfaces {
    read: Box<ReadContent>,
    parsed: Mutex<BTreeMap<[u8; 32], Arc<SurfaceFile>>>,
}

impl ContentSurfaces {
    /// Surfaces whose bytes `read` returns by lowercase hex hash, wherever
    /// the record's workspace and app keep content.
    pub fn new(
        read: impl Fn(&WrittenDocument<'_>, &str) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Box::new(read),
            parsed: Mutex::new(BTreeMap::new()),
        }
    }

    fn file(&self, document: &WrittenDocument<'_>, hash: &[u8; 32]) -> Option<Arc<SurfaceFile>> {
        if let Some(file) = self.parsed.lock_recover().get(hash) {
            return Some(Arc::clone(file));
        }
        let hex = hex::encode(hash);
        let bytes = (self.read)(document, &hex)?;
        // Only the bytes the hash names are the surface.
        if blake3::hash(&bytes).as_bytes() != hash {
            return None;
        }
        // Kept since surfaces are addressed by their JSON form, or by an
        // earlier build, which kept and addressed each as its `surface.toml`:
        // the changes that build wrote name those bytes, and stay readable.
        let text = std::str::from_utf8(&bytes).ok()?;
        let file = Arc::new(
            SurfaceFile::from_json(text)
                .or_else(|_| SurfaceFile::parse(&bytes))
                .ok()?,
        );
        self.parsed.lock_recover().insert(*hash, Arc::clone(&file));
        Some(file)
    }
}

impl SurfaceSource for ContentSurfaces {
    fn surface(
        &self,
        document: &WrittenDocument<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        let file = self.file(document, named?)?;
        let surface = match kind {
            Some(kind) => file.resource(kind)?.clone(),
            None => file.own_resource()?.clone(),
        };
        Some(Arc::new(KindSurface(surface)))
    }
}

/// Whether the footprint unit `allowed` covers the written unit `unit`:
/// the same steps, an [`ANY_ITEM`] item for any item.
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
writes = ["document", "comments.*"]
"#;

    fn surfaces() -> (ContentSurfaces, [u8; 32]) {
        let (_, kept) = SurfaceFile::kept(FILE.as_bytes()).expect("the fixture is a surface");
        let hash = *blake3::hash(kept.as_bytes()).as_bytes();
        let hex = hex::encode(hash);
        (
            ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| kept.as_bytes().to_vec())
            }),
            hash,
        )
    }

    tilo_test_env::scenario! {
        /// An app's surface file: the committed ones parse, and one named by hash decides claims.
        fn a_surface_file_parses_and_decides_claims_by_hash;

        phase a_surface_named_by_hash_decides_claims;
        // A surface fetched by its hash decides kinds, editable fields and
        // footprints; other bytes under that hash are not it.
        {
            let (source, hash) = surfaces();
            let document = WrittenDocument {
                workspace: "r",
                app: "notes",
                document: "p1",
            };
            assert!(
                source.surface(&document, None, None).is_none(),
                "named nothing"
            );
            assert!(
                source.surface(&document, None, Some(&[0; 32])).is_none(),
                "not here"
            );
            let surface = source
                .surface(&document, Some("notes.page"), Some(&hash))
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
            assert_eq!(
                surface.action_name("comment"),
                Some("comment"),
                "a native verb is published to the policy under its own id"
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
            let whole = surface.footprint(&ClaimedAction::whole_resource_edit(
                "notes.page".to_owned(),
            ));
            assert!(
                whole.iter().any(|unit| covers(unit, &c2)),
                "a whole-document edit writes any unit"
            );

            let forged = ContentSurfaces::new(|_, _| Some(b"[[resources]]".to_vec()));
            assert!(forged.surface(&document, None, Some(&hash)).is_none());
        }

        phase a_surface_an_earlier_build_kept_as_its_file_decides_claims;
        // An earlier build kept and named each surface as its `surface.toml`,
        // by the hash of those bytes: the changes it wrote stay decided.
        {
            let hash = *blake3::hash(FILE.as_bytes()).as_bytes();
            let hex = hex::encode(hash);
            let source = ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| FILE.as_bytes().to_vec())
            });
            let document = WrittenDocument {
                workspace: "r",
                app: "notes",
                document: "p1",
            };
            let surface = source
                .surface(&document, Some("notes.page"), Some(&hash))
                .expect("the file an earlier build kept is its surface");
            assert_eq!(surface.kind(), "notes.page");
            assert_eq!(
                surface.verb("comment"),
                Some((ActionKind::Undoable, Some("comments".into())))
            );
        }

        phase every_committed_surface_parses_and_its_generated_form_reads_as_it_does;
        {
            let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
            let mut parsed = 0;
            for app in std::fs::read_dir(apps).unwrap() {
                let file = app.unwrap().path().join("surface.toml");
                let Ok(bytes) = std::fs::read(&file) else {
                    continue;
                };
                let (surface, kept) = SurfaceFile::kept(&bytes)
                    .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
                assert!(surface.own_resource().is_some(), "{}", file.display());
                assert_eq!(
                    SurfaceFile::parse(&bytes).expect("it parses"),
                    surface,
                    "{}",
                    file.display()
                );
                // The form `committed_surface!` writes for a built-in app reads as the
                // file does, and is kept as the package's file of it is.
                let generated = serde_json::to_string(
                    &toml::from_str::<serde_json::Value>(
                        std::str::from_utf8(&bytes).expect("a surface file is UTF-8"),
                    )
                    .expect("a committed surface is TOML"),
                )
                .expect("a surface is JSON");
                assert_eq!(
                    SurfaceFile::from_json(&generated)
                        .unwrap_or_else(|error| panic!("{}: {error}", file.display())),
                    surface,
                    "{}",
                    file.display()
                );
                assert_eq!(
                    canonical_json(&generated).as_deref(),
                    Some(kept.as_str()),
                    "{} is one address as a package's file and as a built-in's",
                    file.display()
                );
                parsed += 1;
            }
            assert!(parsed > 10, "found the apps' surfaces");
        }
    }

    const CANONICAL_JSON_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    #[test]
    fn canonical_bytes_are_pinned_for_shared_corpus() {
        const PINNED_NESTED_DIGEST: [u8; 32] = [
            0x94, 0x8c, 0x6e, 0x61, 0xfa, 0x0a, 0xef, 0x3c, 0x6b, 0xf7, 0x26, 0xc5, 0x9a, 0xae,
            0xa9, 0x25, 0xe2, 0x72, 0x8f, 0x60, 0x66, 0xdb, 0xf3, 0x26, 0x2c, 0x35, 0x86, 0x70,
            0x24, 0x53, 0xef, 0x94,
        ];

        for &(name, input, expected) in CANONICAL_JSON_CORPUS {
            let value: serde_json::Value = serde_json::from_str(input).unwrap();
            assert_eq!(canonical(&value).as_bytes(), expected, "row {name}");
            assert_eq!(
                canonical_json(input).as_deref().map(str::as_bytes),
                Some(expected),
                "canonical_json row {name}"
            );
        }
        let nested: serde_json::Value = serde_json::from_str(CANONICAL_JSON_CORPUS[0].1).unwrap();
        assert_eq!(
            *blake3::hash(canonical(&nested).as_bytes()).as_bytes(),
            PINNED_NESTED_DIGEST
        );
    }
}
