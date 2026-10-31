//! The draft on the confirmation sheet: what would run, field by field,
//! for the person to change before they allow it.
//!
//! A confirmation names exact arguments, and the host shows them as a
//! [`Draft`]. Every field is a row of the sheet the compositor draws: text
//! and numbers in the toolkit's own text field, yes-or-no values as check
//! rows, everything else as read-only text that scrolls rather than hiding
//! what is past its edge. Nothing on it is drawn by the app or agent that
//! asks. A field the draft offers for editing ([`DraftField::editable`])
//! takes the person's typing; what they changed leaves with their answer as
//! [`DraftEdit`]s, checked against the draft before the sheet closes. The
//! host applies them and binds the confirmation to the arguments they make.

use std::ops::Range;

use tilo_actions::{Draft, DraftEdit, DraftField, DraftValue};
use ui::widget::{TextInput, TextInputKind, Widget as _};
use vello::kurbo::Rect;

use super::{DialogLayout, DialogView, PendingDialog, PortalDialogController, PortalDialogQueue};
use crate::shell::app_action_consent::{AppActionAnswer, AppActionConsent};

/// Where one row of a confirmation's draft sits on the sheet.
#[derive(Clone, Copy, Debug)]
pub struct DraftRowLayout {
    pub index: usize,
    pub label: Rect,
    pub control: Rect,
}

impl PortalDialogQueue {
    /// The draft a confirmation under `handle` shows, row by row. A Store
    /// disclosure is reviewed as its own pages instead.
    pub(super) fn action_grant_draft(&self, handle: &str) -> Option<Draft> {
        let inner = self.inner.lock().ok()?;
        match inner.pending.get(handle)? {
            PendingDialog::ActionGrant { request, .. } if request.moderation.is_none() => request
                .draft
                .clone()
                .filter(|draft| !draft.fields.is_empty()),
            _ => None,
        }
    }
}

/// Above the detail: the title and who asks.
const TITLE: f64 = ui::tokens::Layout::BASE.sheet_title_band as f64;
/// A line of the detail, and what follows it before the rows.
const DETAIL_LINE: f64 = ui::tokens::Layout::BASE.sheet_detail_line as f64;
const DETAIL_GAP: f64 = ui::tokens::Spacing::BASE.s4 as f64;
/// An average glyph of the detail, for a wrap not yet measured.
const DETAIL_GLYPH: f64 = 7.5;
/// Between the rows and the answers.
const ANSWERS_GAP: f64 = ui::tokens::Spacing::BASE.s4 as f64;
/// Below the answers: the notice line, kept free so a refusal moves
/// nothing, then the buttons.
const FOOTER: f64 = ui::tokens::Layout::BASE.sheet_footer as f64;
/// The pitch of an answer row: a Control row and the `s1` between two.
const ANSWER_PITCH: f64 =
    (ui::tokens::Metrics::BASE.panel_row_height + ui::tokens::Spacing::BASE.s1) as f64;
/// Kept clear around the sheet.
const MARGIN: f64 = ui::tokens::Spacing::BASE.s4 as f64;
/// The widest the sheet grows, and its side gutter.
const WIDTH: f64 = ui::tokens::Metrics::BASE.panel_width_sheet as f64;
const GUTTER: f64 = ui::tokens::Spacing::BASE.s6 as f64;

/// A field's label line, above its control.
pub(super) const LABEL_HEIGHT: f64 = ui::tokens::Layout::BASE.field_label_height as f64;
/// Between two rows.
pub(super) const ROW_GAP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// A one-line control: a Control row (HIG §9.6).
const LINE_CONTROL: f64 = ui::tokens::Metrics::BASE.panel_row_height as f64;
/// A prose field: room for about three lines.
const PROSE_CONTROL: f64 = ui::tokens::Layout::BASE.field_prose_height as f64;

/// How a row shows its value.
pub(super) enum Control {
    /// A text field: editable where the draft offers it, else read-only.
    Text(TextInput),
    /// A yes-or-no value as a check row.
    Flag { on: bool },
}

/// One field of the draft on the sheet.
pub(super) struct DraftRow {
    pub(super) field: DraftField,
    pub(super) control: Control,
}

