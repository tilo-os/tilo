//! Formula evaluation, through `IronCalc`.
//!
//! The sheets keep every cell's raw input in their own document; `IronCalc`
//! evaluates a workbook built from those inputs. A [`Calc`] keeps that
//! workbook between recalculations and writes only the cells that changed
//! since the last one, so an edit to a large sheet re-parses one formula
//! rather than all of them.
//!
//! Literal input is handed over as the value the app has already read it as
//! (a typed `$1,000` is stored as `1000` with a currency format, see
//! `infer.rs`), never as text for `IronCalc` to interpret again. Formulas go
//! over as written, except that a sheet a reference names is spelled the way
//! the workbook spells it: references match sheet names regardless of case,
//! `IronCalc`'s only exactly.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use ironcalc_base::Model;
use ironcalc_base::expressions::parser::{Node, new_parser_english};
use ironcalc_base::expressions::token::{Error, OpProduct, OpSum};
use ironcalc_base::expressions::types::CellReferenceRC;
use ironcalc_base::expressions::utils::{is_valid_column_number, is_valid_row};
use ironcalc_base::types::{Cell, FormulaValue, SpillValue};

use crate::model::{Addr, CellFormat, CellValue, FormulaError, parse_number};
use crate::refs::{fold_sheet, offset_refs, rename_sheets};
use crate::workbook::{SheetSnapshot, Workbook};
use ui::tilo_resource::Id;

/// The name the one sheet of a lone sheet's workbook goes by.
const LONE_SHEET: &str = "Sheet1";

/// One sheet's raw inputs by position, shared: a [`Calc`] keeps the inputs
/// it was given, and inputs it is given again (the same allocation) are
/// known to be unchanged without comparing them cell by cell.
pub type Inputs = Arc<HashMap<Addr, String>>;

/// Every sheet of a workbook by name and raw input, for evaluating one of
/// them whose formulas read the others.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    /// Each sheet's name and raw inputs, in workbook order.
    pub sheets: Vec<(String, HashMap<Addr, String>)>,
    /// Which of them is evaluated.
    pub own: usize,
}

impl Book {
    /// Whether any input of `raw` may read another sheet: a formula naming
    /// one with `!`. Only such a sheet is evaluated with its workbook.
    #[must_use]
    pub fn needed(raw: &HashMap<Addr, String>) -> bool {
        raw.values()
            .any(|raw| raw.starts_with('=') && raw.contains('!'))
    }
}

/// A workbook kept evaluated: `IronCalc`'s model of it and the raw inputs it
/// was last given, sheet by sheet.
pub struct Calc {
    /// `None` only when `IronCalc` refuses even an empty workbook.
    model: Option<Model<'static>>,
    /// Each sheet's name and the raw inputs written into it.
    fed: Vec<(String, Inputs)>,
    /// The workbook's sheet names by [`fold_sheet`], each to the first sheet
    /// spelled that way: what a formula's sheet reference is rewritten to.
    names: HashMap<String, String>,
}

impl Default for Calc {
    fn default() -> Self {
        Self::new()
    }
}

