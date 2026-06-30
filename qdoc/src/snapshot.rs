//! Read-only materialized view of a `Document` for rendering/export.

use crate::document::MarkSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A scalar block/style attribute value (heading level, list ordering, style id,
/// a style's font size, …). `Float` carries fractional values (style sizes /
/// spacing) that don't fit an integer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrVal {
    Int(i64),
    Bool(bool),
    Str(String),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
    /// Content-addressed binary store (blake3 hex → bytes). `image` blocks carry
    /// their `blob` attr as a key here so a renderer can fetch the pixels.
    #[serde(default)]
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// CRDT-native named styles: `style_id → (property → value)`. Built-ins
    /// (body/heading1…/quote) plus any user- or import-defined styles. Empty for
    /// documents authored before the styles container was populated.
    #[serde(default)]
    pub styles: BTreeMap<String, BTreeMap<String, AttrVal>>,
    /// Document properties / metadata (`title`/`author`/`subject`/`keywords`/
    /// `created`/`modified`/`generator` → value). Round-trips through the `.odt`
    /// `meta.xml` and `.docx` `docProps/*.xml` parts. Empty for documents with no
    /// properties set (and absent in snapshots authored before this field existed).
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockView {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    pub attrs: BTreeMap<String, AttrVal>,
    pub children: Vec<BlockView>,
}
