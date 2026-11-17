//! A cell's input, and what the references of a formula are bound to.
//!
//! A formula's text names cells by position (`Data!A3`), and positions move: a row inserted
//! above makes row 3 another row. So a formula is stored with what its writer meant: beside the
//! text, for each reference, the id of the sheet it names and the ids of its rows and columns.
//! Whoever reads a cell gets the text with every reference spelled where its lines are now
//! ([`Input::shown`]), in ordinary notation; the stored text stays as it was typed. Nothing has
//! to rewrite a formula when rows, columns or sheets change, so a change made by someone who has
//! not seen the formula, or the formula written by someone who has not seen the change, still
//! ends on the cells its writer pointed at, the same on every replica.
//!
//! Text and binding are one stored value, written whole: two writers of one cell never end with
//! one's text and the other's binding. A formula stored without a binding, as every formula was
//! before bindings, reads by position.

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Mutex;

use ui::lock::MutexRecover as _;
use ui::tilo_resource::{self, Atom, Id};

use crate::refs::{Moved, Ref, fold_sheet, rewrite_refs};
use crate::workbook::{Fnv, Layout, Workbook};

/// Between a formula's text and its binding in the stored value: the unit separator, which no
/// keyboard types and [`Input::typed`] takes out of what is pasted.
const BOUND: char = '\u{1f}';

/// In a binding, a reference bound to nothing: it reads by position.
const UNBOUND: &str = "?";

/// What a cell holds as typed: a value, or a formula with what its references meant.
///
/// The binding lists the other sheets the formula reads, by id, then one entry per reference in
/// the order the text holds them: `sheets|entry;entry`. An entry is the reference's lines, by
/// id: a cell's row and column, a range's two corners, the two lines of whole rows or columns;
/// one that reads another sheet starts with that sheet's place in the list and `!`. A line
/// whose id is the one its sheet derives for the place the text names (`r2` for row 3) is left
/// empty, and an entry with nothing in it is left out: a formula in a sheet nobody inserted
/// into has a binding of nothing but the `;` between its references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    binding: Option<Box<str>>,
    spelled: Spelled,
}

/// What a formula last read as, and the stamp of the lines it was read against. Spelling a
/// formula anew is a pass over its text, and between two changes of rows, columns or sheets it
/// reads the same, so what it read is kept. It is no part of the input: two inputs are the same
/// whatever each last read as, and a copy starts with nothing kept.
#[derive(Default)]
struct Spelled(Mutex<Option<Box<Kept>>>);

struct Kept {
    stamp: u64,
    /// `None` for a formula that read as it is typed.
    spelling: Option<Box<str>>,
}

/// What [`Spelled`] holds for a stamp.
enum Read {
    /// Nothing: the formula has to be spelled.
    Unknown,
    /// As it is typed.
    Typed,
    Spelled(String),
}

impl Spelled {
    fn read(&self, stamp: u64) -> Read {
        match self.0.lock_recover().as_deref() {
            Some(kept) if kept.stamp == stamp => kept
                .spelling
                .as_deref()
                .map_or(Read::Typed, |spelling| Read::Spelled(spelling.to_owned())),
            _ => Read::Unknown,
        }
    }

    fn keep(&self, stamp: u64, spelling: Option<&str>) {
        *self.0.lock_recover() = Some(Box::new(Kept {
            stamp,
            spelling: spelling.map(Into::into),
        }));
    }
}

impl Clone for Spelled {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Spelled {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Spelled {}

impl std::fmt::Debug for Spelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Spelled")
    }
}

impl Input {
    /// What someone typed, bound to nothing.
    pub fn typed(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.contains(BOUND) {
            text.retain(|c| c != BOUND);
        }
        Self {
            text,
            binding: None,
            spelled: Spelled::default(),
        }
    }

