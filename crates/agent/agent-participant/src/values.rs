//! Values a step's arguments take from the system rather than from the
//! model's head.
//!
//! A cheaper model asked for a total, an amount less an excess or a
//! deadline ten business days out works it out itself, and gets it wrong
//! often enough to matter; asked to copy a figure it misreads the row. So
//! any argument a plan writes may instead say where the value is and how
//! it is computed, and the host fills it in just before the step runs:
//!
//! - `{{<app>:<record>/<read>#/json/pointer}}` quotes what any read or view
//!   the agent may run answers, at that pointer, through the same reads a
//!   planning model's `read_view` runs. Without `:<record>` the host picks
//!   the record as it does for a step that names none.
//! - `{{Sheet1!B2}}`, `{{'Q3 Budget'!C7}}` and ranges (`{{Sheet1!B2:B9}}`)
//!   are that, for a workbook's `read` of the cells: the one alias the
//!   host knows.
//! - `{{= expr }}` computes: arithmetic, sum, avg, min, max, round, abs,
//!   percentages and dates, with references (`Sheet1!B2`, `{ref}`) as
//!   operands.
//! - `[rule]` after a pointer that reaches a list picks one of its items
//!   by what it holds rather than where it is (`#/messages[from~Ada,
//!   newest]/body`, `{{Sheet1!A1:H9[Invoice=INV-7]/Status}}`); the
//!   rules are in [`select`].
//! - `|$`, `|%`, `|0.00`, `|long` after either format the value, and
//!   `|match:<regex>` takes out of it the regex's first group, or its whole
//!   match (`|match:INV-\d+`).
//!
//! An argument that is one such value whole takes it typed (a number stays
//! a number); inside text it is written out. Anything else in braces is
//! left as it is. A reference that names nothing, or a value that is
//! missing or cannot be computed, fails the step with the reference named,
//! which the task's repair then plans around.
//!
//! Two helps need no syntax at all. An argument whose parameter takes a
//! date or a number may be arithmetic in words (`10 business days after
//! 2026-10-14`, `289 - 50`), which the host computes ([`plain`]); and
//! the dates and amounts a read's answer writes are noted beside them as
//! the host reads them (`03/04/1988 [1988-04-03]`, [`annotate`]), so the
//! model copies ISO rather than guessing which way a date goes.

mod annotate;
mod dates;
mod expr;
mod pattern;
mod plain;
mod select;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod typed_tests;

use std::collections::BTreeMap;

pub(crate) use annotate::{annotate_json, annotate_text, unannotated};
pub(crate) use expr::Value;
pub(crate) use plain::fill as fill_plain;

/// A read a reference names: of `app` (its id, or the last part of it), on
/// `record` or the one the host picks, the operation `operation` (an
/// action of the record's own resource, or `resource#action`) with
/// `arguments`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ViewRef {
    pub app: String,
    pub record: Option<String>,
    pub operation: String,
    pub arguments: BTreeMap<String, String>,
}

/// What a reference names: a read, and where in its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reference {
    view: ViewRef,
    pointer: String,
    /// The items picked from there on, in order.
    selections: Vec<Selection>,
    /// A range of cells, whose rows are read as one list.
    range: bool,
}

/// A rule picking from the list a reference has reached ([`select`]), and
/// the pointer into what it picked.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    rule: String,
    pointer: String,
}

/// The app id a cell reference (`Sheet1!B2`) reads, as [`ViewRef::app`]
/// names apps.
pub(crate) const CELLS_APP: &str = "sheets";

/// Runs the reads references name.
pub(crate) trait Reads {
    /// What `view` answers, whole.
    ///
    /// # Errors
    ///
    /// Why it was not read: no such app or record, an operation that is
    /// not a read the agent may run, or the record's refusal.
    fn read(&mut self, view: &ViewRef) -> Result<serde_json::Value, String>;
}

/// Fill in every value `arguments` quote or compute, reading through
/// `reads`. `params` are the operation's parameters: an argument whose
/// parameter takes text gets text, one that takes a number a number.
///
/// # Errors
///
/// The first reference or expression that cannot be filled in, named.
pub(crate) fn bind(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    params: &[tilo_actions::ActionParam],
    reads: &mut dyn Reads,
) -> Result<(), String> {
    let mut binder = Binder {
        reads,
        answers: BTreeMap::new(),
    };
    for (name, value) in arguments.iter_mut() {
        let wants = params
            .iter()
            .find(|param| param.name.as_ref() == name.as_str())
            .map(|param| param.ty);
        binder.bind_value(value, wants)?;
    }
    Ok(())
}