impl Calc {
    /// A calculator holding no sheets yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: None,
            fed: Vec::new(),
            names: HashMap::new(),
        }
    }

    /// Bring the workbook to `sheets` (each one's name and raw inputs, in
    /// workbook order) and evaluate it when anything changed. A sheet given
    /// the inputs it was last given is left as it is; otherwise cells are
    /// written only where their input differs from what the workbook holds.
    /// A change to the sheets themselves (added, removed, renamed or
    /// reordered) builds the workbook anew, since a formula reads the sheet
    /// it names as the workbook stood when the formula was written.
    pub fn update<'s>(&mut self, sheets: impl IntoIterator<Item = (&'s str, &'s Inputs)>) {
        let sheets: Vec<_> = sheets.into_iter().collect();
        let same_sheets = self.model.is_some()
            && self.fed.len() == sheets.len()
            && self
                .fed
                .iter()
                .zip(&sheets)
                .all(|((fed, _), (name, _))| fed.as_str() == *name);
        if !same_sheets {
            self.load(sheets.iter().map(|(name, raw)| (*name, &***raw)));
            self.fed = sheets
                .iter()
                .map(|(name, raw)| ((*name).to_owned(), Arc::clone(raw)))
                .collect();
            return;
        }
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let mut changed = false;
        for ((_, fed), ((_, raw), index)) in self.fed.iter_mut().zip(sheets.iter().zip(0u32..)) {
            if Arc::ptr_eq(fed, raw) {
                continue;
            }
            let mut writes: Vec<(Addr, &str)> = raw
                .iter()
                .filter(|(addr, input)| fed.get(addr) != Some(input))
                .map(|(addr, input)| (*addr, input.as_str()))
                .collect();
            writes.extend(
                fed.keys()
                    .filter(|addr| !raw.contains_key(addr))
                    .map(|addr| (*addr, "")),
            );
            changed |= !writes.is_empty();
            write_all(model, &self.names, index, raw, writes, false);
            *fed = Arc::clone(raw);
        }
        if changed {
            model.evaluate();
        }
    }

    /// A new workbook of `sheets` (each one's name and raw inputs, in
    /// workbook order), evaluated. Remembers no inputs: what it is given is
    /// only read.
    fn load<'s, S: BuildHasher + 's>(
        &mut self,
        sheets: impl Iterator<Item = (&'s str, &'s HashMap<Addr, String, S>)>,
    ) {
        let sheets: Vec<_> = sheets.collect();
        self.rebuild(sheets.iter().map(|(name, _)| *name));
        let Some(model) = self.model.as_mut() else {
            return;
        };
        for ((_, raw), index) in sheets.iter().zip(0u32..) {
            let writes = raw
                .iter()
                .map(|(addr, input)| (*addr, input.as_str()))
                .collect();
            write_all(model, &self.names, index, raw, writes, true);
        }
        model.evaluate();
    }

    /// A new workbook with one empty sheet per name.
    fn rebuild<'n>(&mut self, names: impl Iterator<Item = &'n str>) {
        self.fed.clear();
        self.names.clear();
        self.model = None;
        let names: Vec<&str> = names.collect();
        let Some(mut model) = Model::new_empty("workbook", "en", zone(), "en")
            .or_else(|_| Model::new_empty("workbook", "en", "UTC", "en"))
            .ok()
        else {
            return;
        };
        // `IronCalc` asks for valid, distinct sheet names, the app does not:
        // each sheet is added under a stand-in and then takes its own name,
        // which formulas reach once the workbook is read anew.
        for at in 1..names.len() {
            if model.add_sheet(&format!("S{at}")).is_err() {
                return;
            }
        }
        for (sheet, name) in model.workbook.worksheets.iter_mut().zip(&names) {
            (*name).clone_into(&mut sheet.name);
        }
        let Ok(model) = Model::from_workbook(model.workbook, "en") else {
            return;
        };
        for name in &names {
            self.names
                .entry(fold_sheet(name))
                .or_insert_with(|| (*name).to_owned());
        }
        self.model = Some(model);
    }

    /// What each cell of the sheet at `sheet` computes, the cells a formula
    /// spills its array into among them.
    #[must_use]
    pub fn values(&self, sheet: usize) -> HashMap<Addr, CellValue> {
        let Some(model) = &self.model else {
            return HashMap::new();
        };
        let Some(worksheet) = model.workbook.worksheets.get(sheet) else {
            return HashMap::new();
        };
        let strings = &model.workbook.shared_strings;
        worksheet
            .sheet_data
            .cells()
            .filter_map(|(row, column, cell)| {
                let addr = (
                    u32::try_from(column.checked_sub(1)?).ok()?,
                    u32::try_from(row.checked_sub(1)?).ok()?,
                );
                Some((addr, value_of(&cell, strings)?))
            })
            .collect()
    }
}