    /// The formula `text` of a sheet laid out as `layout`, bound to the lines its references
    /// name there and on the sheets `lines` knows; any other text as typed. A reference that
    /// names no sheet of the workbook, or a place outside the grid, stays bound to nothing.
    pub fn bound(text: impl Into<String>, layout: &Layout, lines: &Lines<'_>) -> Self {
        let typed = Self::typed(text);
        if !typed.is_formula() {
            return typed;
        }
        let mut sheets: Vec<&Id> = Vec::new();
        let mut entries: Vec<String> = Vec::new();
        drop(rewrite_refs(&typed.text, |sheet, reference| {
            let entry = entry_for(sheet, reference, layout, lines, &mut sheets);
            entries.push(entry.unwrap_or_else(|| UNBOUND.to_owned()));
            Moved::Kept
        }));
        let sheets: Vec<&str> = sheets.iter().map(|id| id.as_str()).collect();
        Self {
            text: typed.text,
            binding: Some(joined(&sheets, &entries)),
            spelled: Spelled::default(),
        }
    }

    /// The text as it was typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_formula(&self) -> bool {
        self.text.starts_with('=')
    }

    /// Whether it is a formula whose references are bound.
    pub fn is_bound(&self) -> bool {
        self.binding.is_some()
    }

    /// The input as a person reads it and the engine computes it, in a sheet laid out as
    /// `layout`: a formula with every reference spelled where its lines are now, `#REF!` where
    /// a line or a sheet is gone, and a sheet by the name it has now.
    pub fn shown<'s>(&'s self, layout: &Layout, lines: &Lines<'_>) -> Cow<'s, str> {
        let Some(binding) = self.binding.as_deref() else {
            return Cow::Borrowed(&self.text);
        };
        // Every reference is to its own sheet's derived lines, and the sheet holds exactly
        // those: each is where the text says.
        if layout.is_plain() && binding.bytes().all(|byte| byte == b';') {
            return Cow::Borrowed(&self.text);
        }
        // A formula of its own sheet alone reads the same until that sheet's lines change; one
        // that reads others, until any sheet's lines or name do.
        let stamp = if binding.contains('|') {
            lines.stamp()
        } else {
            layout.stamp()
        };
        match self.spelled.read(stamp) {
            Read::Typed => return Cow::Borrowed(&self.text),
            Read::Spelled(spelling) => return Cow::Owned(spelling),
            Read::Unknown => {}
        }
        let spelling = respelled(&self.text, binding, layout, lines);
        self.spelled.keep(stamp, spelling.as_deref());
        spelling.map_or(Cow::Borrowed(&self.text), Cow::Owned)
    }

    /// The input once row or column `at` of sheet `sheet` is deleted, when a range of it begins
    /// or ends on that line and reaches further: that end moves to the line beside it, as a
    /// range shrinks when its first or last line goes. `None` when it stays as it is: a
    /// reference to the line alone needs nothing written to read `#REF!`. `own` is the sheet
    /// the formula is on, laid out as `layout`.
    pub fn without_line(
        &self,
        (own, layout): (&Id, &Layout),
        lines: &Lines<'_>,
        sheet: &Id,
        rows: bool,
        at: u32,
    ) -> Option<Self> {
        let binding = self.binding.as_deref()?;
        let (sheets, entries) = parts(binding);
        let mut kept: Vec<String> = entries.iter().map(|entry| (*entry).to_owned()).collect();
        let (mut index, mut changed) = (0usize, false);
        drop(rewrite_refs(&self.text, |_, reference| {
            let here = index;
            index += 1;
            let Some((on, ids)) = entries.get(here).and_then(|entry| entry_parts(entry)) else {
                return Moved::Kept;
            };
            let read = on.map_or(Some(own.as_str()), |on| sheets.get(on).copied());
            if read != Some(sheet.as_str()) {
                return Moved::Kept;
            }
            let held;
            let there = if on.is_some() {
                let Some(found) = lines.layout(sheet) else {
                    return Moved::Kept;
                };
                held = found;
                &*held
            } else {
                layout
            };
            // The two ends along the deleted line's axis: where each is in the entry, and the
            // place the text names.
            let (first, second, arity) = match reference {
                Ref::Range(a, _, b) if rows => ((0, a.addr.1), (2, b.addr.1), 4),
                Ref::Range(a, _, b) => ((1, a.addr.0), (3, b.addr.0), 4),
                Ref::Lines(a, _, b) if a.rows == rows => ((0, a.at), (1, b.at), 2),
                _ => return Moved::Kept,
            };
            let place = |(nth, written): (usize, u32)| {
                let code = ids.get(nth).copied().unwrap_or_default();
                if rows {
                    there.row_of(code, written)
                } else {
                    there.column_of(code, written)
                }
            };
            let (Some(p), Some(q)) = (place(first), place(second)) else {
                return Moved::Kept;
            };
            if p == q || (p != at && q != at) {
                return Moved::Kept;
            }
            let ((nth, written), inward) = if p == at {
                (first, if q > p { at + 1 } else { at - 1 })
            } else {
                (second, if p > q { at + 1 } else { at - 1 })
            };
            let code = if rows {
                there.row_code(inward, written)
            } else {
                there.column_code(inward, written)
            };
            let Some(code) = code else {
                return Moved::Kept;
            };
            // An entry leaves out what it has nothing to say of: a line each for the others.
            let mut ids: Vec<&str> = ids.iter().copied().take(arity).collect();
            ids.resize(arity, "");
            if let Some(held) = ids.get_mut(nth) {
                *held = &code;
            }
            if let Some(entry) = kept.get_mut(here) {
                *entry = entry_of(on, &ids);
                changed = true;
            }
            Moved::Kept
        }));
        changed.then(|| Self {
            text: self.text.clone(),
            binding: Some(joined(&sheets, &kept)),
            spelled: Spelled::default(),
        })
    }

    /// Whether a reference of the formula is bound to one of the rows (or columns) `gone` of
    /// sheet `sheet`, by id. `own` is the sheet the formula is on.
    pub fn reads_line(&self, own: &Id, sheet: &Id, rows: bool, gone: &[String]) -> bool {
        let Some(binding) = self.binding.as_deref() else {
            return false;
        };
        let (sheets, entries) = parts(binding);
        let (mut index, mut reads) = (0usize, false);
        drop(rewrite_refs(&self.text, |_, reference| {
            let entry = entries.get(index).and_then(|entry| entry_parts(entry));
            index += 1;
            let Some((on, ids)) = entry else {
                return Moved::Kept;
            };
            let read = on.map_or(Some(own.as_str()), |on| sheets.get(on).copied());
            if read != Some(sheet.as_str()) {
                return Moved::Kept;
            }
            let ends: &[(usize, u32)] = match reference {
                Ref::Cell(cell) if rows => &[(0, cell.addr.1)],
                Ref::Cell(cell) => &[(1, cell.addr.0)],
                Ref::Range(a, _, b) if rows => &[(0, a.addr.1), (2, b.addr.1)],
                Ref::Range(a, _, b) => &[(1, a.addr.0), (3, b.addr.0)],
                Ref::Lines(a, _, b) if a.rows == rows => &[(0, a.at), (1, b.at)],
                Ref::Lines(..) => &[],
            };
            reads |= ends.iter().any(|(nth, written)| {
                let code = ids.get(*nth).copied().unwrap_or_default();
                if code.is_empty() {
                    let derived =
                        crate::workbook::derived_id(if rows { 'r' } else { 'c' }, *written);
                    gone.contains(&derived)
                } else {
                    gone.iter().any(|id| id == code)
                }
            });
            Moved::Kept
        }));
        reads
    }

    /// The value as stored: the text, and a formula's binding after [`BOUND`].
    fn stored(&self) -> Cow<'_, str> {
        match &self.binding {
            Some(binding) => Cow::Owned(format!("{}{BOUND}{binding}", self.text)),
            None => Cow::Borrowed(&self.text),
        }
    }

    fn from_stored(stored: String) -> Self {
        match stored.split_once(BOUND) {
            Some((text, binding)) if text.starts_with('=') => Self {
                text: text.to_owned(),
                binding: Some(binding.into()),
                spelled: Spelled::default(),
            },
            Some(_) => Self::typed(stored),
            None => Self {
                text: stored,
                binding: None,
                spelled: Spelled::default(),
            },
        }
    }
}

