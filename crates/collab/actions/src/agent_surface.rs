//! The one document an agent reads of an app's surface.
//!
//! [`TestApp::agent_surface`] answers what an agent sees of an app that is
//! running: every action it publishes, with its address, its label, its
//! description and its typed parameters. That vocabulary is what an agent
//! pays for on every turn it still carries, so the document says each shape
//! once: `actions` are the rows, and `$defs` holds the shapes their
//! parameter schemas repeat, which each of them names with a local `$ref`
//! ([`share_repeated_subschemas_among`](crate::params)).
//!
//! ## A format with a name and a number
//!
//! The document is what an app says to whatever reads it — an agent, a
//! script, a tool written in another language — so its root says what it is:
//! `format` names the document ([`AGENT_SURFACE_FORMAT`]) and `version` the
//! format it is written in ([`AGENT_SURFACE_VERSION`]). [`AgentSurface`] is
//! that root, stated once for every producer and every reader.
//!
//! A reader hands what it was given to [`AgentSurface::read`], which answers
//! the surface or the reason it is not one this build reads. It never
//! answers an empty surface for a document it did not understand: an agent
//! told there is nothing to do, while the app publishes seventy operations,
//! is worse off than one told to update.
//!
//! Before this format the snapshot was a bare array of rows, with no root to
//! carry a number (prealpha's `v0`). [`AgentSurface::read`] refuses it as
//! that, and refuses a document that names no version — the shape this
//! format had before it carried one — for the same reason, rather than
//! reading an array or a document that is not the one it speaks.
//!
//! ## What a narrowed surface states
//!
//! A surface may be narrowed to what its reader may have: what one window
//! publishes rather than another provider's ([`AgentSurface::retain`]). A
//! row that goes takes the definitions it alone named with it, so a surface
//! filtered for one caller never carries the shapes — nor the names,
//! descriptions and operations — of what that caller may not do. Every
//! `$ref` a kept row still names is a definition the document states, so a
//! reader that resolves one neither fetches nor guesses anything.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema_refs::{REF, scoped_from, share_repeated_subschemas_among};

/// What an [`AgentSurface`] calls itself: the `format` of every document
/// this crate writes.
pub const AGENT_SURFACE_FORMAT: &str = "tilo.agent-surface";

/// The version of that format: the `version` of every document this crate
/// writes. A change to what a row or a definition states is a new version,
/// and a reader of one version answers a document of another with
/// [`AgentSurfaceError::UnsupportedVersion`] rather than reading half of it.
pub const AGENT_SURFACE_VERSION: u32 = 1;

/// The row field holding a parameter's schema.
const SCHEMA: &str = "schema";
/// The row field holding a row's parameters.
const PARAMS: &str = "params";
/// The prefix of a local reference: `#/$defs/<name>`.
const LOCAL: &str = "#/$defs/";

/// The agent surface of one app, as an agent reads it: the rows, the shapes
/// their schemas repeat, and the format they are written in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSurface {
    /// [`AGENT_SURFACE_FORMAT`] in every document this crate writes. A
    /// reader keeps what it was handed, so it answers with what it read.
    pub format: String,
    /// [`AGENT_SURFACE_VERSION`] in every document this crate writes.
    pub version: u32,
    /// One row per published action: its `resource`, `action`,
    /// `resource_kind`, `label`, `description`, `operation` and `params`, as
    /// [`survey`](crate::door::survey) publishes them
    /// ([`ActionDescriptor`](crate::door::ActionDescriptor)). Whoever builds
    /// the surface sorts the rows by address.
    pub actions: Vec<Value>,
    /// The shapes the rows' parameter schemas repeat, once each, named by
    /// the local `$ref` of every place they apply. Kept by name, so a
    /// document's bytes do not depend on the order its shapes were found in.
    #[serde(rename = "$defs")]
    pub definitions: BTreeMap<String, Value>,
}