/// A workbook evaluated state by state: every sheet in one [`Calc`], so
/// showing another sheet reads values already computed, and a new state
/// re-reads only the sheets it changed.
#[derive(Default)]
pub struct WorkbookCalc {
    calc: Calc,
    /// The state last evaluated. Holding it keeps its sheets where they
    /// are, so a sheet of a later state at the same address is that sheet,
    /// unchanged.
    state: Option<Rc<Workbook>>,
    /// Each sheet's raw inputs as last read, by id.
    inputs: HashMap<Id, Inputs>,
    /// Each sheet's [`effective_formats`] as last worked out, by id, until
    /// the sheet changes: inferring them parses every formula.
    formats: HashMap<Id, HashMap<Addr, CellFormat>>,
}

impl WorkbookCalc {
    /// Evaluate `state`. `known` is one sheet's raw inputs the caller has
    /// read from `state` already, so they are not read again.
    pub fn update(&mut self, state: &Rc<Workbook>, known: Option<(&Id, &HashMap<Addr, String>)>) {
        let previous = self.state.replace(Rc::clone(state));
        let mut inputs = HashMap::with_capacity(state.sheets.len());
        for (id, sheet) in state.sheets.iter() {
            let unchanged = previous
                .as_ref()
                .and_then(|previous| previous.sheet(id))
                .is_some_and(|held| std::ptr::eq(held, sheet));
            if !unchanged {
                self.formats.remove(id);
            }
            let read = match self.inputs.remove(id) {
                Some(held) if unchanged => held,
                _ => match known {
                    Some((known, raw)) if known == id => Arc::new(raw.clone()),
                    _ => Arc::new(sheet.inputs()),
                },
            };
            inputs.insert(id.clone(), read);
        }
        self.inputs = inputs;
        self.formats.retain(|id, _| state.sheets.contains(id));
        let inputs = &self.inputs;
        self.calc.update(
            state
                .sheets
                .iter()
                .filter_map(|(id, sheet)| Some((sheet.name.0.as_str(), inputs.get(id)?))),
        );
    }

    /// The [`effective_formats`] of sheet `id`, whose cells `snap` holds as
    /// of the last update.
    pub fn formats(&mut self, id: &Id, snap: &SheetSnapshot) -> HashMap<Addr, CellFormat> {
        self.formats
            .entry(id.clone())
            .or_insert_with(|| effective_formats(snap))
            .clone()
    }

    /// What each cell of sheet `id` computes, as of the last update.
    #[must_use]
    pub fn values(&self, id: &Id) -> HashMap<Addr, CellValue> {
        self.state
            .as_ref()
            .and_then(|state| state.sheets.position(id))
            .map(|at| self.calc.values(at))
            .unwrap_or_default()
    }
}

/// Write each cell's raw input into sheet `sheet`, `""` clearing it. A
/// `fresh` sheet holds nothing yet, so there is nothing to clear first.
fn write_all<S: BuildHasher>(
    model: &mut Model<'_>,
    names: &HashMap<String, String>,
    sheet: u32,
    inputs: &HashMap<Addr, String, S>,
    mut writes: Vec<(Addr, &str)>,
    fresh: bool,
) {
    // Row by row, so the cell above a formula holds its input already when
    // the formula may be a copy of it.
    writes.sort_unstable_by_key(|((column, row), _)| (*row, *column));
    for (addr, raw) in writes {
        let above = addr
            .1
            .checked_sub(1)
            .and_then(|row| inputs.get(&(addr.0, row)));
        write(
            model,
            names,
            sheet,
            addr,
            raw,
            above.map(String::as_str),
            fresh,
        );
    }
}