/// The binding's entry for one reference: its lines as `layout` (its own sheet's) or the sheet
/// it names holds them now. `None` when it names no sheet or no place of the grid.
fn entry_for<'a>(
    sheet: Option<&str>,
    reference: &Ref,
    layout: &Layout,
    lines: &Lines<'a>,
    sheets: &mut Vec<&'a Id>,
) -> Option<String> {
    let held;
    let (on, there) = match sheet {
        None => (None, layout),
        Some(written) => {
            let id = lines.named(written)?;
            held = lines.layout(id)?;
            let on = sheets
                .iter()
                .position(|known| *known == id)
                .unwrap_or_else(|| {
                    sheets.push(id);
                    sheets.len() - 1
                });
            (Some(on), &*held)
        }
    };
    let ids = match reference {
        Ref::Cell(cell) => vec![
            there.row_code(cell.addr.1, cell.addr.1)?,
            there.column_code(cell.addr.0, cell.addr.0)?,
        ],
        Ref::Range(a, _, b) => vec![
            there.row_code(a.addr.1, a.addr.1)?,
            there.column_code(a.addr.0, a.addr.0)?,
            there.row_code(b.addr.1, b.addr.1)?,
            there.column_code(b.addr.0, b.addr.0)?,
        ],
        Ref::Lines(a, _, b) if a.rows => {
            vec![there.row_code(a.at, a.at)?, there.row_code(b.at, b.at)?]
        }
        Ref::Lines(a, _, b) => vec![
            there.column_code(a.at, a.at)?,
            there.column_code(b.at, b.at)?,
        ],
    };
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    Some(entry_of(on, &ids))
}