impl AgentSurface {
    /// `rows`, as the document this format's readers take: every shape those
    /// schemas repeat is stated once under `$defs` and named where it
    /// applies, and no other schema is touched.
    #[must_use]
    pub fn of_rows(mut rows: Vec<Value>) -> Self {
        let mut schemas: Vec<Value> = rows
            .iter()
            .flat_map(|row| parameters(row).iter())
            .filter_map(|param| param.get(SCHEMA).cloned())
            .collect();
        let definitions = match share_repeated_subschemas_among(&mut schemas) {
            Value::Object(definitions) => definitions.into_iter().collect(),
            _ => BTreeMap::new(),
        };
        let mut shared = schemas.into_iter();
        for row in &mut rows {
            let Some(Value::Array(params)) = row.get_mut(PARAMS) else {
                continue;
            };
            for param in params {
                // The parameters that publish a schema are the ones gathered
                // above, in this order.
                if param.get(SCHEMA).is_some()
                    && let Some(schema) = shared.next()
                {
                    param[SCHEMA] = schema;
                }
            }
        }
        Self {
            format: AGENT_SURFACE_FORMAT.to_owned(),
            version: AGENT_SURFACE_VERSION,
            actions: rows,
            definitions,
        }
    }

    /// `document`, read as this format: the surface it states, or the reason
    /// it is not a document of this version.
    ///
    /// # Errors
    /// [`AgentSurfaceError`] names what was handed instead: the prealpha bare
    /// array, a document of another format, another version of this one, or
    /// one that is malformed.
    pub fn read(document: &Value) -> Result<Self, AgentSurfaceError> {
        let Some(fields) = document.as_object() else {
            return Err(match document {
                Value::Array(_) => AgentSurfaceError::PrealphaArray,
                _ => AgentSurfaceError::NotADocument,
            });
        };
        let Some(version) = fields.get("version") else {
            return Err(AgentSurfaceError::Malformed {
                reason: "it names no `version`".to_owned(),
            });
        };
        let Some(version) = version.as_u64() else {
            return Err(AgentSurfaceError::Malformed {
                reason: format!("`version` is {version}, not a whole number"),
            });
        };
        if version != u64::from(AGENT_SURFACE_VERSION) {
            return Err(AgentSurfaceError::UnsupportedVersion { found: version });
        }
        match fields.get("format") {
            Some(Value::String(format)) if format == AGENT_SURFACE_FORMAT => {}
            Some(Value::String(found)) => {
                return Err(AgentSurfaceError::OtherFormat {
                    found: found.clone(),
                });
            }
            Some(other) => {
                return Err(AgentSurfaceError::Malformed {
                    reason: format!("`format` is {other}, not the format's name"),
                });
            }
            None => {
                return Err(AgentSurfaceError::Malformed {
                    reason: "it names no `format`".to_owned(),
                });
            }
        }
        serde_json::from_value(document.clone()).map_err(|error| AgentSurfaceError::Malformed {
            reason: error.to_string(),
        })
    }

    /// `bytes`, a document in JSON, read as [`Self::read`] reads a value.
    ///
    /// # Errors
    /// As [`Self::read`], and for bytes that are not JSON at all.
    pub fn read_json(bytes: &[u8]) -> Result<Self, AgentSurfaceError> {
        let document: Value =
            serde_json::from_slice(bytes).map_err(|error| AgentSurfaceError::Malformed {
                reason: error.to_string(),
            })?;
        Self::read(&document)
    }