/// Whether `text` holds anything [`bind`] fills in.
pub(crate) fn binds(text: &str) -> bool {
    bindings(text).iter().any(|found| found.binding.is_some())
}

struct Binder<'a> {
    reads: &'a mut dyn Reads,
    /// What each read answered, so a step quoting ten cells of one range
    /// reads each once.
    answers: BTreeMap<ViewRef, serde_json::Value>,
}

/// What a `{{…}}` stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Expression(String),
    Reference(Reference),
}

/// One `{{…}}` of a text: where it is, what is inside, what it binds to
/// (`None`: left as written) and its format.
struct Found {
    start: usize,
    end: usize,
    inner: String,
    binding: Option<Binding>,
    format: Option<String>,
}

impl Binder<'_> {
    fn bind_value(
        &mut self,
        value: &mut serde_json::Value,
        wants: Option<tilo_actions::ParamType>,
    ) -> Result<(), String> {
        match value {
            serde_json::Value::String(text) => {
                if let Some(bound) = self.bind_text(text, wants)? {
                    *value = bound;
                }
                Ok(())
            }
            serde_json::Value::Array(items) => items
                .iter_mut()
                .try_for_each(|item| self.bind_value(item, None)),
            serde_json::Value::Object(fields) => fields
                .values_mut()
                .try_for_each(|field| self.bind_value(field, None)),
            _ => Ok(()),
        }
    }

    /// `text` with its values filled in, `None` when it has none.
    fn bind_text(
        &mut self,
        text: &str,
        wants: Option<tilo_actions::ParamType>,
    ) -> Result<Option<serde_json::Value>, String> {
        let found = bindings(text);
        if !found.iter().any(|found| found.binding.is_some()) {
            return Ok(None);
        }
        let leading = text.len().saturating_sub(text.trim_start().len());
        let trailing = text.trim_end().len();
        if let [one] = found.as_slice()
            && one.start == leading
            && one.end == trailing
            && let Some(binding) = &one.binding
        {
            let named = |why: String| format!("{{{{{}}}}}: {why}", one.inner);
            let value = self.typed(binding, one.format.as_deref()).map_err(named)?;
            return Ok(Some(fit(value, wants)));
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for found in &found {
            out.push_str(text.get(at..found.start).unwrap_or_default());
            match &found.binding {
                Some(binding) => {
                    let value = self
                        .evaluate(binding)
                        .and_then(|value| format(&value, found.format.as_deref()))
                        .map_err(|why| format!("{{{{{}}}}}: {why}", found.inner))?;
                    out.push_str(&value);
                }
                None => out.push_str(text.get(found.start..found.end).unwrap_or_default()),
            }
            at = found.end;
        }
        out.push_str(text.get(at..).unwrap_or_default());
        Ok(Some(serde_json::Value::String(out)))
    }

    /// The value of a binding that is a whole argument: formatted text
    /// when it names a format, else typed.
    fn typed(
        &mut self,
        binding: &Binding,
        format_as: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if format_as.is_some() {
            let value = self.evaluate(binding)?;
            return format(&value, format_as).map(serde_json::Value::String);
        }
        match binding {
            Binding::Expression(_) => Ok(self.evaluate(binding)?.to_json()),
            Binding::Reference(reference) => {
                let raw = self.quoted(reference)?;
                Ok(match raw {
                    serde_json::Value::Number(_) | serde_json::Value::Array(_) => {
                        Value::from_json(&raw)?.to_json()
                    }
                    other => other,
                })
            }
        }
    }

    fn evaluate(&mut self, binding: &Binding) -> Result<Value, String> {
        match binding {
            Binding::Reference(reference) => {
                let raw = self.quoted(reference)?;
                Value::from_json(&raw)
            }
            Binding::Expression(source) => expr::evaluate(source, &mut |text| {
                let reference = parse_reference(text).ok_or_else(|| {
                    format!(
                        "`{text}` is not a reference: name a cell as Sheet1!B2, or a read as \
                         {{app:record/read#/pointer}}"
                    )
                })?;
                let raw = self
                    .quoted(&reference)
                    .map_err(|why| format!("{text}: {why}"))?;
                Value::from_json(&raw)
            })
            // Arithmetic said in words (`10 business days after
            // 2026-10-14`) computes too; the expression's own error is the
            // one shown when neither reads it.
            .or_else(|why| plain::evaluate(source).map_err(|_| why)),
        }
    }

    /// What `reference` points at, present and not empty.
    fn quoted(&mut self, reference: &Reference) -> Result<serde_json::Value, String> {
        let answer = if let Some(answer) = self.answers.get(&reference.view) {
            answer.clone()
        } else {
            let answer = self.reads.read(&reference.view)?;
            self.answers.insert(reference.view.clone(), answer.clone());
            answer
        };
        let found = if reference.pointer.is_empty() {
            Some(&answer)
        } else {
            answer.pointer(&reference.pointer)
        };
        let Some(found) = found else {
            return Err(format!(
                "{} answers nothing at {}",
                reference.view.operation, reference.pointer
            ));
        };
        let mut found = found.clone();
        let mut many = reference.range;
        for selection in &reference.selections {
            let (picked, all) = match select::select(&found, &selection.rule)? {
                select::Picked::One(item) => (item, false),
                select::Picked::All(items) => (serde_json::Value::Array(items), true),
            };
            many = all;
            found = if selection.pointer.is_empty() {
                picked
            } else if let serde_json::Value::Array(items) = &picked
                && all
            {
                serde_json::Value::Array(
                    items
                        .iter()
                        .map(|item| {
                            select::at(item, &selection.pointer)
                                .cloned()
                                .ok_or_else(|| {
                                    nothing_at(item, &selection.rule, &selection.pointer)
                                })
                        })
                        .collect::<Result<_, _>>()?,
                )
            } else {
                select::at(&picked, &selection.pointer)
                    .cloned()
                    .ok_or_else(|| nothing_at(&picked, &selection.rule, &selection.pointer))?
            };
        }
        let empty = match &found {
            serde_json::Value::Null => true,
            serde_json::Value::String(text) => text.trim().is_empty(),
            _ => false,
        };
        if empty && !many {
            return Err("it is empty".into());
        }
        Ok(found)
    }
}

