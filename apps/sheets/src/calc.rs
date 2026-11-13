//! Formula evaluation through `IronCalc`. The sheets keep every cell's raw input; a [`Calc`]
//! keeps a workbook built from them and writes only the cells that changed, so an edit
//! re-parses one formula rather than all. Literal input arrives as the value the app already
//! read it as, never as text to interpret again.

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

/// The sheet name a lone sheet's workbook uses.
const LONE_SHEET: &str = "Sheet1";

/// One sheet's raw inputs by position, shared: inputs given again as the same allocation are
/// known unchanged without comparing cells.
pub type Inputs = Arc<HashMap<Addr, String>>;

/// Every sheet's raw inputs by name, for evaluating one whose formulas read the others.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Book {
    pub sheets: Vec<(String, HashMap<Addr, String>)>,
    /// Which of them is evaluated.
    pub own: usize,
}

impl Book {
    /// Whether any input may read another sheet with a `!`; only such a sheet is evaluated
    /// with its workbook.
    #[must_use]
    pub fn needed(raw: &HashMap<Addr, String>) -> bool {
        raw.values()
            .any(|raw| raw.starts_with('=') && raw.contains('!'))
    }
}

/// A workbook kept evaluated: `IronCalc`'s model and the raw inputs it was last given.
pub struct Calc {
    /// `None` only when `IronCalc` refuses even an empty workbook.
    model: Option<Model<'static>>,
    /// Each sheet's name and the inputs written into it.
    fed: Vec<(String, Inputs)>,
    /// The workbook's sheet names by [`fold_sheet`], each to the first sheet spelled that way.
    names: HashMap<String, String>,
}

impl Default for Calc {
    fn default() -> Self {
        Self::new()
    }
}

impl Calc {
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: None,
            fed: Vec::new(),
            names: HashMap::new(),
        }
    }

    /// Bring the workbook to `sheets` and evaluate it when anything changed. A sheet given the
    /// inputs it had is left as it is; a change to the sheets themselves rebuilds the workbook,
    /// since a formula reads the sheet it names as the workbook stood.
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

    /// A new workbook of `sheets`, evaluated; it remembers no inputs.
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
        // `IronCalc` asks for valid, distinct sheet names, the app does not: each sheet is added
        // under a stand-in and then takes its own name.
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

    /// What each cell of the sheet computes, spilled cells among them.
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

/// A workbook evaluated state by state, so showing another sheet reads values already computed
/// and a new state re-reads only what changed.
#[derive(Default)]
pub struct WorkbookCalc {
    calc: Calc,
    /// The state last evaluated; holding it keeps its sheets where they are.
    state: Option<Rc<Workbook>>,
    /// Each sheet's raw inputs as last read, by id.
    inputs: HashMap<Id, Inputs>,
    /// Each sheet's [`effective_formats`] as last worked out, until the sheet changes: inferring
    /// them parses every formula.
    formats: HashMap<Id, HashMap<Addr, CellFormat>>,
}

impl WorkbookCalc {
    /// Evaluate `state`; `known` is one sheet's inputs the caller already read.
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

    /// The effective formats of sheet `id`, whose cells `snap` holds as of the last update.
    pub fn formats(&mut self, id: &Id, snap: &SheetSnapshot) -> HashMap<Addr, CellFormat> {
        self.formats
            .entry(id.clone())
            .or_insert_with(|| effective_formats(snap))
            .clone()
    }

    #[must_use]
    pub fn values(&self, id: &Id) -> HashMap<Addr, CellValue> {
        self.state
            .as_ref()
            .and_then(|state| state.sheets.position(id))
            .map(|at| self.calc.values(at))
            .unwrap_or_default()
    }
}

/// Each cell's input written into the sheet, `""` clearing it; a `fresh` sheet has nothing to
/// clear first.
fn write_all<S: BuildHasher>(
    model: &mut Model<'_>,
    names: &HashMap<String, String>,
    sheet: u32,
    inputs: &HashMap<Addr, String, S>,
    mut writes: Vec<(Addr, &str)>,
    fresh: bool,
) {
    // Row by row, so the cell above a formula holds its input before the formula copies it.
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

/// Write `raw` into the cell, `""` clearing it.
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
    // Clearing first drops what the cell held, including a spilled array and another formula's
    // array shown there.
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
    // The value is all the app reads, so input goes through the calls that set it alone:
    // `set_user_input` would also work out a number format per formula.
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

/// The formula of the cell above, when `raw` is its input copied one row down. `IronCalc`
/// keeps formulas in relative form, so the copy needs no parsing.
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

/// Whether `raw` names whole rows (`1:3`, `$2:$2`), which a copy keeps as written. Text like
/// `"10:30"` answers yes too, which only costs a parse.
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

/// What input that is no formula means: a number, `TRUE` or `FALSE`, otherwise the text.
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
    if trimmed.eq_ignore_ascii_case("true") {
        CellValue::Bool(true)
    } else if trimmed.eq_ignore_ascii_case("false") {
        CellValue::Bool(false)
    } else {
        CellValue::Text(raw.to_string())
    }
}