/// An entry as stored: the sheet's place in the list when it reads another, then its lines.
fn entry_of(on: Option<usize>, ids: &[&str]) -> String {
    match on {
        None if ids.iter().all(|id| id.is_empty()) => String::new(),
        None => ids.join(","),
        Some(on) => format!("{on}!{}", ids.join(",")),
    }
}

/// A binding as stored, of the sheets it names and its entries.
fn joined<S: AsRef<str>>(sheets: &[&str], entries: &[S]) -> Box<str> {
    let mut binding = String::new();
    if !sheets.is_empty() {
        binding.push_str(&sheets.join(","));
        binding.push('|');
    }
    for (at, entry) in entries.iter().enumerate() {
        if at > 0 {
            binding.push(';');
        }
        binding.push_str(entry.as_ref());
    }
    binding.into_boxed_str()
}

/// A binding's sheets and entries.
fn parts(binding: &str) -> (Vec<&str>, Vec<&str>) {
    let (sheets, entries) = binding.split_once('|').unwrap_or(("", binding));
    let sheets = if sheets.is_empty() {
        Vec::new()
    } else {
        sheets.split(',').collect()
    };
    (sheets, entries.split(';').collect())
}

/// An entry's sheet, by its place in the list, and its lines; `None` for a reference bound to
/// nothing and for an entry that does not read.
fn entry_parts(entry: &str) -> Option<(Option<usize>, Vec<&str>)> {
    if entry == UNBOUND {
        return None;
    }
    let (on, ids) = match entry.split_once('!') {
        Some((on, ids)) => (Some(on.parse::<usize>().ok()?), ids),
        None => (None, entry),
    };
    Some((on, ids.split(',').collect()))
}