impl DraftRow {
    fn new(field: &DraftField, index: usize) -> Self {
        let control = match &field.value {
            DraftValue::Flag { on } => Control::Flag { on: *on },
            value => {
                let text = match value {
                    DraftValue::Text { text } | DraftValue::Number { text } => text.clone(),
                    other => other.line(),
                };
                let mut input = TextInput::local()
                    .with_id(&format!("portal-draft-field-{index}"))
                    .with_accessible_name(field.label.clone())
                    .with_initial_value(&text)
                    .with_read_only(!field.editable);
                if !field.editable {
                    // Read as text, not as a field waiting for typing.
                    input = input.with_plain_chrome();
                }
                if matches!(value, DraftValue::Number { .. }) {
                    input = input.with_kind(TextInputKind::Number);
                }
                if field.multiline {
                    input = input.with_min_lines(3);
                }
                if matches!(value, DraftValue::Withheld) {
                    input.set_secret(true);
                }
                // The caret waits after what is there, where a change to it
                // starts.
                input.set_cursor_pos(text.len());
                Control::Text(input)
            }
        };
        Self {
            field: field.clone(),
            control,
        }
    }

    /// The height the row takes on the sheet, its label included.
    pub(super) fn height(&self) -> f64 {
        match &self.control {
            // A check row carries its own label.
            Control::Flag { .. } => LINE_CONTROL,
            Control::Text(_) if self.field.multiline => LABEL_HEIGHT + PROSE_CONTROL,
            Control::Text(_) => LABEL_HEIGHT + LINE_CONTROL,
        }
    }

    /// Whether the person may change it.
    pub(super) fn editable(&self) -> bool {
        self.field.editable
    }

    /// Whether Enter types a new line here rather than answering.
    pub(super) fn takes_new_lines(&self) -> bool {
        self.editable() && self.field.multiline && matches!(self.control, Control::Text(_))
    }

    /// What the person changed here, if anything.
    fn edit(&self) -> Option<DraftEdit> {
        if !self.field.editable {
            return None;
        }
        let (now, was) = match (&self.control, &self.field.value) {
            (Control::Text(input), DraftValue::Text { text } | DraftValue::Number { text }) => {
                (input.value().to_owned(), text.clone())
            }
            (Control::Flag { on }, DraftValue::Flag { on: was }) => {
                (on.to_string(), was.to_string())
            }
            _ => return None,
        };
        (now != was).then(|| DraftEdit {
            path: self.field.path.clone(),
            text: now,
        })
    }
}

/// The draft as the sheet holds it while the person reviews it.
pub(super) struct DraftSheet {
    draft: Draft,
    pub(super) rows: Vec<DraftRow>,
    /// The row the keyboard is on; past the rows, the sheet's answers.
    pub(super) focus: usize,
}