/// Why `pointer` reaches nothing in the item `rule` picked.
fn nothing_at(item: &serde_json::Value, rule: &str, pointer: &str) -> String {
    let fields = item.as_object().map_or_else(String::new, |fields| {
        let names: Vec<&str> = fields.keys().map(String::as_str).collect();
        format!(" (it has {})", names.join(", "))
    });
    format!("the item [{rule}] picked has nothing at {pointer}{fields}")
}

/// `value` as a parameter of type `wants` takes it: written out for text,
/// read as a number for a number.
fn fit(value: serde_json::Value, wants: Option<tilo_actions::ParamType>) -> serde_json::Value {
    use tilo_actions::ParamType;
    match (wants, &value) {
        (
            Some(ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret),
            serde_json::Value::Number(_) | serde_json::Value::Bool(_),
        ) => {
            Value::from_json(&value).map_or(value, |typed| serde_json::Value::String(typed.text()))
        }
        (Some(ParamType::Number | ParamType::Integer), serde_json::Value::String(text)) => {
            expr::lenient_number(text).map_or(value, |number| Value::Number(number).to_json())
        }
        _ => value,
    }
}

/// Every `{{…}}` in `text`, in order: braces inside count, so
/// `{{= {a#/x} * 2}}` closes at its last `}}`.
fn bindings(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = text.get(from..).and_then(|rest| rest.find("{{")) {
        let start = from.saturating_add(open);
        let inner_start = start.saturating_add(2);
        let Some(inner_end) = closing(text, inner_start) else {
            break;
        };
        let inner = text.get(inner_start..inner_end).unwrap_or_default();
        let end = inner_end.saturating_add(2);
        let (body, format) = split_format(inner);
        let body = body.trim();
        let binding = match body.strip_prefix('=') {
            Some(source) => Some(Binding::Expression(source.trim().to_owned())),
            None => parse_reference(body).map(Binding::Reference),
        };
        found.push(Found {
            start,
            end,
            inner: inner.trim().to_owned(),
            binding,
            format: format.map(str::to_owned),
        });
        from = end;
    }
    found
}

/// Where the `}}` closing a `{{` whose inside starts at `from` is.
fn closing(text: &str, from: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let bytes = text.as_bytes();
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' if depth > 0 => depth = depth.saturating_sub(1),
            b'}' if bytes.get(at.saturating_add(1)) == Some(&b'}') => return Some(at),
            _ => {}
        }
        at = at.saturating_add(1);
    }
    None
}

/// `inner` and the format it ends with (`…|$`), when it names one. A
/// pattern (`…|match:a|b`) runs to the end, `|` and all.
fn split_format(inner: &str) -> (&str, Option<&str>) {
    if let Some(at) = inner.find("|match:") {
        return (
            inner.get(..at).unwrap_or_default(),
            inner.get(at.saturating_add(1)..),
        );
    }
    let Some((body, format)) = inner.rsplit_once('|') else {
        return (inner, None);
    };
    let format = format.trim();
    if known_format(format) {
        (body, Some(format))
    } else {
        (inner, None)
    }
}