/// Write `raw` into the cell at `addr` of sheet `sheet`, `""` clearing it.
fn write(
    model: &mut Model<'_>,
    names: &HashMap<String, String>,
    sheet: u32,
    addr: Addr,
    raw: &str,
    above: Option<&str>,
    fresh: bool,
) {
    let (Some(row), Some(column)) = (
        i32::try_from(addr.1)
            .ok()
            .and_then(|row| row.checked_add(1)),
        i32::try_from(addr.0)
            .ok()
            .and_then(|col| col.checked_add(1)),
    ) else {
        return;
    };
    // A cell past the last one `IronCalc` addresses has no value.
    if !is_valid_row(row) || !is_valid_column_number(column) {
        return;
    }
    // Clearing first drops what the cell held, the array a formula there
    // spilled included, and what of another formula's array it showed.
    if !fresh
        && model
            .set_user_input(sheet, row, column, String::new())
            .is_err()
    {
        return;
    }
    if raw.is_empty() {
        return;
    }
    // The value is all the app reads of a cell, so input goes in through
    // the calls that set it alone: `set_user_input` would also work out a
    // number format for each formula and look the cell's style up again.
    let written = if raw.starts_with('=') {
        if let Some(formula) =
            above.and_then(|above| copied_down(model, sheet, (row, column), above, raw))
        {
            model
                .get_cell_style_index(sheet, row, column)
                .and_then(|style| {
                    model
                        .workbook
                        .worksheet_mut(sheet)?
                        .set_cell_with_formula(row, column, formula, style)
                })
        } else if parse_error(raw).is_some() {
            set_error(model, sheet, row, column)
        } else {
            let formula = rename_sheets(raw, |written| {
                names
                    .get(&fold_sheet(written))
                    .filter(|name| name.as_str() != written)
                    .cloned()
            });
            model.update_cell_with_formula(sheet, row, column, formula)
        }
    } else {
        match literal(raw) {
            CellValue::Number(n) => model.update_cell_with_number(sheet, row, column, n),
            CellValue::Bool(b) => model.update_cell_with_bool(sheet, row, column, b),
            CellValue::Text(text) => model.update_cell_with_text(sheet, row, column, &text),
            CellValue::Empty | CellValue::Error(_) => Ok(()),
        }
    };
    if written.is_err() {
        // Nothing the cell could hold: it shows that it holds nothing valid.
        let _ = set_error(model, sheet, row, column);
    }
}

/// The formula of the cell above `(row, column)` when `raw` is its input,
/// `above`, copied one row down. `IronCalc` keeps a sheet's formulas by
/// their relative form, so the copy is that same formula and needs no
/// parsing: a column of formulas filled down, as large sheets mostly are,
/// is parsed once.
fn copied_down(
    model: &Model<'_>,
    sheet: u32,
    (row, column): (i32, i32),
    above: &str,
    raw: &str,
) -> Option<i32> {
    let formula = match model
        .workbook
        .worksheet(sheet)
        .ok()?
        .cell(row.checked_sub(1)?, column)
        .as_deref()?
    {
        Cell::CellFormula { f, .. } => *f,
        _ => return None,
    };
    (!names_whole_rows(raw) && offset_refs(above, 0, 1) == raw).then_some(formula)
}

/// Whether `raw` may name whole rows (`1:3`, `$2:$2`): a `:` with a row
/// number after it. A copy keeps those as written, where `IronCalc` reads
/// them relative to the cell, so such a formula is never taken for a copy.
/// Text like `"10:30"` answers yes too, which only costs a parse.
fn names_whole_rows(raw: &str) -> bool {
    raw.split(':').skip(1).any(|after| {
        after
            .trim_start_matches([' ', '$'])
            .starts_with(|c: char| c.is_ascii_digit())
    })
}

fn set_error(model: &mut Model<'_>, sheet: u32, row: i32, column: i32) -> Result<(), String> {
    model
        .workbook
        .worksheet_mut(sheet)?
        .set_cell_with_error(row, column, Error::ERROR, 0)
}

/// The value input that is no formula stands for: a number, `TRUE` or
/// `FALSE` in any case, otherwise the text as written.
fn literal(raw: &str) -> CellValue {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CellValue::Empty;
    }
    if let Some(n) = parse_number(trimmed) {
        return CellValue::Number(n);
    }
    if let Some(n) = trimmed
        .strip_suffix('%')
        .and_then(|number| parse_number(number.trim()))
    {
        return CellValue::Number(n / 100.0);
    }
    if trimmed.eq_ignore_ascii_case("TRUE") {
        return CellValue::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("FALSE") {
        return CellValue::Bool(false);
    }
    CellValue::Text(raw.to_string())
}

