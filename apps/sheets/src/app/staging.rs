//! The view a review capture stages against the document, applied once the window has the layout it needs.

use super::*;

impl SheetsApp {
    /// Stage a screenshot scenario's view state over the sample workbook its
    /// actions wrote: the active cell, an edit in progress, a message. A menu
    /// or sheet opens in the first measured frame (it anchors to what that
    /// frame lays out); what needs the document (a fill, a paste, the
    /// palette's actions) is done by the `sync` that follows.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        // A smart-typed currency cell, so the formula bar and the type
        // indicator are populated.
        if let Some(addr) = a1_to_addr("E2") {
            self.grid.select_only(addr);
        }
        match scenario {
            Some(name @ ("docsettings" | "tabmenu" | "tabcolour" | "tabrename" | "cellmenu")) => {
                self.staged_overlay = Some(name.to_owned());
            }
            Some("editing") => self.grid.begin_edit_in_place(),
            // A formula built purely by clicking cells (point mode): the
            // coloured references it inserted and the matching outlines on
            // the referenced cells (B2 and C2).
            Some("pointmode") => {
                self.grid.select_only((2, 9)); // C10, an empty cell
                self.grid.begin_edit(Some("=".into()));
                self.grid.point_click_addr((1, 1)); // click B2 → =B2
                self.grid.insert_text_public("+"); // type + → =B2+
                self.grid.point_click_addr((2, 1)); // click C2 → =B2+C2
            }
            // Budget!B2:B6 — the January column body — and its summary.
            Some("summary") => {
                self.grid.select_range_public((1, 1), (1, 5));
            }
            // Two separate highlighted rectangles with lit headers: the
            // January column body and the Q1 Total column.
            Some("multiselect") => {
                self.grid.select_range_public((1, 1), (1, 5)); // B2:B6
                self.grid.add_range_public((4, 1), (4, 6)); // E2:E7
            }
            // The range reference inside E2's `=SUM(B2:D2)` selected in the
            // editable formula bar.
            Some("fbarselect") => {
                self.grid.select_only((4, 1));
                self.grid.begin_edit_in_place();
                self.grid.set_edit_caret(5, false); // just before "B2:D2"
                self.grid.set_edit_caret(10, true); // select "B2:D2"
            }
            Some("commandpalette") => self.staged = Some(Staged::CommandPalette),
            // Both scrollbar thumbs offset, proving the two-dimensional
            // viewport rather than idle rails.
            Some("scrollbars") => self.grid.on_scroll(540.0, 260.0),
            Some("fillseries") => self.staged = Some(Staged::FillSeries),
            Some("pasteformula") => self.staged = Some(Staged::PasteFormula),
            Some("error") => {
                self.status = "Excel import failed: the workbook is password protected".into();
            }
            // An incoming revision superseding the value being edited.
            Some("conflict") => {
                self.status =
                    "Budget was updated by a collaborator; showing the latest value".into();
            }
            // The acknowledgement stays visible with animations suppressed.
            Some("reducedmotion") => {
                self.set_motion_reduced(true);
                self.status = "Pasted into B2:D4".into();
            }
            Some("charts" | "comments") => {
                self.status = "2 charts and 1 resolved comment thread added".into();
            }
            // The bold header row, so the Bold command shows it is on.
            Some("bold") => self.grid.select_range_public((0, 0), (6, 0)),
            _ => {}
        }
    }

    /// Open the staged menu or sheet, if any, now that the chrome is measured
    /// and laid out where it will be painted.
    pub(super) fn apply_staged_overlay(&mut self) {
        let Some(name) = self.staged_overlay.take() else {
            return;
        };
        match name.as_str() {
            "docsettings" => self.open_doc_settings(),
            "tabmenu" => self.open_tab_menu(0),
            "cellmenu" => {
                self.grid.select_only((1, 1));
                let anchor = self.grid.cell_screen_rect(1, 1).center();
                self.open_cell_menu(anchor);
            }
            "tabrename" => self.begin_tab_rename(0),
            // The popover opens over tabs that already carry a colour.
            "tabcolour" => {
                self.open_tab_menu(0);
                self.tab_menu.close();
                self.tab_colour.set_open(true);
            }
            _ => {}
        }
    }

    /// Finish the staged part of a scenario that works on the document.
    pub(super) fn finish_staging(&mut self, doc: &mut Edits<'_>) {
        let Some(staged) = self.staged.take() else {
            return;
        };
        match staged {
            Staged::CommandPalette => self.open_command_palette(),
            // The seeded patterns extended through the real fill-handle path:
            // a number run (1, 2 → …6), weekdays (Mon → …Sat) and a
            // text+number run (Task 1 → …Task 6), with the fill handle on the
            // filled numeric column.
            Staged::FillSeries => {
                self.grid.select_range_public((1, 15), (1, 16));
                self.grid.start_fill_to((1, 20));
                self.run_pending_fill(doc);
                self.grid.select_only((2, 15));
                self.grid.start_fill_to((2, 20));
                self.run_pending_fill(doc);
                self.grid.select_only((3, 15));
                self.grid.start_fill_to((3, 20));
                self.run_pending_fill(doc);
                self.grid.select_range_public((1, 15), (1, 20));
            }
            // E2's `=SUM(B2:D2)` copied one row down: the formula bar shows
            // the adjusted `=SUM(B3:D3)` and the cell its result.
            Staged::PasteFormula => {
                self.grid.select_only((4, 1));
                self.copy_selection(false);
                self.grid.select_only((4, 2)); // E3, the Groceries total
                self.paste(doc);
                self.grid.select_only((4, 2));
            }
        }
        self.reload();
    }

    /// Open the per-document settings sheet.
    pub(super) fn open_doc_settings(&mut self) {
        self.close_overlays();
        self.settings_panel = SettingsPanel::Document;
    }
}