fn known_format(format: &str) -> bool {
    matches!(
        format.to_ascii_lowercase().as_str(),
        "$" | "%" | "long" | "short" | "iso" | "date" | "int" | "text"
    ) || fixed_places(format).is_some()
        || format
            .strip_prefix(['$', '%'])
            .or_else(|| format.strip_suffix('%'))
            .and_then(fixed_places)
            .is_some()
}

/// The decimals a number pattern asks for, and whether it groups thousands:
/// `0` (none), `0.00` (two), `#,##0.0` (one, grouped).
fn fixed_places(pattern: &str) -> Option<(usize, bool)> {
    let (whole, decimals) = pattern.split_once('.').unwrap_or((pattern, ""));
    let grouped = whole.contains(',');
    let whole_ok = !whole.is_empty()
        && whole.ends_with('0')
        && whole.chars().all(|c| matches!(c, '0' | '#' | ','));
    let decimals_ok = decimals.chars().all(|c| c == '0');
    (whole_ok && decimals_ok).then_some((decimals.len(), grouped))
}

/// `value` written as `format_as` asks.
fn format(value: &Value, format_as: Option<&str>) -> Result<String, String> {
    if let Some(pattern) = format_as.and_then(|format_as| format_as.strip_prefix("match:")) {
        let text = value.text();
        if text.trim().is_empty() {
            return Err("it is empty".into());
        }
        return pattern::first_match(&text, pattern.trim());
    }
    let Some(format_as) = format_as else {
        let text = value.text();
        return if text.trim().is_empty() {
            Err("it is empty".into())
        } else {
            Ok(text)
        };
    };
    let lower = format_as.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "text" => value.text(),
        "int" => grouped(value.number()?, 0, false),
        "iso" | "date" => dates::iso(value.date()?),
        "long" => dates::written(value.date()?, false),
        "short" => dates::written(value.date()?, true),
        "$" => money(value.number()?, 2),
        "%" => {
            let percent = value.number()? * 100.0;
            format!("{}%", expr::number_text((percent * 100.0).round() / 100.0))
        }
        _ => {
            if let Some(pattern) = format_as.strip_prefix('$') {
                let (places, _) = fixed_places(pattern).ok_or("an unknown format")?;
                money(value.number()?, places)
            } else if let Some(pattern) = format_as
                .strip_prefix('%')
                .or_else(|| format_as.strip_suffix('%'))
            {
                let (places, grouping) = fixed_places(pattern).ok_or("an unknown format")?;
                format!("{}%", grouped(value.number()? * 100.0, places, grouping))
            } else {
                let (places, grouping) = fixed_places(format_as).ok_or("an unknown format")?;
                grouped(value.number()?, places, grouping)
            }
        }
    })
}

/// `number` as dollars: `$1,234.50`, `-$80.00`.
fn money(number: f64, places: usize) -> String {
    let text = grouped(number.abs(), places, true);
    if number < 0.0 && text.chars().any(|c| c.is_ascii_digit() && c != '0') {
        format!("-${text}")
    } else {
        format!("${text}")
    }
}

/// `number` with `places` decimals, rounded half away from zero, its
/// thousands grouped by commas when `grouping`.
fn grouped(number: f64, places: usize, grouping: bool) -> String {
    let scale = 10_f64.powi(i32::try_from(places).unwrap_or(0));
    let rounded = ((number * scale).abs() + 1e-9).round() / scale * number.signum();
    let text = format!("{:.*}", places, rounded.abs());
    let (whole, decimals) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let whole = if grouping {
        let digits: Vec<char> = whole.chars().collect();
        let mut out = String::new();
        for (index, digit) in digits.iter().enumerate() {
            if index > 0 && digits.len().saturating_sub(index).is_multiple_of(3) {
                out.push(',');
            }
            out.push(*digit);
        }
        out
    } else {
        whole.to_owned()
    };
    let sign = if rounded < 0.0 { "-" } else { "" };
    if decimals.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{decimals}")
    }
}

/// The reference `text` (the inside of `{{…}}`, without a format) names:
/// a cell or range of a sheet, or a read and a pointer into its answer.
fn parse_reference(text: &str) -> Option<Reference> {
    let text = text.trim();
    cell_reference(text).or_else(|| view_reference(text))
}