impl DraftSheet {
    /// The sheet's rows for `draft`, focused on the first field the person
    /// may change.
    pub(super) fn new(draft: &Draft) -> Self {
        let rows: Vec<DraftRow> = draft
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| DraftRow::new(field, index))
            .collect();
        let focus = rows.iter().position(DraftRow::editable).unwrap_or(0);
        Self {
            draft: draft.clone(),
            rows,
            focus,
        }
    }

    /// Whether any field takes the person's changes.
    pub(super) fn editable(&self) -> bool {
        self.rows.iter().any(DraftRow::editable)
    }

    /// The person's changes, as the draft accepts them.
    ///
    /// # Errors
    /// The first change the draft refuses, in words for the person: a
    /// number field that holds no number, say.
    pub(super) fn edits(&self) -> Result<Vec<DraftEdit>, String> {
        let edits: Vec<DraftEdit> = self.rows.iter().filter_map(DraftRow::edit).collect();
        self.draft.check_edits(&edits)?;
        Ok(edits)
    }

    /// The rows split into pages that each fit `room` high, in order. A
    /// row taller than `room` gets a page of its own (the layout refuses a
    /// room that small).
    pub(super) fn pages(&self, room: f64) -> Vec<Range<usize>> {
        let mut pages = Vec::new();
        let mut start = 0;
        let mut used = 0.0;
        for (index, row) in self.rows.iter().enumerate() {
            let height = row.height() + if index > start { ROW_GAP } else { 0.0 };
            if index > start && used + height > room {
                pages.push(start..index);
                start = index;
                used = row.height();
            } else {
                used += height;
            }
        }
        if start < self.rows.len() || pages.is_empty() {
            pages.push(start..self.rows.len());
        }
        pages
    }

    /// How tall `page` stands.
    pub(super) fn page_height(&self, page: &Range<usize>) -> f64 {
        let rows = self.rows.get(page.clone()).unwrap_or_default();
        let gaps = rows.len().saturating_sub(1) as f64 * ROW_GAP;
        rows.iter().map(DraftRow::height).sum::<f64>() + gaps
    }

    /// The focused row's text field, when it is one.
    pub(super) fn focused_text_mut(&mut self) -> Option<&mut TextInput> {
        match &mut self.rows.get_mut(self.focus)?.control {
            Control::Text(input) => Some(input),
            Control::Flag { .. } => None,
        }
    }

    /// The focused row, when the keyboard is on one.
    pub(super) fn focused_row(&self) -> Option<&DraftRow> {
        self.rows.get(self.focus)
    }

    /// Tick or untick the yes-or-no field at `index`. `false` when it is
    /// not one the person may change.
    pub(super) fn toggle(&mut self, index: usize) -> bool {
        let Some(row) = self.rows.get_mut(index) else {
            return false;
        };
        if !row.field.editable {
            return false;
        }
        match &mut row.control {
            Control::Flag { on } => {
                *on = !*on;
                true
            }
            Control::Text(_) => false,
        }
    }

    /// Put a whole new value in the text field at `index`, as an assistive
    /// technology sets it. `false` when it is not one the person may change.
    pub(super) fn set_text(&mut self, index: usize, value: &str) -> bool {
        let Some(row) = self.rows.get_mut(index) else {
            return false;
        };
        if !row.field.editable {
            return false;
        }
        let multiline = row.field.multiline;
        match &mut row.control {
            Control::Text(input) => {
                let typed: String = value
                    .chars()
                    .filter(|ch| !ch.is_control() || (multiline && *ch == '\n'))
                    .collect();
                input.set_text(&typed);
                input.set_cursor_pos(typed.len());
                true
            }
            Control::Flag { .. } => false,
        }
    }
}

/// What the painter draws for one row besides its text field.
pub struct DraftRowFace {
    pub index: usize,
    pub label: String,
    /// A yes-or-no field's value, for its check row.
    pub flag: Option<bool>,
    pub editable: bool,
    pub focused: bool,
}

impl PortalDialogController {
    /// Answer the grant sheet `handle` with the chosen one of `answers` and
    /// what the person changed on its draft, once the draft accepts it.
    /// `false`, saying why, when it does not.
    pub(super) fn answer_action_grant(
        &mut self,
        handle: &str,
        answers: &[AppActionConsent],
    ) -> bool {
        let edits = match self.draft.as_ref().map(DraftSheet::edits) {
            Some(Err(reason)) => {
                self.entry_error = Some(reason);
                return false;
            }
            Some(Ok(edits)) => edits,
            None => Vec::new(),
        };
        answers.get(self.selected).is_some_and(|consent| {
            self.queue.finish_action_grant(
                handle,
                AppActionAnswer {
                    consent: *consent,
                    edits,
                },
            )
        })
    }

    /// Lay this page's text fields out where `layout` puts them, the
    /// focused one marked; for painting and pointer routing.
    pub(super) fn place_draft_fields(&mut self, layout: &DialogLayout) -> Vec<&mut TextInput> {
        let mut placed = Vec::new();
        let Some(draft) = &mut self.draft else {
            return placed;
        };
        let focus = draft.focus;
        for (index, row) in draft.rows.iter_mut().enumerate() {
            let Some(placement) = layout.draft_rows.iter().find(|row| row.index == index) else {
                continue;
            };
            if let Control::Text(input) = &mut row.control {
                input.apply_layout_external(placement.control);
                input.set_focus(index == focus);
                placed.push(input);
            }
        }
        placed
    }

    /// Whether the sheet up is a confirmation showing its draft.
    pub fn is_draft_sheet(&self) -> bool {
        self.draft.is_some()
    }

    /// Whether Enter types a new line in the focused field rather than
    /// answering the sheet.
    pub fn draft_takes_new_line(&self) -> bool {
        self.draft
            .as_ref()
            .and_then(DraftSheet::focused_row)
            .is_some_and(DraftRow::takes_new_lines)
    }