    /// This surface as the JSON value a writer stores: the value whose bytes
    /// are what `serde_json` writes for `self`.
    #[must_use]
    pub fn document(&self) -> Value {
        #[expect(
            clippy::expect_used,
            reason = "an agent surface is a JSON document: nothing in it can fail to encode"
        )]
        let document = serde_json::to_value(self).expect("an agent surface encodes as JSON");
        document
    }

    /// The rows, one per published action.
    #[must_use]
    pub fn rows(&self) -> &[Value] {
        &self.actions
    }

    /// The shapes the rows' schemas repeat, by name.
    #[must_use]
    pub fn definitions(&self) -> &BTreeMap<String, Value> {
        &self.definitions
    }

    /// The row for `resource`'s `action`, when this surface publishes it.
    #[must_use]
    pub fn row(&self, resource: &str, action: &str) -> Option<&Value> {
        self.actions
            .iter()
            .find(|row| row["resource"] == resource && row["action"] == action)
    }

    /// `schema`, one of this surface's, with the definitions it names: what a
    /// reader holding a single parameter's schema validates, rather than
    /// keeping a resolver of its own
    /// ([`scoped`](crate::params::scoped) for a document in a value).
    #[must_use]
    pub fn scoped(&self, schema: &Value) -> Value {
        scoped_from(schema, || -> Map<String, Value> {
            self.definitions
                .iter()
                .map(|(name, shape)| (name.clone(), shape.clone()))
                .collect()
        })
    }

    /// Keep the rows `keep` admits, and the definitions those rows still
    /// name.
    ///
    /// A definition a dropped row alone named is dropped with it, so a
    /// surface narrowed to what a caller may do does not carry the shapes of
    /// what it may not; and every `$ref` a kept row or a kept definition
    /// still names stays in `$defs`, so the document remains one a reader
    /// resolves on its own.
    pub fn retain(&mut self, mut keep: impl FnMut(&Value) -> bool) {
        self.actions.retain(|row| keep(row));
        let mut pending: Vec<String> = Vec::new();
        for row in &self.actions {
            references(row, &mut pending);
        }
        let mut named: BTreeSet<String> = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            let Some(name) = definition_name(&reference) else {
                continue;
            };
            if named.insert(name.clone())
                && let Some(definition) = self.definitions.get(&name)
            {
                references(definition, &mut pending);
            }
        }
        self.definitions.retain(|name, _| named.contains(name));
    }
}

impl fmt::Display for AgentSurface {
    /// The document as one line of JSON: what `Value`'s own `Display` writes
    /// for the same document.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[expect(
            clippy::expect_used,
            reason = "an agent surface is a JSON document: nothing in it can fail to encode"
        )]
        let document = serde_json::to_string(self).expect("an agent surface encodes as JSON");
        f.write_str(&document)
    }
}

/// Why what a reader was handed is not the agent surface this build reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSurfaceError {
    /// The prealpha surface, before the document had a root: a bare array of
    /// rows, which named no format and no version.
    PrealphaArray,
    /// A JSON value that is not an object, so not a document of any format.
    NotADocument,
    /// An object of another format.
    OtherFormat {
        /// The `format` it names.
        found: String,
    },
    /// This format at another version. A reader built for one version reads
    /// nothing else.
    UnsupportedVersion {
        /// The `version` it names.
        found: u64,
    },
    /// A document of this format whose shape is not the one it states.
    Malformed {
        /// What is wrong with it.
        reason: String,
    },
}

impl fmt::Display for AgentSurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PrealphaArray => write!(
                f,
                "this is the prealpha agent surface, a bare array of rows with no version; \
                 version {AGENT_SURFACE_VERSION} of `{AGENT_SURFACE_FORMAT}` is \
                 `{{\"format\": \"{AGENT_SURFACE_FORMAT}\", \"version\": {AGENT_SURFACE_VERSION}, \
                 \"actions\": […], \"$defs\": {{…}}}}`"
            ),
            Self::NotADocument => {
                write!(
                    f,
                    "an agent surface document is an object, not this JSON value"
                )
            }
            Self::OtherFormat { found } => write!(
                f,
                "this document names the format `{found}`, not `{AGENT_SURFACE_FORMAT}`"
            ),
            Self::UnsupportedVersion { found } => write!(
                f,
                "this is version {found} of an agent surface, and this reader reads \
                 version {AGENT_SURFACE_VERSION}"
            ),
            Self::Malformed { reason } => write!(f, "this agent surface is malformed: {reason}"),
        }
    }
}

impl std::error::Error for AgentSurfaceError {}

/// The parameters of one row.
fn parameters(row: &Value) -> &[Value] {
    row[PARAMS].as_array().map_or(&[], Vec::as_slice)
}

