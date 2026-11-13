//! How the calculator's changes show in the shared History view: a calculation
//! typeset as the display sets it with its result, a graph change as the
//! function it plotted. A face is made from the change's recorded facts alone,
//! so it reads the same after the record dropped it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::charts::ChartPalette;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tilo_actions::{Did, EditOp, FieldEdit, PathStep};
use ui::tokens::TextRole;
use ui::widget::{EntryBody, EntryFace, HistoryEntry, HistoryEntryPresenter};
use vello::kurbo::{BezPath, Point, Rect, Stroke};

use crate::actions::GraphKind;
use crate::draw::{self, Align};
use crate::engine;
use crate::mathfield::MathField;

/// The x range a thumbnail plots, in the graph's units.
const THUMBNAIL_X: std::ops::RangeInclusive<f64> = -5.0..=5.0;
const THUMBNAIL_SAMPLES: usize = 48;

/// Each curve's place in the list, by row id: the colour it is drawn in.
type Series = Rc<RefCell<HashMap<String, usize>>>;

#[derive(Default)]
pub struct CalculatorEntries {
    series: Series,
}

impl CalculatorEntries {
    /// The curves now listed, in order.
    pub fn set_curves<'a>(&self, rows: impl IntoIterator<Item = &'a str>) {
        let mut series = self.series.borrow_mut();
        series.clear();
        series.extend(
            rows.into_iter()
                .enumerate()
                .map(|(at, id)| (id.to_owned(), at)),
        );
    }
}

impl HistoryEntryPresenter for CalculatorEntries {
    fn face(&self, entry: &HistoryEntry) -> Option<EntryFace> {
        if let Some((expression, result)) = calculation(entry) {
            let spoken = format!("{expression} = {result}");
            return Some(
                EntryFace::new(spoken)
                    .with_body(Rc::new(CalculationBody::new(&expression, &result)))
                    .copying(result)
                    .opening(),
            );
        }
        let (kind, formula) = plotted(entry)?;
        let spoken = format!("{} = {formula}", kind.variable());
        let curve = (kind == GraphKind::Curve2d)
            .then(|| row_of(entry))
            .flatten()
            .map(|row| (row, Rc::clone(&self.series)));
        Some(EntryFace::new(spoken).with_body(Rc::new(GraphBody::new(kind, &formula, curve))))
    }
    /// A band reads as the functions written in it once each, its
    /// calculations, and a count of other changes.
    fn group_summary(&self, entries: &[HistoryEntry]) -> Option<String> {
        let calculations = entries
            .iter()
            .filter(|entry| calculation(entry).is_some())
            .count();
        let mut rows = std::collections::BTreeSet::new();
        let mut unrowed = 0_usize;
        for entry in entries.iter().filter(|entry| plotted(entry).is_some()) {
            match row_of(entry) {
                Some(row) => {
                    rows.insert(row);
                }
                None => unrowed += 1,
            }
        }
        let functions = rows.len() + unrowed;
        if calculations + functions == 0 {
            return None;
        }
        let others = entries
            .iter()
            .filter(|entry| {
                calculation(entry).is_none()
                    && plotted(entry).is_none()
                    && row_of(entry).is_none_or(|row| !rows.contains(&row))
            })
            .count();
        let count = |key: &str, count: usize| {
            crate::resource::CATALOG
                .t_args(key, &[("count", ui::i18n::ArgValue::Number(count as f64))])
        };
        let parts: Vec<String> = [
            ("calculator-history-functions", functions),
            ("calculator-history-calculations", calculations),
            ("calculator-history-other-changes", others),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| count(key, n))
        .collect();
        Some(parts.join(", "))
    }
}

/// The expression and result a `commit-history` change made.
pub fn calculation(entry: &HistoryEntry) -> Option<(String, String)> {
    let Some(Did::Verb(call)) = entry.context.as_ref()?.what.as_ref() else {
        return None;
    };
    if call.action.action != "commit-history" {
        return None;
    }
    let text = |name: &str| {
        call.args
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .map(str::to_owned)
    };
    let expression = text("expr").filter(|expression| !expression.is_empty())?;
    Some((expression, text("result").unwrap_or_default()))
}

