//! Read-only materialized view of a `Document` for rendering/export.

use crate::document::MarkSpan;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockView {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    pub children: Vec<BlockView>,
}