/// The value `IronCalc` holds in `cell`; `None` for an empty one.
fn value_of(cell: &Cell, strings: &[String]) -> Option<CellValue> {
    Some(match cell {
        Cell::EmptyCell { .. } => return None,
        Cell::BooleanCell { v, .. } => CellValue::Bool(*v),
        Cell::NumberCell { v, .. } => CellValue::Number(*v),
        Cell::ErrorCell { ei, .. } => CellValue::Error(error_of(ei)),
        Cell::SharedString { si, .. } => {
            CellValue::Text(strings.get(usize::try_from(*si).ok()?)?.clone())
        }
        Cell::CellFormula { v, .. } | Cell::ArrayFormula { v, .. } => match v {
            FormulaValue::Unevaluated => return None,
            FormulaValue::Boolean(b) => CellValue::Bool(*b),
            FormulaValue::Number(n) => CellValue::Number(*n),
            FormulaValue::Text(text) => CellValue::Text(text.clone()),
            FormulaValue::Error { ei, .. } => CellValue::Error(error_of(ei)),
        },
        Cell::SpillCell { v, .. } => match v {
            SpillValue::Boolean(b) => CellValue::Bool(*b),
            SpillValue::Number(n) => CellValue::Number(*n),
            SpillValue::Text(text) => CellValue::Text(text.clone()),
            SpillValue::Error(ei) => CellValue::Error(error_of(ei)),
        },
    })
}

fn error_of(error: &Error) -> FormulaError {
    match error {
        Error::REF => FormulaError::Ref,
        Error::NAME => FormulaError::Name,
        Error::VALUE => FormulaError::Value,
        Error::DIV => FormulaError::Div0,
        Error::NA => FormulaError::Na,
        Error::NUM => FormulaError::Num,
        Error::ERROR => FormulaError::Error,
        Error::NIMPL => FormulaError::NotImplemented,
        Error::SPILL => FormulaError::Spill,
        Error::CALC => FormulaError::Calc,
        Error::CIRC => FormulaError::Cycle,
        Error::NULL => FormulaError::Null,
    }
}

/// The local time zone's IANA name, which `TODAY()` and `NOW()` read the
/// date and time in.
fn zone() -> &'static str {
    static ZONE: OnceLock<String> = OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned()))
}

/// What each cell of one sheet's raw inputs computes, the sheet on its own.
pub fn recalculate<S: BuildHasher>(raw: &HashMap<Addr, String, S>) -> HashMap<Addr, CellValue> {
    let mut calc = Calc::new();
    calc.load([(LONE_SHEET, raw)].into_iter());
    calc.values(0)
}

/// What each cell of `snap` computes: with the workbook it came from when
/// its formulas read other sheets, on its own otherwise.
#[must_use]
pub fn recalculate_snapshot(snap: &SheetSnapshot) -> HashMap<Addr, CellValue> {
    let mut calc = Calc::new();
    match &snap.book {
        Some(book) => {
            calc.load(book.sheets.iter().map(|(name, raw)| (name.as_str(), raw)));
            calc.values(book.own)
        }
        None => recalculate(&snap.raw),
    }
}

/// Why a formula does not parse: the error its cell shows, and the problem
/// in words for whoever wrote it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    /// What the cell shows.
    pub error: FormulaError,
    /// What is wrong.
    pub problem: String,
}

/// `body` (a formula without its `=`) parsed as if in the first cell of a
/// sheet with no others: a reference to another sheet parses, and reads as
/// a broken one.
fn parse(body: &str) -> Node {
    let mut parser = new_parser_english(vec![LONE_SHEET.to_owned()], Vec::new(), HashMap::new());
    parser.parse(
        body,
        &CellReferenceRC {
            sheet: LONE_SHEET.to_owned(),
            column: 1,
            row: 1,
        },
    )
}