/// The function a graph change plotted.
fn plotted(entry: &HistoryEntry) -> Option<(GraphKind, String)> {
    let context = entry.context.as_ref()?;
    let formula = match context.what.as_ref()? {
        Did::Verb(call) => {
            let kind = match call.action.action.as_str() {
                "add-function" => GraphKind::Curve2d,
                "add-surface" => GraphKind::Surface3d,
                _ => return None,
            };
            let formula = call.args.get("formula")?.as_str()?.trim().to_owned();
            return (!formula.is_empty()).then_some((kind, formula));
        }
        Did::Edit(edit) => match edit.op {
            // A formula typed into a row.
            EditOp::Replace | EditOp::Splice if edit.field == "expr" => edit
                .new
                .as_ref()
                .and_then(|new| new.as_str())
                .map(str::to_owned)
                .or_else(|| written_formula(context)),
            // A row added with its formula: the item's label is its formula.
            EditOp::Insert if graph_kind(edit).is_some() && edit.new.is_none() => {
                written_formula(context)
            }
            _ => None,
        },
        Did::Group(_) | Did::History(_) => None,
    }?;
    let formula = formula.trim().to_owned();
    if formula.is_empty() {
        return None;
    }
    Some((graph_kind(edit_of(context)?)?, formula))
}

/// A row with no formula yet is offered under its id, which is not a formula.
fn written_formula(context: &ui::tilo_actions::ActionContext) -> Option<String> {
    context
        .item
        .as_ref()
        .filter(|item| item.label != item.id)
        .map(|item| item.label.clone())
}

/// The item its edit names, or the item it addressed.
fn row_of(entry: &HistoryEntry) -> Option<String> {
    let context = entry.context.as_ref()?;
    edit_of(context)
        .and_then(|edit| {
            edit.path.iter().find_map(|step| match step {
                PathStep::Item(id) => Some(id.clone()),
                _ => None,
            })
        })
        .or_else(|| context.item.as_ref().map(|item| item.id.clone()))
}

fn edit_of(context: &ui::tilo_actions::ActionContext) -> Option<&FieldEdit> {
    match context.what.as_ref()? {
        Did::Edit(edit) => Some(edit),
        _ => None,
    }
}

fn graph_kind(edit: &FieldEdit) -> Option<GraphKind> {
    let names = |list: &str| {
        edit.field == list
            || edit
                .path
                .iter()
                .any(|step| matches!(step, PathStep::Field(field) if field == list))
    };
    if names(GraphKind::Curve2d.field()) {
        Some(GraphKind::Curve2d)
    } else if names(GraphKind::Surface3d.field()) {
        Some(GraphKind::Surface3d)
    } else {
        None
    }
}

impl GraphKind {
    /// `y` for a curve, `z` for a surface.
    fn variable(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "y",
            GraphKind::Surface3d => "z",
        }
    }
}

/// A calculation: the expression in the math layout, its result beneath.
struct CalculationBody {
    field: RefCell<MathField>,
    expression: String,
    result: String,
}

impl CalculationBody {
    fn new(expression: &str, result: &str) -> Self {
        Self {
            // Read-only, and following: an undo or a peer can change it.
            field: RefCell::new(MathField::new(expression).follow_source()),
            expression: expression.to_owned(),
            result: result.to_owned(),
        }
    }
}