/// The value `IronCalc` holds in `cell`.
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

/// The local zone's IANA name, which `TODAY()` and `NOW()` read the date in.
fn zone() -> &'static str {
    static ZONE: OnceLock<String> = OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned()))
}

/// What each cell of one sheet's inputs computes, the sheet on its own.
pub fn recalculate<S: BuildHasher>(raw: &HashMap<Addr, String, S>) -> HashMap<Addr, CellValue> {
    let mut calc = Calc::new();
    calc.load([(LONE_SHEET, raw)].into_iter());
    calc.values(0)
}

/// What each cell of `snap` computes: with its workbook when a formula reads other sheets.
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

/// Why a formula does not parse: the error its cell shows and the problem in words.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    pub error: FormulaError,
    pub problem: String,
}

/// `body` parsed as if in the first cell of a sheet with no others.
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

/// Why `raw` is a formula that does not parse; `None` for one that parses or is no formula.
pub fn parse_error(raw: &str) -> Option<ParseError> {
    let body = raw.strip_prefix('=')?;
    // IronCalc's parser stops at the first whole expression and ignores the rest, where a
    // spreadsheet refuses `=1 2`, which fails once parenthesised.
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

/// Whether `body` has a `)` with no `(` before it, outside quotes and quoted sheet names.
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

/// Why a cell holding `raw` evaluates to `err`, naming the likely mistake.
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

/// The unit a sub-expression carries while propagating formats: the category, and the currency
/// when it is money.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    /// A bare scalar; a scalar times a currency is that currency.
    Scalar,
    /// Money in a specific currency.
    Currency(tilo_locale::Currency),
    /// A percentage value.
    Percent,
    Date(tilo_locale::DateStyle),
    /// A time or duration serial.
    Time(ui::clock::TimeStyle),
    /// Conflicting units such as USD + EUR, which render plain.
    Mixed,
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

/// The format each cell shows in: its own, else the one a formula takes from its operands.
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

/// The format a computed cell takes from the cells its formula references; `None` when
/// nothing useful can be inferred. Money times a scalar stays money, money with money of the
/// same currency stays, different currencies and anything ambiguous render plain, and an
/// aggregate over a uniformly formatted range inherits it.
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

fn unit_of_format(fmt: CellFormat) -> Unit {
    match fmt {
        CellFormat::Currency(c) => Unit::Currency(c),
        CellFormat::Percent => Unit::Percent,
        CellFormat::Date(s) => Unit::Date(s),
        CellFormat::Time(s) => Unit::Time(s),
        // General, Number, Boolean and Text all behave as scalars.
        _ => Unit::Scalar,
    }
}

/// The cell a parsed reference names, unless anchored.
fn referenced(row: i32, column: i32, absolute_row: bool, absolute_column: bool) -> Option<Addr> {
    let at = |n: i32, absolute: bool| {
        let one_based = if absolute { n } else { n.checked_add(1)? };
        u32::try_from(one_based.checked_sub(1)?).ok()
    };
    Some((at(column, absolute_column)?, at(row, absolute_row)?))
}

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

fn combine(op: Op, l: Unit, r: Unit) -> Unit {
    match op {
        // Two equal units divided are a ratio, so unitless.
        Op::Div if l == r && l != Unit::Scalar => Unit::Scalar,
        // A scalar keeps the unit; any other pair of units is ambiguous.
        Op::Mul | Op::Div | Op::Pow => match (l, r) {
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
        // The same unit stays; date minus date becomes a duration; a unit with a scalar keeps it.
        Op::Add | Op::Sub => match (l, r) {
            // Date minus date is a span of days.
            (Unit::Date(_), Unit::Date(_)) if op == Op::Sub => Unit::Scalar,
            (a, b) if a == b => a,
            (Unit::Scalar, u) | (u, Unit::Scalar) => u,
            _ => Unit::Mixed,
        },
    }
}

/// A call's unit: an aggregate inherits its range's unit, the rest their first argument.
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
        // Date builders render as dates, so a bare `=TODAY()` shows a date, not a serial.
        "DATE" | "TODAY" | "NOW" => Unit::Date(tilo_locale::DateStyle::Iso),
        "TIME" => Unit::Time(ui::clock::TimeStyle::H24),
        // Counting, text, logical and lookup functions are unitless.
        _ => Unit::Scalar,
    }
}

/// The units one argument contributes: a range contributes each formatted cell's unit.
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