/// Why cell input `raw` is a formula that does not parse: `None` for a
/// formula that parses and for input that is no formula. The cell keeps the
/// input and shows the error, as a spreadsheet does.
pub fn parse_error(raw: &str) -> Option<ParseError> {
    let body = raw.strip_prefix('=')?;
    // IronCalc's parser stops at the end of the first whole expression and
    // ignores what follows, where a spreadsheet refuses the formula: `=1 2`
    // fails once wrapped in parentheses, which must close after it, and a
    // `)` that closes nothing is caught before the parser sees it.
    let broken = body.trim().is_empty()
        || closes_unopened(body)
        || [body, &format!("N(({body}))")]
            .iter()
            .any(|formula| matches!(parse(formula), Node::ParseErrorKind { .. }));
    broken.then(|| ParseError {
        error: FormulaError::Error,
        problem: explain(raw, FormulaError::Error),
    })
}

/// Whether `body` has a `)` with no `(` before it to close, outside text in
/// quotes and quoted sheet names.
fn closes_unopened(body: &str) -> bool {
    let mut depth = 0usize;
    let mut quote = None;
    for c in body.chars() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth = depth.saturating_add(1),
            (None, ')') => match depth.checked_sub(1) {
                Some(less) => depth = less,
                None => return true,
            },
            _ => {}
        }
    }
    false
}

/// A sentence saying why a cell holding `raw` evaluates to `err`. For a
/// formula that could not be read, it names the most likely mistake.
pub fn explain(raw: &str, err: FormulaError) -> String {
    match err {
        FormulaError::Error => {
            let body = raw.trim_start_matches('=').trim_end();
            let opens = body.matches('(').count();
            let closes = body.matches(')').count();
            if body.is_empty() {
                "The formula is empty: type a value or reference after =".into()
            } else if body.ends_with(['+', '-', '*', '/', '^', '&', '=', '<', '>', ',', ';']) {
                "The formula ends with an operator: add a value or reference after it".into()
            } else if opens > closes {
                "A parenthesis is opened but not closed".into()
            } else if closes > opens {
                "A closing parenthesis has no matching opening one".into()
            } else if body.matches('"').count() % 2 == 1 {
                "A text value is missing its closing quote".into()
            } else {
                "The formula could not be read: check for a missing value, operator or parenthesis"
                    .into()
            }
        }
        FormulaError::Div0 => "Division by zero or by an empty cell".into(),
        FormulaError::Name => "Unknown function or name".into(),
        FormulaError::Ref => "The formula refers to a cell that does not exist".into(),
        FormulaError::Value => {
            "A value has the wrong type for this operation, such as text in arithmetic".into()
        }
        FormulaError::Na => "No value is available, such as a lookup that found nothing".into(),
        FormulaError::Num => "The result is not a valid number".into(),
        FormulaError::Cycle => "The formula depends on its own cell (circular reference)".into(),
        FormulaError::Spill => {
            "The formula's result needs more cells, and some of them are not empty".into()
        }
        FormulaError::Calc => "The calculation has no result, such as an empty array".into(),
        FormulaError::Null => "Two ranges that do not meet are used as one".into(),
        FormulaError::NotImplemented => "This function is not supported yet".into(),
    }
}

// ---------------------------------------------------------------------------
// Format propagation
// ---------------------------------------------------------------------------

/// The unit a sub-expression carries while propagating formats. We track
/// the *category* (and the specific currency, when it's currency) rather
/// than a full `CellFormat`, because propagation cares about the unit, not
/// the exact rendering style.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    /// A bare scalar — `5`, `=2+2`, a General/Number cell. Multiplying a
    /// scalar by a currency yields that currency (`5 * $100 = $500`).
    Scalar,
    /// Money in a specific currency.
    Currency(tilo_locale::Currency),
    /// A percentage value.
    Percent,
    Date(tilo_locale::DateStyle),
    /// A time/duration serial.
    Time(ui::clock::TimeStyle),
    /// Conflicting units (e.g. USD + EUR) — render plain.
    Mixed,
}

/// The arithmetic operators units combine through.
#[derive(Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