    /// The rows on this page, as the painter labels them and draws their
    /// check rows; text fields come from [`Self::place_fields`].
    pub fn draft_faces(&self, layout: &DialogLayout) -> Vec<DraftRowFace> {
        let Some(draft) = &self.draft else {
            return Vec::new();
        };
        layout
            .draft_rows
            .iter()
            .filter_map(|placed| {
                let row = draft.rows.get(placed.index)?;
                Some(DraftRowFace {
                    index: placed.index,
                    label: row.field.label.clone(),
                    flag: match row.control {
                        Control::Flag { on } => Some(on),
                        Control::Text(_) => None,
                    },
                    editable: row.field.editable,
                    focused: draft.focus == placed.index,
                })
            })
            .collect()
    }

    /// Space on the sheet: tick the focused yes-or-no field, or choose the
    /// focused answer. `false` when the focus is in a text field, which
    /// takes the space as typing.
    pub fn toggle_draft_focus(&mut self) -> bool {
        let Some(draft) = &mut self.draft else {
            return false;
        };
        let focus = draft.focus;
        if focus >= draft.rows.len() {
            self.selected = focus - draft.rows.len();
            return true;
        }
        if matches!(
            draft.focused_row().map(|row| &row.control),
            Some(Control::Flag { .. })
        ) {
            draft.toggle(focus);
            self.entry_error = None;
            return true;
        }
        false
    }

    /// A press on the row at `index`: its field takes the keyboard, and a
    /// check row the person may change flips.
    pub(super) fn press_draft_row(&mut self, index: usize) -> bool {
        let Some(draft) = &mut self.draft else {
            return false;
        };
        if index >= draft.rows.len() {
            return false;
        }
        draft.focus = index;
        if matches!(draft.rows[index].control, Control::Flag { .. }) && draft.toggle(index) {
            self.entry_error = None;
        }
        true
    }

    /// Give the keyboard to the row at `index`.
    pub(super) fn focus_draft_row(&mut self, index: usize) -> bool {
        match &mut self.draft {
            Some(draft) if index < draft.rows.len() => {
                draft.focus = index;
                true
            }
            _ => false,
        }
    }