/// `Sheet1!B2`, `'Q3 Budget'!C7`, `Totals!A1:C4`, and a range's rows
/// picked by a rule: `Totals!A1:C4[Region=North]/Amount`.
fn cell_reference(text: &str) -> Option<Reference> {
    let (cells, picks) = match text.find('[') {
        Some(at) => (text.get(..at)?, text.get(at..)?),
        None => (text, ""),
    };
    let mut reference = cells_reference(cells)?;
    if !picks.is_empty() {
        if !reference.range {
            return None;
        }
        reference.selections = selections(picks)?;
    }
    Some(reference)
}

fn cells_reference(text: &str) -> Option<Reference> {
    let (sheet, cells) = text.rsplit_once('!')?;
    let sheet = match sheet
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        Some(quoted) => quoted.replace("''", "'"),
        None if !sheet.is_empty()
            && sheet
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.') =>
        {
            sheet.to_owned()
        }
        None => return None,
    };
    let cells = cells.replace('$', "");
    let (first, last) = cells.split_once(':').unwrap_or((&cells, &cells));
    if !is_cell(first) || !is_cell(last) {
        return None;
    }
    let range = cells.contains(':');
    Some(Reference {
        view: ViewRef {
            app: CELLS_APP.to_owned(),
            record: None,
            operation: "read".to_owned(),
            arguments: BTreeMap::from([("item".to_owned(), format!("{sheet}!{cells}"))]),
        },
        pointer: if range { "/rows" } else { "/rows/0/0" }.to_owned(),
        selections: Vec::new(),
        range,
    })
}

fn is_cell(text: &str) -> bool {
    let letters = text.chars().take_while(char::is_ascii_alphabetic).count();
    let digits = text.get(letters..).unwrap_or_default();
    (1..=3).contains(&letters)
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('0')
}

/// `app:record/operation?name=value#/pointer`, the record and the
/// arguments optional; the pointer is empty for the whole answer, and
/// rules may follow it (`#/messages[newest]/body`, `#[last]`).
fn view_reference(text: &str) -> Option<Reference> {
    let app_end = text.find([':', '/'])?;
    let app = text.get(..app_end)?;
    if app.is_empty()
        || !app
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    let mut rest = text.get(app_end..)?;
    let record = match rest.strip_prefix(':') {
        Some(after) => {
            let slash = after.find('/')?;
            let record = after.get(..slash)?.trim();
            rest = after.get(slash..)?;
            (!record.is_empty()).then(|| record.to_owned())
        }
        None => None,
    };
    let rest = rest.strip_prefix('/')?;
    let (hash, _) = rest.match_indices('#').find(|(at, _)| {
        matches!(
            rest.get(at.saturating_add(1)..)
                .and_then(|after| after.chars().next()),
            None | Some('/' | '[')
        )
    })?;
    let (operation, path) = (rest.get(..hash)?, rest.get(hash.saturating_add(1)..)?);
    let (pointer, picks) = match path.find('[') {
        Some(at) => (path.get(..at)?, path.get(at..)?),
        None => (path, ""),
    };
    let (operation, query) = operation.split_once('?').unwrap_or((operation, ""));
    if operation.is_empty() || operation.contains(char::is_whitespace) {
        return None;
    }
    let mut arguments = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=')?;
        arguments.insert(name.trim().to_owned(), value.trim().to_owned());
    }
    Some(Reference {
        view: ViewRef {
            app: app.to_owned(),
            record,
            operation: operation.to_owned(),
            arguments,
        },
        pointer: pointer.to_owned(),
        selections: selections(picks)?,
        range: false,
    })
}

/// The rules of `text` (`[rule]/pointer[rule]…`) and the pointer after
/// each; `None` when it is not that.
fn selections(text: &str) -> Option<Vec<Selection>> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.strip_prefix('[') {
        let mut quoted = false;
        let close = after.char_indices().find_map(|(at, c)| match c {
            '"' => {
                quoted = !quoted;
                None
            }
            ']' if !quoted => Some(at),
            _ => None,
        })?;
        let rule = after.get(..close)?.trim();
        let tail = after.get(close.saturating_add(1)..)?;
        let pointer_end = tail.find('[').unwrap_or(tail.len());
        let pointer = tail.get(..pointer_end)?.trim_end();
        if !(pointer.is_empty() || pointer.starts_with('/')) {
            return None;
        }
        found.push(Selection {
            rule: rule.to_owned(),
            pointer: pointer.to_owned(),
        });
        rest = tail.get(pointer_end..)?;
    }
    rest.is_empty().then_some(found)
}