/// The format each cell of `snap` shows in: its own, else, for a formula,
/// the one it takes from its operands ([`infer_result_format`]).
#[must_use]
pub fn effective_formats(snap: &SheetSnapshot) -> HashMap<Addr, CellFormat> {
    let mut fmt = snap.fmt.clone();
    for (addr, raw) in &snap.raw {
        if !snap.fmt.contains_key(addr)
            && let Some(body) = raw.strip_prefix('=')
            && let Some(inferred) = infer_result_format(body, &snap.fmt)
        {
            fmt.insert(*addr, inferred);
        }
    }
    fmt
}

/// Infer the display format a computed cell should adopt from the formats
/// of the cells its formula references. Returns `None` when nothing useful
/// can be inferred (the caller then leaves the cell's own format, defaulting
/// to General).
///
/// Rules, mirroring Excel/Sheets behaviour:
///   * `=5*B2` where B2 is currency → that currency (a scalar times money
///     stays money). Likewise `B2/2`, `B2+B3`, `-B2`.
///   * currency `+`/`-` currency of the **same** currency → that currency;
///     **different** currencies → plain number (Mixed).
///   * percent through `+`/`-`/scalar `*` → percent.
///   * date `-` date → a duration (Number); date `+` scalar → date.
///   * SUM/AVERAGE/MIN/MAX/etc. over a uniformly-formatted range inherit
///     that format.
///   * anything ambiguous → `None` (plain).
pub fn infer_result_format<S: BuildHasher>(
    body: &str,
    formats: &HashMap<Addr, CellFormat, S>,
) -> Option<CellFormat> {
    match unit_of(&parse(body), formats) {
        Unit::Scalar | Unit::Mixed => None,
        Unit::Currency(c) => Some(CellFormat::Currency(c)),
        Unit::Percent => Some(CellFormat::Percent),
        Unit::Date(s) => Some(CellFormat::Date(s)),
        Unit::Time(s) => Some(CellFormat::Time(s)),
    }
}

/// The unit a cell's stored format implies.
fn unit_of_format(fmt: CellFormat) -> Unit {
    match fmt {
        CellFormat::Currency(c) => Unit::Currency(c),
        CellFormat::Percent => Unit::Percent,
        CellFormat::Date(s) => Unit::Date(s),
        CellFormat::Time(s) => Unit::Time(s),
        // General/Number/Boolean/Text all behave as scalars for propagation.
        _ => Unit::Scalar,
    }
}

/// The cell a reference [`parse`] read names: relative to the first cell,
/// where it was parsed, unless anchored.
fn referenced(row: i32, column: i32, absolute_row: bool, absolute_column: bool) -> Option<Addr> {
    let at = |n: i32, absolute: bool| {
        let one_based = if absolute { n } else { n.checked_add(1)? };
        u32::try_from(one_based.checked_sub(1)?).ok()
    };
    Some((at(column, absolute_column)?, at(row, absolute_row)?))
}

/// Recursively determine an expression's unit.
fn unit_of<S: BuildHasher>(node: &Node, formats: &HashMap<Addr, CellFormat, S>) -> Unit {
    match node {
        Node::ReferenceKind {
            sheet_name: None,
            row,
            column,
            absolute_row,
            absolute_column,
            ..
        } => referenced(*row, *column, *absolute_row, *absolute_column)
            .and_then(|addr| formats.get(&addr))
            .map_or(Unit::Scalar, |f| unit_of_format(*f)),
        Node::UnaryKind { right, .. } => unit_of(right, formats),
        Node::ImplicitIntersection { child, .. } => unit_of(child, formats),
        Node::OpSumKind { kind, left, right } => {
            let op = match kind {
                OpSum::Add => Op::Add,
                OpSum::Minus => Op::Sub,
            };
            combine(op, unit_of(left, formats), unit_of(right, formats))
        }
        Node::OpProductKind { kind, left, right } => {
            let op = match kind {
                OpProduct::Times => Op::Mul,
                OpProduct::Divide => Op::Div,
            };
            combine(op, unit_of(left, formats), unit_of(right, formats))
        }
        Node::OpPowerKind { left, right } => {
            combine(Op::Pow, unit_of(left, formats), unit_of(right, formats))
        }
        Node::FunctionKind { kind, args } => {
            let name = kind.to_xlsx_string();
            unit_of_call(name.rsplit('.').next().unwrap_or_default(), args, formats)
        }
        _ => Unit::Scalar,
    }
}