/// The reference as spelled where its lines are in `there` now, and whether that is elsewhere
/// than written; `None` when one of its lines is gone.
fn spelled(reference: &Ref, ids: &[&str], there: &Layout) -> Option<(String, bool)> {
    let code = |nth: usize| ids.get(nth).copied().unwrap_or_default();
    Some(match reference {
        Ref::Cell(cell) => {
            let now = (
                there.column_of(code(1), cell.addr.0)?,
                there.row_of(code(0), cell.addr.1)?,
            );
            (cell.render(now), now != cell.addr)
        }
        Ref::Range(a, sep, b) => {
            let first = (
                there.column_of(code(1), a.addr.0)?,
                there.row_of(code(0), a.addr.1)?,
            );
            let second = (
                there.column_of(code(3), b.addr.0)?,
                there.row_of(code(2), b.addr.1)?,
            );
            (
                format!("{}{sep}{}", a.render(first), b.render(second)),
                first != a.addr || second != b.addr,
            )
        }
        Ref::Lines(a, sep, b) => {
            let place = |nth: usize, written: u32| {
                if a.rows {
                    there.row_of(code(nth), written)
                } else {
                    there.column_of(code(nth), written)
                }
            };
            let (first, second) = (place(0, a.at)?, place(1, b.at)?);
            (
                format!("{}{sep}{}", a.render(first), b.render(second)),
                first != a.at || second != b.at,
            )
        }
    })
}

/// The formula `text` with each reference spelled where `binding` finds its lines now, in a
/// sheet laid out as `layout`. `None` when every reference is where the text says, and when
/// text and binding do not belong together.
fn respelled(text: &str, binding: &str, layout: &Layout, lines: &Lines<'_>) -> Option<String> {
    let (sheets, entries) = parts(binding);
    let (mut index, mut sound, mut moved) = (0usize, true, false);
    let spelling = rewrite_refs(text, |written, reference| {
        let Some(entry) = entries.get(index) else {
            sound = false;
            return Moved::Kept;
        };
        index += 1;
        if *entry == UNBOUND {
            return Moved::Kept;
        }
        let Some((on, ids)) = entry_parts(entry) else {
            sound = false;
            return Moved::Kept;
        };
        let how = match (on, written) {
            (None, None) => match spelled(reference, &ids, layout) {
                Some((_, false)) => Moved::Kept,
                Some((spelling, true)) => Moved::To(spelling),
                None => Moved::Gone,
            },
            (Some(on), Some(written)) => {
                let Some(id) = sheets.get(on).map(|id| Id::from_raw(*id)) else {
                    sound = false;
                    return Moved::Kept;
                };
                let found = lines
                    .layout(&id)
                    .and_then(|there| spelled(reference, &ids, &there));
                match found {
                    None => Moved::Gone,
                    // The name as written still reaches the sheet the reference means.
                    Some((spelling, moved)) if lines.named(written) == Some(&id) => {
                        if moved {
                            Moved::To(spelling)
                        } else {
                            Moved::Kept
                        }
                    }
                    Some((spelling, _)) => match lines.name_of(&id) {
                        Some(name) => Moved::Whole(format!(
                            "{}!{spelling}",
                            ironcalc_base::expressions::utils::quote_name(name)
                        )),
                        None => Moved::Gone,
                    },
                }
            }
            _ => {
                sound = false;
                return Moved::Kept;
            }
        };
        moved |= !matches!(how, Moved::Kept);
        how
    });
    // A formula with no reference has one entry of nothing.
    let whole = index == entries.len() || (index == 0 && binding.is_empty());
    (sound && whole && moved).then_some(spelling)
}

/// The workbook's sheets as a formula's references find them: each one's name, and where its
/// rows and columns are, laid out when first asked for.
pub struct Lines<'a> {
    book: &'a Workbook,
    /// Each sheet's id and name, and the name as references match it.
    names: Vec<(&'a Id, &'a str, String)>,
    layouts: RefCell<Vec<(Id, Rc<Layout>)>>,
    stamp: OnceCell<u64>,
}