impl EntryBody for CalculationBody {
    fn height(&self, theme: &Theme) -> f64 {
        let pad_y = f64::from(theme.tokens.spacing.s1);
        let result = f64::from(theme.text(TextRole::Title).numeric().line_height_px());
        2.0 * pad_y + formula_line(theme) + result
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let pad_y = f64::from(theme.tokens.spacing.s1);
        let result_style = theme.text(TextRole::Title).numeric();
        let result_h = f64::from(result_style.line_height_px()).min(rect.height() / 2.0);
        let expression = Rect::new(
            rect.x0,
            rect.y0 + pad_y,
            rect.x1,
            rect.y1 - pad_y - result_h,
        );
        let mut field = self.field.borrow_mut();
        field.set_source(&self.expression);
        field.set_size(formula_size(theme));
        field.set_rect(expression, ctx.text);
        field.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            colors.text_dim,
            false,
        );
        let result = Rect::new(rect.x0, expression.y1, rect.x1, expression.y1 + result_h);
        draw::text_in(
            ctx,
            result,
            &self.result,
            &result_style,
            colors.text_primary,
            Align::Right,
        );
    }
}

fn formula_line(theme: &Theme) -> f64 {
    let set = f64::from(formula_size(theme));
    (set * 1.25).max(f64::from(theme.text(TextRole::Body).line_height_px()))
        + f64::from(theme.tokens.spacing.s2)
}

/// The body face. A role is already resolved against the reader's text scale,
/// and the math layout paints at the size it is given.
fn formula_size(theme: &Theme) -> f32 {
    theme.tokens.typography.size(TextRole::Body)
}

/// A plotted function typeset, and for a curve a small plot beside it.
struct GraphBody {
    kind: GraphKind,
    field: RefCell<MathField>,
    /// The formula the row was made from; the field shows it.
    formula: String,
    /// `None` where it is undefined; empty for a surface or an unparseable
    /// formula.
    samples: Vec<Option<f64>>,
    /// The row and the list it is looked up in: drawn in its colour, or
    /// quietly once it was removed.
    curve: Option<(String, Series)>,
}