/// Every `$ref` `node` carries, in the order it meets them.
fn references(node: &Value, found: &mut Vec<String>) {
    match node {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == REF
                    && let Some(reference) = value.as_str()
                {
                    found.push(reference.to_owned());
                }
                references(value, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                references(item, found);
            }
        }
        _ => {}
    }
}

/// The definition `reference` names, when it names one of the document's.
fn definition_name(reference: &str) -> Option<String> {
    let name = reference.strip_prefix(LOCAL)?;
    (!name.is_empty()).then(|| name.replace("~1", "/").replace("~0", "~"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn region() -> Value {
        json!({
            "type": "array",
            "items": {"type": "number"},
            "minItems": 4,
            "maxItems": 4,
            "description": "A rectangle of the artwork."
        })
    }

    /// The rectangle's shape as a document states it once: the same, less
    /// the description, which stays beside the `$ref` at each place.
    fn shape() -> Value {
        let mut shape = region();
        if let Some(fields) = shape.as_object_mut() {
            fields.remove("description");
        }
        shape
    }

    /// A parameter that takes a rectangle, whose shape is the one under its
    /// own `rect` property: what a surface states once when several actions
    /// take a region. `title` tells the parameter apart by the action that
    /// takes it, as a published one does.
    fn region_parameter(title: &str) -> Value {
        json!({
            "type": "object",
            "title": title,
            "properties": {"rect": region()},
            "required": ["rect"],
            "additionalProperties": false,
        })
    }

    fn row(resource: &str, action: &str, schema: &Value) -> Value {
        json!({
            "resource": resource,
            "action": action,
            "resource_kind": "canvas.artwork",
            "label": "Select a region",
            "description": "Select the part of the artwork a rectangle holds.",
            "operation": "View",
            "params": [{"name": "rect", "required": true, "schema": schema}],
        })
    }

    fn surface() -> AgentSurface {
        AgentSurface::of_rows(vec![
            row(
                "canvas.artwork",
                "inspect",
                &region_parameter("Inspect a rectangle"),
            ),
            row(
                "canvas.artwork",
                "look",
                &region_parameter("Look at a rectangle"),
            ),
        ])
    }

    /// A surface names its format and version, and states a shape two rows
    /// take once, at the name each of them references.
    #[test]
    fn a_surface_names_its_format_and_states_a_shared_shape_once() {
        let surface = surface();
        assert_eq!(surface.format, AGENT_SURFACE_FORMAT);
        assert_eq!(surface.version, AGENT_SURFACE_VERSION);
        assert_eq!(surface.definitions().len(), 1, "{surface:#?}");
        for row in surface.rows() {
            assert_eq!(
                row["params"][0]["schema"]["properties"]["rect"]["$ref"],
                "#/$defs/rect"
            );
            assert_eq!(
                row["params"][0]["schema"]["properties"]["rect"]["description"],
                "A rectangle of the artwork.",
                "the description each place carries stays beside its `$ref`"
            );
        }
        assert_eq!(
            surface.definitions()["rect"],
            shape(),
            "the shape is stated once, as it was"
        );
        assert_eq!(
            surface.row("canvas.artwork", "look").unwrap()["action"],
            "look"
        );
        assert!(surface.row("canvas.artwork", "paint").is_none());
    }

    /// A document written by this build is the document this build reads: the
    /// value, the bytes, and what a reader hands back agree.
    #[test]
    fn a_document_round_trips_through_its_json() {
        let surface = surface();
        let document = surface.document();
        let bytes = serde_json::to_vec(&surface).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            document,
            "the value and the bytes say the same"
        );
        assert_eq!(AgentSurface::read(&document).unwrap(), surface);
        assert_eq!(AgentSurface::read_json(&bytes).unwrap(), surface);
        assert_eq!(
            serde_json::to_vec(&AgentSurface::read_json(&bytes).unwrap()).unwrap(),
            bytes,
            "what a reader hands back writes the bytes it read"
        );
    }

    /// The format before this one — a bare array of rows, which named no
    /// version — is refused as what it is, not read as an empty surface.
    #[test]
    fn the_prealpha_bare_array_is_refused_by_name() {
        let prealpha = json!([row(
            "canvas.artwork",
            "look",
            &region_parameter("Look at a rectangle"),
        )]);
        let refused = AgentSurface::read(&prealpha).unwrap_err();
        assert_eq!(refused, AgentSurfaceError::PrealphaArray);
        let said = refused.to_string();
        assert!(said.contains("prealpha"), "{said}");
        assert!(
            said.contains(AGENT_SURFACE_FORMAT),
            "the refusal names the format to write: {said}"
        );
        assert!(
            said.contains(&AGENT_SURFACE_VERSION.to_string()),
            "the refusal names the version to write: {said}"
        );
    }

    /// A document at another version, or of another format, is refused with
    /// the version or the name it carries; nothing is read out of it.
    #[test]
    fn another_version_or_format_is_refused_truthfully() {
        let rows = vec![row(
            "canvas.artwork",
            "look",
            &region_parameter("Look at a rectangle"),
        )];
        let future = json!({
            "format": AGENT_SURFACE_FORMAT,
            "version": AGENT_SURFACE_VERSION + 1,
            "actions": rows,
            "$defs": {},
        });
        let refused = AgentSurface::read(&future).unwrap_err();
        assert_eq!(
            refused,
            AgentSurfaceError::UnsupportedVersion {
                found: u64::from(AGENT_SURFACE_VERSION) + 1
            }
        );
        assert!(refused.to_string().contains("version 2"), "{refused}");
        let other = json!({
            "format": "tilo.something-else",
            "version": AGENT_SURFACE_VERSION,
            "actions": [],
            "$defs": {},
        });
        assert_eq!(
            AgentSurface::read(&other).unwrap_err(),
            AgentSurfaceError::OtherFormat {
                found: "tilo.something-else".to_owned()
            }
        );
    }

    /// A document with no version, or no format, or not an object at all, is
    /// malformed rather than an empty surface; so is one of this version
    /// whose shape is not the shape it states.
    #[test]
    fn a_malformed_document_is_refused_rather_than_read_as_nothing() {
        for document in [
            json!({"actions": [], "$defs": {}}),
            json!({"format": AGENT_SURFACE_FORMAT, "actions": [], "$defs": {}}),
            json!({"format": AGENT_SURFACE_FORMAT, "version": "one", "actions": []}),
            json!({"format": AGENT_SURFACE_FORMAT, "version": 1.5, "actions": []}),
            json!({"format": 1, "version": AGENT_SURFACE_VERSION, "actions": []}),
            json!({"format": AGENT_SURFACE_FORMAT, "version": AGENT_SURFACE_VERSION}),
            json!({
                "format": AGENT_SURFACE_FORMAT,
                "version": AGENT_SURFACE_VERSION,
                "actions": [],
                "$defs": {},
                "surface": [],
            }),
            json!(null),
            json!("an agent surface"),
            json!(7),
            json!(true),
        ] {
            let refused = AgentSurface::read(&document);
            assert!(
                matches!(
                    refused,
                    Err(AgentSurfaceError::Malformed { .. } | AgentSurfaceError::NotADocument)
                ),
                "{document} is refused: {refused:?}"
            );
        }
    }

    /// A surface narrowed to one caller's rows does not state a definition
    /// its dropped rows alone named, and a row that keeps a shape keeps it.
    #[test]
    fn a_narrowed_surface_states_only_what_its_rows_name() {
        let mut narrowed = AgentSurface::of_rows(vec![
            row(
                "canvas.artwork",
                "look",
                &region_parameter("Look at a rectangle"),
            ),
            row(
                "canvas.artwork",
                "inspect",
                &region_parameter("Inspect a rectangle"),
            ),
            json!({
                "resource": "canvas.artwork",
                "action": "paint",
                "resource_kind": "canvas.artwork",
                "label": "Paint",
                "description": null,
                "operation": "Document",
                "params": [],
            }),
        ]);
        assert_eq!(narrowed.definitions().len(), 1);
        narrowed.retain(|row| row["action"] == "paint");
        assert_eq!(narrowed.rows().len(), 1);
        assert!(
            narrowed.definitions().is_empty(),
            "a definition only dropped rows named goes with them: {narrowed:#?}"
        );

        let mut kept = surface();
        kept.retain(|row| row["action"] == "look");
        assert_eq!(kept.definitions().len(), 1);
        let schema = kept.rows()[0]["params"][0]["schema"].clone();
        assert_eq!(
            kept.scoped(&schema),
            crate::params::scoped(&kept.document(), &schema),
            "a schema read with the surface's definitions is what a document gives"
        );
        assert_eq!(
            kept.scoped(&schema)["$defs"]["rect"],
            shape(),
            "the kept row still resolves the shape it names"
        );
    }

    /// A definition may name another: narrowing a surface keeps the ones its
    /// kept rows reach, transitively, and drops the rest.
    #[test]
    fn a_narrowed_surface_keeps_the_definitions_its_definitions_name() {
        let mut narrowed = AgentSurface {
            format: AGENT_SURFACE_FORMAT.to_owned(),
            version: AGENT_SURFACE_VERSION,
            actions: vec![
                json!({
                    "resource": "canvas.artwork",
                    "action": "look",
                    "params": [{"name": "rect", "required": true, "schema": {"$ref": "#/$defs/rect"}}],
                }),
                json!({
                    "resource": "canvas.artwork",
                    "action": "paint",
                    "params": [{"name": "brush", "required": true, "schema": {"$ref": "#/$defs/brush"}}],
                }),
            ],
            definitions: [
                (
                    "rect".to_owned(),
                    json!({"type": "array", "items": {"$ref": "#/$defs/point"}}),
                ),
                ("point".to_owned(), json!({"type": "array"})),
                (
                    "brush".to_owned(),
                    json!({"type": "string", "minLength": 1}),
                ),
            ]
            .into_iter()
            .collect(),
        };
        narrowed.retain(|row| row["action"] == "look");
        assert_eq!(
            narrowed.definitions().keys().collect::<Vec<_>>(),
            ["point", "rect"],
            "the brush a dropped row named is gone; the point a kept one reaches stays"
        );
    }

    /// A surface of no rows states no definitions, and a row that publishes
    /// no schema is no reason to invent one.
    #[test]
    fn an_empty_surface_states_no_definitions() {
        let surface = AgentSurface::of_rows(Vec::new());
        assert!(surface.rows().is_empty());
        assert!(surface.definitions().is_empty());
        assert_eq!(
            surface.document(),
            json!({
                "format": AGENT_SURFACE_FORMAT,
                "version": AGENT_SURFACE_VERSION,
                "actions": [],
                "$defs": {},
            })
        );
    }

    /// Sharing a shape is a representation of the contract, not a different
    /// one: a published schema admits and refuses the values the shape it
    /// replaced did.
    #[test]
    fn a_shared_shape_admits_what_the_shape_replaced_admitted() {
        let declared = [
            ("look", region_parameter("Look at a rectangle")),
            ("inspect", region_parameter("Inspect a rectangle")),
        ];
        let surface = AgentSurface::of_rows(
            declared
                .iter()
                .map(|(action, schema)| row("canvas.artwork", action, schema))
                .collect(),
        );
        assert_eq!(surface.definitions().len(), 1, "{surface:#?}");
        for (row, (action, shape)) in surface.rows().iter().zip(declared) {
            assert_eq!(row["action"], action);
            let published = surface.scoped(&row["params"][0]["schema"]);
            for probe in [
                json!({"rect": [0, 0, 10, 10]}),
                json!({"rect": [0, 0, 10]}),
                json!({"rect": [0, 0, 10, "ten"]}),
                json!({"rect": []}),
                json!({"rect": "a rectangle"}),
                json!({"rect": null}),
                json!({}),
                json!({"rect": [0, 0, 10, 10], "elsewhere": 1}),
            ] {
                assert_eq!(
                    crate::validate_json_schema(&probe, &published).is_ok(),
                    crate::validate_json_schema(&probe, &shape).is_ok(),
                    "{action} admits {probe} the same way"
                );
            }
        }
    }
}
