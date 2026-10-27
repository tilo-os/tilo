//! Workbooks read from and kept as content: an `.xlsx` the room holds (an
//! attachment, a shared file, another app's export) imported into sheets,
//! and the workbook kept as `.xlsx` content another app or an agent reads,
//! attaches or sends, with no file or dialog in between.

use ui::framework::content::ContentStore;
use ui::framework::{EffectAnswer, EffectCx, Effected};

use super::{Interchange, MAX_IMPORT_FILE_BYTES, SheetsApp};
use crate::workbook::{SheetSnapshot, Workbook};

impl SheetsApp {
    /// Read and keep content in `content`, the store the host lends the
    /// window; without one, content is kept in memory.
    pub(crate) fn attach_content(&mut self, content: Option<ContentStore>) {
        self.content = content;
    }

    fn content_store(&mut self) -> ContentStore {
        self.content
            .get_or_insert_with(ContentStore::memory)
            .clone()
    }

    /// Every sheet as the file formats take it, a sheet whose formulas read
    /// others evaluated with the workbook.
    pub(super) fn workbook_snapshots(&self) -> Vec<SheetSnapshot> {
        self.workbook
            .sheets
            .ids()
            .filter_map(|id| self.workbook.snapshot_of(id))
            .collect()
    }

    /// Add the sheets of the `.xlsx` workbook at `content` (a
    /// `tilo.content:<hash>` address) as one step, naming the workbook
    /// `title` when it has no name yet. Content imported before is not added
    /// again: the answer names the sheets it became.
    pub(super) fn import_xlsx_content(
        &mut self,
        content: &str,
        title: Option<String>,
        cx: &EffectCx<Workbook, Self>,
    ) -> EffectAnswer {
        let hash = ui::tilo_actions::content_hash(content.trim())
            // A plan that carried the content gives its bare hash.
            .or_else(|| {
                let bare = content.trim();
                (bare.len() == 64 && bare.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then_some(bare)
            })
            .ok_or_else(|| format!("`{content}` names no content: give `tilo.content:<hash>`"))?
            .to_ascii_lowercase();
        let source = format!("tilo.content:{hash}");
        let already: Vec<String> = self
            .workbook
            .sheets
            .values()
            .filter(|sheet| sheet.source.as_ref().is_some_and(|it| it.0 == source))
            .map(|sheet| sheet.name.0.clone())
            .collect();
        if !already.is_empty() {
            return Ok(Effected::value(serde_json::json!({
                "sheets": already,
                "imported": false,
                "content": source,
            })));
        }
        let bytes = self
            .content_store()
            .read(&hash)
            .ok_or_else(|| format!("content `{hash}` is not on this device"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_FILE_BYTES {
            return self.failed(format!(
                "the workbook is larger than the {} import limit",
                ui::format::memory(MAX_IMPORT_FILE_BYTES)
            ));
        }
        let tables = Self::read_tables(Interchange::Xlsx, &bytes, "")
            .map_err(|error| format!("content `{hash}` is not an Excel workbook: {error}"))?;
        let sheets: Vec<serde_json::Value> = tables
            .iter()
            .map(|table| serde_json::json!({ "name": table.name, "hidden": table.hidden }))
            .collect();
        let title = title
            .map(|title| title.trim().trim_end_matches(".xlsx").trim().to_owned())
            .filter(|title| !title.is_empty());
        let (invoke, total) = self.import_action(tables, Some(source.clone()), title)?;
        cx.submit(invoke);
        self.status = format!("Imported {total} cells");
        Ok(Effected::value(serde_json::json!({
            "sheets": sheets,
            "cells": total,
            "imported": true,
            "content": source,
        })))
    }

    /// Keep every sheet as an `.xlsx` workbook in the content store and
    /// answer its address.
    pub(super) fn export_xlsx_content(&mut self) -> EffectAnswer {
        let sheets = self.workbook_snapshots();
        let bytes = Self::encode_tables(Interchange::Xlsx, &sheets)?;
        let hash = self
            .content_store()
            .put(&bytes)
            .map_err(|error| format!("the workbook could not be kept: {error}"))?;
        Effected::content(&hash)
    }
}