impl GraphBody {
    fn new(kind: GraphKind, formula: &str, curve: Option<(String, Series)>) -> Self {
        let samples = match kind {
            GraphKind::Curve2d => engine::compile(formula, false, false)
                .map(|curve| {
                    (0..=THUMBNAIL_SAMPLES)
                        .map(|i| {
                            let t = i as f64 / THUMBNAIL_SAMPLES as f64;
                            let (low, high) = (*THUMBNAIL_X.start(), *THUMBNAIL_X.end());
                            let y = curve.eval1(low + (high - low) * t);
                            y.is_finite().then_some(y)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            GraphKind::Surface3d => Vec::new(),
        };
        Self {
            kind,
            // Read-only, and following: an edited formula is drawn as it is
            // now.
            field: RefCell::new(MathField::new(formula).follow_source()),
            formula: formula.into(),
            samples,
            curve,
        }
    }

    /// Its series colour while listed, the dim text colour once it is gone.
    fn ink(&self, theme: &Theme) -> vello::peniko::Color {
        let listed = self
            .curve
            .as_ref()
            .and_then(|(row, series)| series.borrow().get(row).copied());
        match listed {
            Some(at) => ChartPalette::from_theme(theme).categorical(at),
            None => theme.tokens.colors.text_dim,
        }
    }

    fn thumbnail(&self, frame: Rect) -> Option<BezPath> {
        let defined: Vec<f64> = self.samples.iter().flatten().copied().collect();
        let low = defined.iter().copied().fold(f64::INFINITY, f64::min);
        let high = defined.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if !low.is_finite() || !high.is_finite() {
            return None;
        }
        let span = (high - low).max(1e-9);
        let mut path = BezPath::new();
        let mut drawing = false;
        let last = self.samples.len().saturating_sub(1).max(1) as f64;
        for (i, sample) in self.samples.iter().enumerate() {
            let Some(y) = sample else {
                drawing = false;
                continue;
            };
            let point = Point::new(
                frame.x0 + frame.width() * i as f64 / last,
                frame.y1 - frame.height() * (y - low) / span,
            );
            if drawing {
                path.line_to(point);
            } else {
                path.move_to(point);
                drawing = true;
            }
        }
        Some(path)
    }
}

impl EntryBody for GraphBody {
    /// Two formula lines: room for a fraction or a root over its radicand.
    fn height(&self, theme: &Theme) -> f64 {
        let pad_y = f64::from(theme.tokens.spacing.s1);
        2.0 * pad_y + 2.0 * formula_line(theme)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let gap = f64::from(theme.tokens.spacing.s2);
        let pad_y = f64::from(theme.tokens.spacing.s1);
        let thumb_w = if self.samples.is_empty() {
            0.0
        } else {
            (rect.height() * 1.25).min(rect.width() / 4.0)
        };
        let label_style = theme.text(TextRole::Body);
        let (label_w, _) =
            ctx.text
                .measure_style(&format!("{} =", self.kind.variable()), None, &label_style);
        let label = Rect::new(rect.x0, rect.y0, rect.x0 + f64::from(label_w), rect.y1);
        draw::text_in(
            ctx,
            label,
            &format!("{} =", self.kind.variable()),
            &label_style,
            colors.text_dim,
            Align::Left,
        );
        let formula = Rect::new(
            label.x1 + gap,
            rect.y0 + pad_y,
            (rect.x1 - thumb_w - if thumb_w > 0.0 { gap } else { 0.0 }).max(label.x1 + gap),
            rect.y1 - pad_y,
        );
        let mut field = self.field.borrow_mut();
        field.set_source(&self.formula);
        field.set_size(formula_size(theme));
        field.set_rect(formula, ctx.text);
        field.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            colors.text_primary,
            false,
        );
        if thumb_w > 0.0 {
            let frame = Rect::new(
                rect.x1 - thumb_w,
                rect.y0 + 2.0 * pad_y,
                rect.x1,
                rect.y1 - 2.0 * pad_y,
            );
            if let Some(path) = self.thumbnail(frame) {
                let stroke = Stroke::new(1.5 * f64::from(theme.ui_scale));
                ctx.scene
                    .stroke(&stroke, ctx.transform, self.ink(theme), None, &path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::{ActionContext, ActionItem, ActionKey, Scalar, VerbCall};
    use ui::widget::{ArtifactKind, EntryKind, HistoryAuthor, HistoryEntryId};

    fn entry(what: Did, item: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([7; 16]),
            author: HistoryAuthor::new("You", 0.0, "you"),
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::Note,
            timestamp: ui::time::SystemTime::UNIX_EPOCH,
            summary: "Changed the document".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(ActionContext {
                what: Some(what),
                item: item.map(|label| ActionItem {
                    id: "row".into(),
                    label: label.into(),
                }),
                ..ActionContext::default()
            }),
        }
    }

    fn verb(action: &str, args: &[(&str, &str)]) -> Did {
        Did::Verb(VerbCall::new(
            ActionKey::new("calculator", "calculator.document", action),
            args.iter()
                .map(|(name, value)| ((*name).to_owned(), Scalar::text(value))),
        ))
    }

    fn formula_edit(list: &str, op: EditOp, new: Option<&str>) -> Did {
        Did::Edit(FieldEdit {
            ty: "plot-fn".into(),
            field: "expr".into(),
            path: vec![
                PathStep::Field(list.into()),
                PathStep::Item("row".into()),
                PathStep::Field("expr".into()),
            ],
            op,
            old: None,
            new: new.map(Scalar::text),
            args: std::collections::BTreeMap::new(),
        })
    }

    #[test]
    fn a_calculation_reads_as_itself_opens_and_copies_its_result() {
        let face = CalculatorEntries::default()
            .face(&entry(
                verb(
                    "commit-history",
                    &[("expr", "sqrt(2)/2"), ("result", "0.7071")],
                ),
                None,
            ))
            .expect("a calculation has a face");
        assert_eq!(face.spoken, "sqrt(2)/2 = 0.7071");
        assert_eq!(face.copy.as_deref(), Some("0.7071"));
        assert!(face.opens, "choosing it puts it back into the calculator");
        assert!(face.body.is_some(), "it is typeset, not a sentence");
    }

    #[test]
    fn a_graph_change_shows_the_function_it_plotted() {
        let curve = CalculatorEntries::default()
            .face(&entry(
                formula_edit("funcs", EditOp::Replace, Some("x^2-1")),
                Some("x^2"),
            ))
            .expect("a typed formula has a face");
        assert_eq!(curve.spoken, "y = x^2-1", "the formula after the change");
        assert!(!curve.opens && curve.copy.is_none());

        let surface = CalculatorEntries::default()
            .face(&entry(verb("add-surface", &[("formula", "x*y")]), None))
            .expect("an added surface has a face");
        assert_eq!(surface.spoken, "z = x*y");

        let body = GraphBody::new(GraphKind::Curve2d, "x^2-1", None);
        assert!(
            body.thumbnail(Rect::new(0.0, 0.0, 60.0, 30.0)).is_some(),
            "a curve plots"
        );
        assert!(
            GraphBody::new(GraphKind::Surface3d, "x*y", None)
                .samples
                .is_empty()
        );
    }

    #[test]
    fn a_blank_function_row_is_not_read_as_its_id() {
        let added = Did::Edit(FieldEdit {
            ty: "plot-fn".into(),
            field: "funcs".into(),
            path: vec![
                PathStep::Field("funcs".into()),
                PathStep::Item("row".into()),
            ],
            op: EditOp::Insert,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        });
        assert!(
            CalculatorEntries::default()
                .face(&entry(added.clone(), Some("row")))
                .is_none(),
            "a blank row has no formula to show"
        );
        let face = CalculatorEntries::default()
            .face(&entry(added, Some("x^2")))
            .expect("a row added with its formula shows it");
        assert_eq!(face.spoken, "y = x^2");
        assert!(
            CalculatorEntries::default()
                .face(&entry(
                    formula_edit("funcs", EditOp::Replace, None),
                    Some("row")
                ))
                .is_none(),
            "a formula cleared to nothing is not read as the row's id"
        );
    }

    #[test]
    fn a_band_counts_its_functions_and_calculations() {
        let calculation = || {
            entry(
                verb("commit-history", &[("expr", "1+2"), ("result", "3")]),
                None,
            )
        };
        let curve = entry(verb("add-function", &[("formula", "x")]), None);
        let other = entry(verb("clear-history", &[]), None);
        let entries = [calculation(), calculation(), curve, other];
        assert_eq!(
            CalculatorEntries::default()
                .group_summary(&entries)
                .as_deref(),
            Some("1 function, 2 calculations, 1 other change")
        );
        let none = [entry(verb("clear-history", &[]), None)];
        assert_eq!(CalculatorEntries::default().group_summary(&none), None);
    }

    #[test]
    fn a_band_counts_a_function_once_however_it_was_typed() {
        let added = entry(
            Did::Edit(FieldEdit {
                ty: "plot-fn".into(),
                field: "funcs".into(),
                path: vec![
                    PathStep::Field("funcs".into()),
                    PathStep::Item("row".into()),
                ],
                op: EditOp::Insert,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            }),
            Some("row"),
        );
        let typed = |text: &str| entry(formula_edit("funcs", EditOp::Replace, Some(text)), None);
        let entries = [added, typed("x"), typed("x^"), typed("x^2")];
        assert_eq!(
            CalculatorEntries::default()
                .group_summary(&entries)
                .as_deref(),
            Some("1 function")
        );
    }

    #[test]
    fn other_changes_read_as_their_sentence() {
        for what in [
            verb("clear-history", &[]),
            verb("delete-function", &[("function", "row")]),
            Did::Edit(FieldEdit {
                ty: "plot-fn".into(),
                field: "visible".into(),
                path: vec![PathStep::Field("funcs".into())],
                op: EditOp::Set,
                old: None,
                new: Some(Scalar::of(&false).unwrap()),
                args: std::collections::BTreeMap::new(),
            }),
        ] {
            assert!(
                CalculatorEntries::default()
                    .face(&entry(what, Some("x")))
                    .is_none()
            );
        }
    }
}
