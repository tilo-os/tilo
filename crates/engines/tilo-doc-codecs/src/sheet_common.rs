//! Shared helpers for the spreadsheet codecs (`.xlsx`/`.ods`): the provider
//! contract and prefix-insensitive XML name/attribute access. Their packages
//! are [`crate::package::ZipPackage`]s, read under the same zip-bomb /
//! entry-count / DTD-XXE guards as the document codecs.

use crate::format::CodecError;
use std::collections::BTreeMap;
use tilo_doc::sheet::{CellInput, Workbook};

/// A spreadsheet format provider — the workbook analogue of [`crate::format::FormatProvider`].
/// Import is bounded/hardened via `ZipPackage::read`; the contract is identical either side of a sandbox.
pub trait SheetProvider: Send + Sync {
    fn import(&self, bytes: &[u8], wb: &mut Workbook) -> Result<(), CodecError>;
    /// # Errors
    /// Returns [`CodecError`] if the workbook cannot be serialised into this
    /// format, or if writing its container fails.
    fn export(&self, wb: &Workbook) -> Result<Vec<u8>, CodecError>;
}

/// A spreadsheet package parsed without touching a CRDT: its tabs in order with
/// their cells, the document properties, and the unmodeled parts an export
/// re-emits verbatim. [`SheetProvider::import`] writes one of these into a
/// [`Workbook`]; a reader that keeps its own model (an app's grid) can use it
/// directly and skip the CRDT.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedWorkbook {
    pub sheets: Vec<ParsedSheet>,
    pub properties: BTreeMap<String, String>,
    pub preserved: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSheet {
    pub name: String,
    /// `(A1 reference, cell)` in document order; a later entry for a reference
    /// replaces an earlier one, as it does in a [`Workbook`].
    pub cells: Vec<(String, CellInput)>,
    pub hidden: bool,
}

impl ParsedWorkbook {
    /// Writes everything into `wb`, filing preserved parts under `format`
    /// (`xlsx`/`ods`) where that codec's export looks for them.
    pub(crate) fn apply(self, wb: &mut Workbook, format: &str) {
        for sheet in self.sheets {
            let id = wb.add_sheet(&sheet.name);
            for (reference, cell) in sheet.cells {
                wb.set_cell(&id, &reference, cell);
            }
        }
        for (key, value) in &self.properties {
            wb.set_prop(key, value);
        }
        for (name, bytes) in &self.preserved {
            wb.set_preserved(&format!("{format}/part/{name}"), bytes);
        }
    }
}

fn strip_prefix(raw: &[u8]) -> &[u8] {
    raw.splitn(2, |b| *b == b':').nth(1).unwrap_or(raw)
}

pub(crate) fn local_name<'a>(e: &'a quick_xml::events::BytesStart<'a>) -> &'a [u8] {
    strip_prefix(e.name().into_inner())
}

pub(crate) fn end_local_name<'a>(e: &'a quick_xml::events::BytesEnd<'a>) -> &'a [u8] {
    strip_prefix(e.name().into_inner())
}

pub(crate) fn attr_local(e: &quick_xml::events::BytesStart, local: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if strip_prefix(a.key.as_ref()) == local {
            return crate::unescape_attr(&a);
        }
    }
    None
}