    /// Give the keyboard to the row under `point`, before the press reaches
    /// the fields, so the caret lands in the field pressed.
    pub fn focus_draft_at(&mut self, point: vello::kurbo::Point, layout: &DialogLayout) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        if let Some(row) = layout
            .draft_rows
            .iter()
            .find(|row| row.control.contains(point))
        {
            draft.focus = row.index;
        }
    }

    /// The detail the draft sheet wraps, and the width it wraps at in
    /// `bounds`, for the painter to measure before it lays the sheet out.
    pub fn draft_detail_wrap(&self, bounds: Rect) -> Option<(String, f64)> {
        self.draft.as_ref()?;
        Some((
            self.view()?.detail(),
            bounds.width().min(WIDTH) - 2.0 * GUTTER,
        ))
    }

    /// How tall the wrapped detail stands, as the painter measured it.
    pub fn set_draft_detail_height(&self, height: f64) {
        self.detail_height.set(Some(height));
    }

    /// The height above the rows: the title, and the detail as it wraps at
    /// `width` (measured, or estimated before it was).
    fn draft_header(&self, view: &DialogView, width: f64) -> f64 {
        let detail = self.detail_height.get().unwrap_or_else(|| {
            let chars = view.detail().chars().count() as f64;
            (chars * DETAIL_GLYPH / width.max(1.0)).ceil().max(1.0) * DETAIL_LINE
        });
        TITLE + detail.max(DETAIL_LINE) + DETAIL_GAP
    }

    /// Move the keyboard through this page's rows and then the answers.
    pub(super) fn draft_focus_next(&mut self, view: &DialogView, reverse: bool) -> bool {
        let room = self.draft_room.get();
        let page = self.review_page;
        let answers = view.selectable_count();
        let Some(draft) = &mut self.draft else {
            return false;
        };
        let pages = draft.pages(room);
        let Some(rows) = pages.get(page.min(pages.len().saturating_sub(1))).cloned() else {
            return false;
        };
        // This page's rows, then the answers.
        let stops: Vec<usize> = rows
            .clone()
            .chain((0..answers).map(|answer| draft.rows.len() + answer))
            .collect();
        if stops.is_empty() {
            return false;
        }
        let at = stops.iter().position(|stop| *stop == draft.focus);
        let next = match (at, reverse) {
            (None, _) => 0,
            (Some(at), false) => (at + 1) % stops.len(),
            (Some(at), true) => (at + stops.len() - 1) % stops.len(),
        };
        draft.focus = stops[next];
        if draft.focus >= draft.rows.len() {
            self.selected = draft.focus - draft.rows.len();
        }
        true
    }

    /// How many pages the draft takes at the last layout.
    pub(super) fn draft_page_count(&self) -> usize {
        self.draft
            .as_ref()
            .map_or(0, |draft| draft.pages(self.draft_room.get()).len())
    }

    /// Put the keyboard on the first row of the page now shown.
    pub(super) fn focus_draft_page(&mut self) {
        let room = self.draft_room.get();
        let page = self.review_page;
        if let Some(draft) = &mut self.draft
            && let Some(rows) = draft.pages(room).get(page)
        {
            draft.focus = rows.start;
        }
    }

    /// The confirmation sheet with its draft's rows, in `bounds` less what
    /// the on-screen keyboard covers: the rows of the page shown, the
    /// answers under them and the buttons. Rows that do not fit are paged,
    /// each page seen before Allow; the sheet keeps one height on every
    /// page. `None` when not even one row fits: a review that cannot be
    /// read is never approved.
    pub(super) fn draft_layout(
        &self,
        view: &DialogView,
        draft: &DraftSheet,
        bounds: Rect,
    ) -> Option<DialogLayout> {
        let bounds = Rect::new(
            bounds.x0,
            bounds.y0,
            bounds.x1,
            bounds.y1 - self.keyboard_inset(bounds),
        );
        let answers = view.selectable_count().min(6);
        let answers_height = super::small_count_as_f64(answers) * ANSWER_PITCH;
        let width = bounds.width().min(WIDTH);
        let header = self.draft_header(view, width - 2.0 * GUTTER);
        let chrome = header + ANSWERS_GAP + answers_height + FOOTER;
        let room = bounds.height() - 2.0 * MARGIN - chrome;
        let tallest = draft.rows.iter().map(DraftRow::height).fold(0.0, f64::max);
        if room < tallest {
            return None;
        }
        self.draft_room.set(room);
        let pages = draft.pages(room);
        let rows_height = pages
            .iter()
            .map(|page| draft.page_height(page))
            .fold(0.0, f64::max)
            .min(room);
        let height = chrome + rows_height;
        let center = bounds.center();
        let sheet = Rect::new(
            center.x - width / 2.0,
            center.y - height / 2.0,
            center.x + width / 2.0,
            center.y + height / 2.0,
        );
        let (left, right) = (sheet.x0 + GUTTER, sheet.x1 - GUTTER);
        let page = pages
            .get(self.review_page.min(pages.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        let mut y = sheet.y0 + header;
        let mut draft_rows = Vec::new();
        for index in page {
            let row = &draft.rows[index];
            let label_height = match row.control {
                Control::Flag { .. } => 0.0,
                Control::Text(_) => LABEL_HEIGHT,
            };
            let label = Rect::new(left, y, right, y + label_height);
            let control = Rect::new(left, y + label_height, right, y + row.height());
            draft_rows.push(DraftRowLayout {
                index,
                label,
                control,
            });
            y += row.height() + ROW_GAP;
        }
        let row_h = f64::from(ui::tokens::Metrics::BASE.panel_row_height);
        let answers_top = sheet.y0 + header + rows_height + ANSWERS_GAP;
        let choices = (0..answers)
            .map(|index| {
                let y = answers_top + super::small_count_as_f64(index) * ANSWER_PITCH;
                Rect::new(left, y, right, y + row_h)
            })
            .collect();
        let cancel = Rect::new(
            sheet.x1 - 212.0,
            sheet.y1 - 54.0,
            sheet.x1 - 120.0,
            sheet.y1 - 20.0,
        );
        let confirm = Rect::new(
            sheet.x1 - 108.0,
            sheet.y1 - 54.0,
            sheet.x1 - 24.0,
            sheet.y1 - 20.0,
        );
        Some(DialogLayout {
            sheet,
            choices,
            cancel,
            confirm,
            launcher_name: None,
            launcher_icons: Vec::new(),
            remember: None,
            review_back: (self.review_page > 0)
                .then(|| Rect::new(left, cancel.y0, left + 84.0, cancel.y1)),
            answers: Vec::new(),
            draft_rows,
            detail_width: Some(right - left),
        })
    }

    /// The draft as `state-dump` reports it: every row with its value as
    /// it stands, a withheld one without it.
    pub(super) fn draft_state(&self) -> serde_json::Value {
        let Some(draft) = &self.draft else {
            return serde_json::Value::Null;
        };
        let rows: Vec<serde_json::Value> = draft
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let value = match (&row.control, &row.field.value) {
                    (_, DraftValue::Withheld) => serde_json::Value::Null,
                    (Control::Flag { on }, _) => (*on).into(),
                    (Control::Text(input), _) => input.value().into(),
                };
                serde_json::json!({
                    "path": row.field.path,
                    "label": row.field.label,
                    "value": value,
                    "editable": row.field.editable,
                    "focused": index == draft.focus,
                })
            })
            .collect();
        serde_json::json!({
            "rows": rows,
            "page": self.review_page,
            "pages": self.draft_page_count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::type_key;
    use super::*;

    fn field(name: &str, value: DraftValue) -> DraftField {
        DraftField {
            path: vec![name.into()],
            label: name.into(),
            value,
            editable: true,
            multiline: false,
            role: None,
        }
    }

    fn draft() -> Draft {
        Draft {
            action_label: "Send".into(),
            resource_label: "Mail".into(),
            fields: vec![
                field(
                    "to",
                    DraftValue::Text {
                        text: "ada@example.org".into(),
                    },
                ),
                field("count", DraftValue::Number { text: "3".into() }),
                field("urgent", DraftValue::Flag { on: false }),
            ],
            ..Draft::default()
        }
    }

    #[test]
    fn only_what_the_person_changed_leaves_with_the_answer() {
        let mut sheet = DraftSheet::new(&draft());
        assert_eq!(
            sheet.edits(),
            Ok(Vec::new()),
            "nothing changed, nothing sent"
        );
        assert!(sheet.set_text(0, "grace@example.org"));
        assert!(sheet.toggle(2));
        assert_eq!(
            sheet.edits().unwrap(),
            vec![
                DraftEdit {
                    path: vec!["to".into()],
                    text: "grace@example.org".into(),
                },
                DraftEdit {
                    path: vec!["urgent".into()],
                    text: "true".into(),
                },
            ]
        );
        assert!(sheet.set_text(1, "three"));
        assert!(sheet.edits().is_err(), "a number field holds a number");
    }

    fn request(draft: Draft) -> crate::shell::app_action_consent::AppActionConsentRequest {
        crate::shell::app_action_consent::AppActionConsentRequest {
            handle: "confirm:draft".into(),
            room: uuid::Uuid::nil().to_string(),
            person: "person:ada".into(),
            app_id: "scribe".into(),
            target_app: "so.tilo.mail".into(),
            resource: "mail.account".into(),
            action: "send".into(),
            resource_label: "Mail".into(),
            action_label: "Send".into(),
            risk: tilo_actions::RiskLevel::Sensitive,
            needs_confirmation: true,
            confirming: true,
            moderation: None,
            preview: None,
            scope: None,
            draft: Some(draft),
        }
    }

    /// The person changes the draft on the sheet with the keyboard and the
    /// pointer, at a laptop's size and a phone's, and allows it: their
    /// answer carries exactly what they changed.
    #[test]
    fn the_person_edits_the_draft_on_the_sheet_and_the_answer_carries_it() {
        use crate::shell::app_action_consent::{AppActionAnswer, AppActionConsent};
        use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};
        use winit::keyboard::{Key, NamedKey};

        for bounds in [
            Rect::new(0.0, 0.0, 1280.0, 800.0),
            Rect::new(0.0, 0.0, 420.0, 860.0),
        ] {
            let queue = PortalDialogQueue::default();
            let mut answer = queue.enqueue_action_grant(request(draft())).unwrap();
            let mut sheet = PortalDialogController::new(queue);
            assert!(sheet.pump());
            assert!(sheet.is_draft_sheet() && sheet.is_text_entry());
            let layout = sheet.layout(bounds).expect("the sheet fits");
            assert!(bounds.contains_rect(layout.sheet), "{bounds:?}");
            assert_eq!(layout.draft_rows.len(), 3, "every field on one page");
            for row in &layout.draft_rows {
                assert!(layout.sheet.contains_rect(row.control));
                assert!(row.control.height() >= 44.0, "a touch target");
            }
            // The keyboard starts in the first field: typing changes it.
            for _ in 0.."ada@example.org".len() {
                type_key(&mut sheet, Key::Named(NamedKey::Backspace));
            }
            for letter in ["b", "o", "b"] {
                assert!(type_key(&mut sheet, Key::Character(letter.into())));
            }
            // Tab to the number field; a word is refused, nothing answered.
            assert!(sheet.select_next(false));
            type_key(&mut sheet, Key::Named(NamedKey::Backspace));
            type_key(&mut sheet, Key::Character("x".into()));
            assert!(!sheet.confirm(), "a number field holds a number");
            assert!(sheet.entry_error().is_some());
            assert!(answer.try_recv().is_err());
            type_key(&mut sheet, Key::Named(NamedKey::Backspace));
            type_key(&mut sheet, Key::Character("5".into()));
            // A press on the check row ticks it.
            let layout = sheet.layout(bounds).unwrap();
            let flag = layout.draft_rows[2].control.center();
            assert!(sheet.click(flag, &layout));
            assert!(sheet.confirm(), "allowed");
            assert_eq!(
                answer.try_recv().unwrap(),
                AppActionAnswer {
                    consent: AppActionConsent::Once,
                    edits: vec![
                        DraftEdit {
                            path: vec!["to".into()],
                            text: "bob".into(),
                        },
                        DraftEdit {
                            path: vec!["count".into()],
                            text: "5".into(),
                        },
                        DraftEdit {
                            path: vec!["urgent".into()],
                            text: "true".into(),
                        },
                    ],
                }
            );
        }
    }

    /// Allowed as it was asked, the answer carries no edits: exactly as an
    /// answer did before drafts could be changed.
    #[test]
    fn an_unedited_draft_is_allowed_as_it_was_asked() {
        use crate::shell::app_action_consent::{AppActionAnswer, AppActionConsent};
        use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};

        let queue = PortalDialogQueue::default();
        let mut answer = queue.enqueue_action_grant(request(draft())).unwrap();
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        assert!(sheet.layout(Rect::new(0.0, 0.0, 1280.0, 800.0)).is_some());
        assert!(sheet.confirm());
        assert_eq!(
            answer.try_recv().unwrap(),
            AppActionAnswer::unedited(AppActionConsent::Once)
        );
    }

    /// A draft too long for the screen is paged, every page seen before
    /// Allow, on a sheet that keeps one height.
    #[test]
    fn a_long_draft_is_paged_and_every_page_seen_before_allow() {
        use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};

        let mut long = draft();
        for index in 0..8 {
            long.fields.push(DraftField {
                path: vec![format!("line{index}")],
                ..field("line", DraftValue::Text { text: "x".into() })
            });
        }
        let queue = PortalDialogQueue::default();
        let mut answer = queue.enqueue_action_grant(request(long)).unwrap();
        let mut sheet = PortalDialogController::new(queue);
        assert!(sheet.pump());
        let bounds = Rect::new(0.0, 0.0, 420.0, 860.0);
        let first = sheet.layout(bounds).unwrap();
        assert!(sheet.reviewing_more());
        let mut seen = first.draft_rows.len();
        while sheet.reviewing_more() {
            assert!(sheet.confirm(), "the next page, not an answer");
            assert!(answer.try_recv().is_err());
            let page = sheet.layout(bounds).unwrap();
            assert_eq!(page.sheet, first.sheet, "one height on every page");
            seen += page.draft_rows.len();
        }
        assert_eq!(seen, 11, "every field shown");
        assert!(sheet.confirm());
        assert!(answer.try_recv().is_ok());
    }

    #[test]
    fn rows_page_by_the_room_there_is() {
        let sheet = DraftSheet::new(&draft());
        assert_eq!(sheet.pages(1000.0), vec![0..3]);
        let one_row = LABEL_HEIGHT + LINE_CONTROL;
        assert_eq!(sheet.pages(one_row), vec![0..1, 1..2, 2..3]);
        assert_eq!(sheet.page_height(&(0..1)), one_row);
    }
}