/// Combine two operand units under a binary operator.
fn combine(op: Op, l: Unit, r: Unit) -> Unit {
    match op {
        // Division of two equal units is a ratio → unitless (`$/$`, `%/%`).
        Op::Div if l == r && l != Unit::Scalar => Unit::Scalar,
        // Multiplication/division by a scalar keeps the unit; any other
        // pairing of two real units is ambiguous (Mixed).
        Op::Mul | Op::Div | Op::Pow => match (l, r) {
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
        // Addition/subtraction: same unit stays; date−date becomes a
        // duration (treated as Number → Scalar); unit ± scalar keeps unit.
        Op::Add | Op::Sub => match (l, r) {
            // date − date is a span of days, not a date.
            (Unit::Date(_), Unit::Date(_)) if op == Op::Sub => Unit::Scalar,
            (a, b) if a == b => a,
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
    }
}

/// The unit a function call produces. Aggregates over a range inherit the
/// range's uniform unit; ROUND/ABS/etc. inherit their first argument.
fn unit_of_call<S: BuildHasher>(
    name: &str,
    args: &[Node],
    formats: &HashMap<Addr, CellFormat, S>,
) -> Unit {
    match name {
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "MEDIAN" | "PRODUCT" | "ROUND" | "ROUNDUP"
        | "ROUNDDOWN" | "ABS" | "FLOOR" | "CEILING" | "INT" => {
            let mut acc: Option<Unit> = None;
            for arg in args {
                for u in arg_units(arg, formats) {
                    acc = Some(match acc {
                        None | Some(Unit::Scalar) => u,
                        Some(prev) if prev == u || u == Unit::Scalar => prev,
                        Some(_) => Unit::Mixed,
                    });
                }
            }
            acc.unwrap_or(Unit::Scalar)
        }
        // Date/time builders render as dates/times so a bare `=TODAY()` shows
        // a date rather than its underlying serial number.
        "DATE" | "TODAY" | "NOW" => Unit::Date(tilo_locale::DateStyle::Iso),
        "TIME" => Unit::Time(ui::clock::TimeStyle::H24),
        // Counting / text / logical / lookup functions are unitless.
        _ => Unit::Scalar,
    }
}

/// Units contributed by a single argument — a range contributes the unit of
/// each formatted cell it spans (an unformatted one is a scalar, which
/// leaves an aggregate's unit as it is).
fn arg_units<S: BuildHasher>(arg: &Node, formats: &HashMap<Addr, CellFormat, S>) -> Vec<Unit> {
    match arg {
        Node::RangeKind {
            sheet_name: None,
            absolute_row1,
            absolute_column1,
            row1,
            column1,
            absolute_row2,
            absolute_column2,
            row2,
            column2,
            ..
        } => {
            let (Some(a), Some(b)) = (
                referenced(*row1, *column1, *absolute_row1, *absolute_column1),
                referenced(*row2, *column2, *absolute_row2, *absolute_column2),
            ) else {
                return Vec::new();
            };
            let (c0, c1) = (a.0.min(b.0), a.0.max(b.0));
            let (r0, r1) = (a.1.min(b.1), a.1.max(b.1));
            formats
                .iter()
                .filter(|((c, r), _)| (c0..=c1).contains(c) && (r0..=r1).contains(r))
                .map(|(_, f)| unit_of_format(*f))
                .collect()
        }
        Node::ImplicitIntersection { child, .. } => arg_units(child, formats),
        _ => vec![unit_of(arg, formats)],
    }
}

#[cfg(test)]
#[path = "calc_tests.rs"]
mod tests;