impl<'a> Lines<'a> {
    pub fn of(book: &'a Workbook) -> Self {
        Self {
            book,
            names: book
                .sheets
                .iter()
                .map(|(id, sheet)| (id, sheet.name.0.as_str(), fold_sheet(&sheet.name.0)))
                .collect(),
            layouts: RefCell::new(Vec::new()),
            stamp: OnceCell::new(),
        }
    }

    /// What every sheet is called and where its rows and columns are, as one number: a
    /// formula reads the same in two workbooks of one stamp.
    fn stamp(&self) -> u64 {
        *self.stamp.get_or_init(|| {
            let mut stamp = Fnv::new();
            for (id, name, _) in &self.names {
                stamp.text(id.as_str());
                stamp.text(name);
                stamp.number(self.layout(id).map_or(0, |layout| layout.stamp()));
            }
            stamp.finish()
        })
    }

    /// [`Self::of`], with sheet `id` laid out as `layout`: the layout its caller already made,
    /// or one that reaches further than the sheet does yet.
    pub fn with(book: &'a Workbook, id: &Id, layout: Rc<Layout>) -> Self {
        let lines = Self::of(book);
        lines.layouts.borrow_mut().push((id.clone(), layout));
        lines
    }

    /// Where the rows and columns of sheet `id` are; `None` when the workbook has no such
    /// sheet.
    pub fn layout(&self, id: &Id) -> Option<Rc<Layout>> {
        if let Some((_, held)) = self.layouts.borrow().iter().find(|(held, _)| held == id) {
            return Some(Rc::clone(held));
        }
        let layout = Rc::new(self.book.sheet(id)?.layout());
        self.layouts
            .borrow_mut()
            .push((id.clone(), Rc::clone(&layout)));
        Some(layout)
    }

    /// The sheet a reference naming `written` reaches: the first of that name.
    pub fn named(&self, written: &str) -> Option<&'a Id> {
        let name = fold_sheet(written);
        self.names
            .iter()
            .find(|(_, _, folded)| *folded == name)
            .map(|(id, _, _)| *id)
    }

    /// The name formulas reach sheet `id` by; `None` when it is gone, and when an earlier
    /// sheet of the same name is the one that name reaches.
    pub fn name_of(&self, id: &Id) -> Option<&'a str> {
        let (_, name, folded) = self.names.iter().find(|(held, _, _)| *held == id)?;
        let (first, _, _) = self.names.iter().find(|(_, _, other)| other == folded)?;
        (*first == id).then_some(*name)
    }
}

/// An input is stored as one scalar, written whole.
impl tilo_resource::Settable for Input {}
impl tilo_resource::Model for Input {
    const SCALAR: bool = true;
    tilo_resource::leaf_paths!();
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "string"})
    }
    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        <Atom as tilo_resource::Model>::field_type(types)
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.stored().into_owned())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, tilo_resource::ModelError> {
        match value {
            serde_json::Value::String(stored) => Ok(Self::from_stored(stored.clone())),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(tilo_resource::ModelError::shape("a cell's input")),
        }
    }
    fn load(
        doc: &tilo_resource::Doc,
        value: &automerge::Value<'_>,
        id: &automerge::ObjId,
    ) -> Result<Self, tilo_resource::ModelError> {
        let stored = <Atom as tilo_resource::Model>::load(doc, value, id)?;
        Ok(Self::from_stored(stored.0))
    }
    fn missing() -> Result<Self, tilo_resource::ModelError> {
        Ok(Self::default())
    }
    fn store(
        &self,
        doc: &mut tilo_resource::Doc,
        at: tilo_resource::Slot<'_>,
    ) -> Result<(), tilo_resource::ModelError> {
        at.put_scalar(
            doc,
            automerge::ScalarValue::Str(self.stored().as_ref().into()),
        )
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}
