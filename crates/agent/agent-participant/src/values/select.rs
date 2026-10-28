//! Which item of a list a reference means, said as a rule the host applies:
//! a model that reads three messages from one sender and copies "the
//! newest" itself takes an older one often enough to matter, and one that counts
//! positions miscounts.
//!
//! A rule is written in brackets after the pointer that reaches a list,
//! and a pointer into the item it picks may follow it:
//! `{{mail/read#/messages[from~Ada, after 2026-10-14T09:15Z, newest]/subject}}`,
//! `{{Sheet1!A1:H20[Invoice=INV-7]/Status}}`. Its clauses, separated by
//! commas, are applied in order:
//!
//! - `field=value`, `field~text`, and their negations `field!=value`,
//!   `field!~text`: the field equals or contains the value, case and runs
//!   of spaces folded; numbers compare as numbers (`2,000` = `2000`), and
//!   a field holding a list matches when one of its elements does. A value
//!   holding a comma or a bracket is quoted: `subject~"Re: [claim]"`.
//! - `after T`, `before T`, `on DATE`: the item's time, strictly after or
//!   before `T`, or on that day; `field after T` names the field. `T` is
//!   ISO (`2026-10-14T09:15Z`, `2026-10-14 09:15 UTC`; a date alone is its
//!   midnight, UTC when no zone is written), a written date (`October 14,
//!   2026`) or epoch seconds.
//! - `newest`, `oldest` (`newest by field`): order what is left by time,
//!   latest or earliest first.
//! - `first`, `last`, `nth N` (from 1, negative from the end) or `all`:
//!   which of what is left it takes, in the list's order or the one
//!   `newest`/`oldest` set. An order alone takes the first; a rule with
//!   neither must leave exactly one item. `all` keeps the list, and a
//!   pointer after it is taken from each item.
//!
//! An item's time is its first field named as times are (`date`, `ts`,
//! `time`, `received`, `sent`, `created`, `start`, …), else its first field
//! that holds written dates. Field names match with case, spaces, `_` and
//! `-` folded, so `receivedAt` names `received_at`; `a/b` names a field of
//! a field. Rows whose first row is all names (a sheet's range read from
//! its header) are read as records keyed by those names, and an object
//! holding exactly one list stands for that list.

use serde_json::Value as Json;

use super::{dates, expr};

/// What a rule picked: one item, or with `all` every item it left.
#[derive(Debug, PartialEq)]
pub(super) enum Picked {
    One(Json),
    All(Vec<Json>),
}

#[derive(Debug)]
enum Clause {
    Field {
        field: String,
        contains: bool,
        negated: bool,
        value: String,
    },
    Time {
        field: Option<String>,
        bound: Bound,
        at: i64,
    },
    Order {
        newest: bool,
        field: Option<String>,
    },
    Pick(Pick),
}

#[derive(Clone, Copy, Debug)]
enum Bound {
    After,
    Before,
    On,
}

#[derive(Clone, Copy, Debug)]
enum Pick {
    First,
    Last,
    Nth(i64),
    All,
}

const DAY_MS: i64 = 86_400_000;

/// Field names, values and items an error shows of a list at most.
const MOST_SHOWN: usize = 4;
const MOST_FIELDS_SHOWN: usize = 12;
const MOST_SHOWN_CHARS: usize = 40;

/// Field names an item's time goes by, folded as [`fold_name`] folds them,
/// most telling first.
const TIME_FIELDS: [&str; 20] = [
    "date",
    "ts",
    "timestamp",
    "time",
    "datetime",
    "received",
    "receivedat",
    "sent",
    "sentat",
    "created",
    "createdat",
    "updated",
    "updatedat",
    "modified",
    "start",
    "starts",
    "startsat",
    "due",
    "at",
    "when",
];

/// The item of `list` (or the list itself, with `all`) that `rule` picks.
///
/// # Errors
///
/// The rule cannot be read, names a field no item has, leaves nothing, or
/// leaves several where it takes one: each said with the rule and, briefly,
/// what the list held.
pub(super) fn select(list: &Json, rule: &str) -> Result<Picked, String> {
    let clauses = parse(rule)?;
    let items = items(list).map_err(|why| format!("[{rule}]: {why}"))?;
    let all: Vec<&Json> = items.iter().collect();
    if all.is_empty() {
        return Err(format!("[{rule}]: the list is empty"));
    }
    for field in clauses.iter().filter_map(Clause::field) {
        known(&all, field).map_err(|why| format!("[{rule}]: {why}"))?;
    }
    let mut left: Vec<(usize, &Json)> = all.iter().copied().enumerate().collect();
    for clause in &clauses {
        match clause {
            Clause::Field {
                field,
                contains,
                negated,
                value,
            } => left.retain(|(_, item)| holds(lookup(item, field), value, *contains) != *negated),
            Clause::Time { field, bound, at } => {
                let key =
                    time_field(&all, field.as_deref()).map_err(|why| format!("[{rule}]: {why}"))?;
                left.retain(|(_, item)| {
                    moment(item, &key).is_some_and(|when| bound.holds(when, *at))
                });
            }
            Clause::Order { .. } | Clause::Pick(_) => {}
        }
    }
    if left.is_empty() {
        return Err(format!(
            "nothing matches [{rule}]: the list held {}",
            held(&all, &clauses)
        ));
    }
    let orders: Vec<(bool, Option<&str>)> = clauses
        .iter()
        .filter_map(|clause| match clause {
            Clause::Order { newest, field } => Some((*newest, field.as_deref())),
            _ => None,
        })
        .collect();
    let picks: Vec<Pick> = clauses
        .iter()
        .filter_map(|clause| match clause {
            Clause::Pick(pick) => Some(*pick),
            _ => None,
        })
        .collect();
    if orders.len() > 1 || picks.len() > 1 {
        return Err(format!(
            "[{rule}]: at most one order (newest, oldest) and one pick (first, last, nth N, all)"
        ));
    }
    if let Some((newest, field)) = orders.first().copied() {
        let key = time_field(&all, field).map_err(|why| format!("[{rule}]: {why}"))?;
        left = by_time(&left, &key, newest);
        if left.is_empty() {
            return Err(format!("[{rule}]: no item left has a time in `{key}`"));
        }
    }
    let left: Vec<&Json> = left.into_iter().map(|(_, item)| item).collect();
    match picks.first().copied() {
        None if orders.is_empty() => {
            if let [one] = left.as_slice() {
                return Ok(Picked::One((*one).clone()));
            }
            Err(format!(
                "{} items match [{rule}] and it takes one: add newest, oldest, first, last, \
                 nth N or all, or narrow it ({})",
                left.len(),
                held(&left, &clauses)
            ))
        }
        None => pick(&left, Pick::First, rule),
        Some(chosen) => pick(&left, chosen, rule),
    }
}

/// The items of `left` that have a time in `key`, latest first when
/// `newest`, else earliest first; items of one time in the list's order,
/// reversed with it.
fn by_time<'a>(left: &[(usize, &'a Json)], key: &str, newest: bool) -> Vec<(usize, &'a Json)> {
    let mut timed: Vec<(i64, usize, &Json)> = left
        .iter()
        .filter_map(|(index, item)| moment(item, key).map(|when| (when, *index, *item)))
        .collect();
    timed.sort_by_key(|(when, index, _)| (*when, *index));
    if newest {
        timed.reverse();
    }
    timed
        .into_iter()
        .map(|(_, index, item)| (index, item))
        .collect()
}

/// The item `chosen` takes of `left`, which is not empty.
fn pick(left: &[&Json], chosen: Pick, rule: &str) -> Result<Picked, String> {
    let found = match chosen {
        Pick::All => {
            return Ok(Picked::All(
                left.iter().map(|item| (*item).clone()).collect(),
            ));
        }
        Pick::First => left.first(),
        Pick::Last => left.last(),
        Pick::Nth(n) => {
            let index = if n > 0 {
                usize::try_from(n.saturating_sub(1)).ok()
            } else {
                usize::try_from(n.unsigned_abs())
                    .ok()
                    .and_then(|back| left.len().checked_sub(back))
            };
            let found = index.and_then(|index| left.get(index));
            if found.is_none() {
                return Err(format!(
                    "only {} items match [{rule}]: nth {n} is past them",
                    left.len()
                ));
            }
            found
        }
    };
    found
        .map(|item| Picked::One((*item).clone()))
        .ok_or_else(|| format!("nothing matches [{rule}]"))
}

/// What `pointer` reaches in `item`: as JSON pointers do, else with names
/// folded as fields are (`/Status` in a row keyed `status`).
pub(super) fn at<'a>(item: &'a Json, pointer: &str) -> Option<&'a Json> {
    if let Some(found) = item.pointer(pointer) {
        return Some(found);
    }
    let path = pointer.strip_prefix('/')?;
    path.split('/').try_fold(item, |at, segment| {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        child(at, &segment)
    })
}

impl Clause {
    /// The field the clause names, when it names one.
    fn field(&self) -> Option<&str> {
        match self {
            Self::Field { field, .. } => Some(field),
            Self::Time { field, .. } | Self::Order { field, .. } => field.as_deref(),
            Self::Pick(_) => None,
        }
    }

    fn timed(&self) -> bool {
        matches!(self, Self::Time { .. } | Self::Order { .. })
    }
}

impl Bound {
    fn holds(self, when: i64, at: i64) -> bool {
        match self {
            Self::After => when > at,
            Self::Before => when < at,
            Self::On => {
                let day = at.saturating_sub(at.rem_euclid(DAY_MS));
                when >= day && when < day.saturating_add(DAY_MS)
            }
        }
    }
}

fn parse(rule: &str) -> Result<Vec<Clause>, String> {
    let clauses =
        split_clauses(rule).ok_or_else(|| format!("[{rule}]: a quote is never closed"))?;
    if clauses.is_empty() {
        return Err("[]: the rule is empty".into());
    }
    clauses
        .iter()
        .map(|text| clause(text).map_err(|why| format!("[{rule}]: {why}")))
        .collect()
}

/// `rule`'s clauses: split at commas outside double quotes.
fn split_clauses(rule: &str) -> Option<Vec<String>> {
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in rule.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            ',' if !quoted => clauses.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    if quoted {
        return None;
    }
    clauses.push(current);
    Some(
        clauses
            .into_iter()
            .map(|clause| clause.trim().to_owned())
            .filter(|clause| !clause.is_empty())
            .collect(),
    )
}

fn clause(text: &str) -> Result<Clause, String> {
    if let Some(at) = operator(text) {
        let field = text.get(..at).unwrap_or_default();
        let contains = text.get(at..).is_some_and(|rest| rest.starts_with('~'));
        let value = text.get(at.saturating_add(1)..).unwrap_or_default();
        let negated = field.ends_with('!');
        let field = field.trim_end_matches('!').trim();
        if field.is_empty() {
            return Err(format!("`{text}` names no field"));
        }
        return Ok(Clause::Field {
            field: field.to_owned(),
            contains,
            negated,
            value: unquote(value),
        });
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
    let lower: Vec<&str> = lower.iter().map(String::as_str).collect();
    match lower.as_slice() {
        ["first"] => return Ok(Clause::Pick(Pick::First)),
        ["last"] => return Ok(Clause::Pick(Pick::Last)),
        ["all"] => return Ok(Clause::Pick(Pick::All)),
        ["nth", n] => {
            return match n.parse::<i64>() {
                Ok(n) if n != 0 => Ok(Clause::Pick(Pick::Nth(n))),
                _ => Err(format!("`{text}`: nth counts from 1 (or back from -1)")),
            };
        }
        [order @ ("newest" | "oldest"), rest @ ..] => {
            let field = match rest {
                [] => None,
                ["by", _, ..] => Some(words.iter().skip(2).copied().collect::<Vec<_>>().join(" ")),
                _ => return Err(format!("`{text}`: write {order}, or {order} by <field>")),
            };
            return Ok(Clause::Order {
                newest: *order == "newest",
                field,
            });
        }
        _ => {}
    }
    for (index, word) in lower.iter().enumerate().rev() {
        let bound = match *word {
            "after" => Bound::After,
            "before" => Bound::Before,
            "on" => Bound::On,
            _ => continue,
        };
        let when = words
            .iter()
            .skip(index.saturating_add(1))
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let field = words
            .iter()
            .take(index)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let at = dates::instant(&unquote(&when)).ok_or_else(|| {
            format!("`{text}`: `{when}` is not a time (write 2026-10-14T09:15Z or 2026-10-14)")
        })?;
        return Ok(Clause::Time {
            field: (!field.is_empty()).then(|| unquote(&field)),
            bound,
            at,
        });
    }
    Err(format!(
        "`{text}` is not a clause: write field=value, field~text, after/before <time>, \
         newest, oldest, first, last, nth N or all"
    ))
}

/// Where the first `=` or `~` outside double quotes is.
fn operator(text: &str) -> Option<usize> {
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '=' | '~' if !quoted => return Some(at),
            _ => {}
        }
    }
    None
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(text)
        .to_owned()
}

/// The items of `list`: its elements, a table's rows as records keyed by
/// its header, or the one list an object holds.
fn items(list: &Json) -> Result<Vec<Json>, String> {
    match list {
        Json::Array(rows) => match rows.first() {
            Some(Json::Array(header)) => table(header, rows.get(1..).unwrap_or_default()),
            _ => Ok(rows.clone()),
        },
        Json::Object(fields) => {
            let lists: Vec<(&String, &Json)> = fields
                .iter()
                .filter(|(_, value)| value.is_array())
                .collect();
            match lists.as_slice() {
                [(_, one)] => items(one),
                [] => Err("it is not a list".into()),
                several => Err(format!(
                    "it holds several lists: point at one, as #/{}[…]",
                    several
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join("[…] or #/")
                )),
            }
        }
        _ => Err("it is not a list".into()),
    }
}

/// `rows` as records keyed by the names in `header`; a column with no name
/// is left out, and an empty row skipped.
fn table(header: &[Json], rows: &[Json]) -> Result<Vec<Json>, String> {
    let mut names = Vec::new();
    for (column, cell) in header.iter().enumerate() {
        match cell {
            _ if blank(cell) => {}
            Json::String(name) => names.push((column, name.trim().to_owned())),
            _ => return Err(no_header()),
        }
    }
    if names.is_empty() {
        return Err(no_header());
    }
    Ok(rows
        .iter()
        .filter_map(Json::as_array)
        .filter(|row| !row.iter().all(blank))
        .map(|row| {
            Json::Object(
                names
                    .iter()
                    .map(|(column, name)| {
                        (
                            name.clone(),
                            row.get(*column).cloned().unwrap_or(Json::Null),
                        )
                    })
                    .collect(),
            )
        })
        .collect())
}

fn no_header() -> String {
    "its rows have no header row of names: start the range at the header".into()
}

fn blank(value: &Json) -> bool {
    match value {
        Json::Null => true,
        Json::String(text) => text.trim().is_empty(),
        _ => false,
    }
}

/// The field `path` names in `item`: the name whole, else `a/b` as a field
/// of a field.
fn lookup<'a>(item: &'a Json, path: &str) -> Option<&'a Json> {
    child(item, path).or_else(|| {
        path.contains('/')
            .then(|| path.split('/').try_fold(item, child))
            .flatten()
    })
}

fn child<'a>(at: &'a Json, name: &str) -> Option<&'a Json> {
    match at {
        Json::Object(fields) => fields.get(name).or_else(|| {
            let wanted = fold_name(name);
            fields
                .iter()
                .find(|(key, _)| fold_name(key) == wanted)
                .map(|(_, value)| value)
        }),
        Json::Array(items) => name
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index)),
        _ => None,
    }
}

fn fold_name(name: &str) -> String {
    name.chars()
        .filter(|c| !(c.is_whitespace() || *c == '_' || *c == '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// `text` with case and runs of spaces folded.
fn fold(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn text_of(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whether `value` equals (or contains) `wanted`.
fn holds(value: Option<&Json>, wanted: &str, contains: bool) -> bool {
    match value {
        None => false,
        Some(Json::Array(items)) => items.iter().any(|item| holds(Some(item), wanted, contains)),
        Some(value) => {
            let text = text_of(value);
            let (have, want) = (fold(&text), fold(wanted));
            if contains {
                have.contains(&want)
            } else {
                have == want
                    || expr::lenient_number(&text)
                        .zip(expr::lenient_number(wanted))
                        .is_some_and(|(a, b)| (a - b).abs() < 1e-9)
            }
        }
    }
}

/// Whether some item has `field`.
fn known(items: &[&Json], field: &str) -> Result<(), String> {
    if items.iter().any(|item| lookup(item, field).is_some()) {
        Ok(())
    } else {
        Err(format!(
            "no field `{field}`: the items have {}",
            fields_of(items).join(", ")
        ))
    }
}

/// The names of the items' fields, in the order they first appear.
fn fields_of(items: &[&Json]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for fields in items.iter().filter_map(|item| item.as_object()) {
        for name in fields.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names
}

/// The field the items' times are in: `named`, else the first named as
/// times are, else the first holding written dates.
fn time_field(items: &[&Json], named: Option<&str>) -> Result<String, String> {
    if let Some(named) = named {
        known(items, named)?;
        return Ok(named.to_owned());
    }
    let keys = fields_of(items);
    for candidate in TIME_FIELDS {
        if let Some(key) = keys.iter().find(|key| fold_name(key) == candidate)
            && items.iter().any(|item| moment(item, key).is_some())
        {
            return Ok(key.clone());
        }
    }
    keys.iter()
        .find(|key| {
            let present: Vec<&Json> = items
                .iter()
                .filter_map(|item| lookup(item, key))
                .filter(|value| !blank(value))
                .collect();
            !present.is_empty()
                && present.iter().all(|value| {
                    value.as_str().is_some_and(|text| {
                        !text.trim().chars().all(|c| c.is_ascii_digit())
                            && dates::instant(text).is_some()
                    })
                })
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "the items have no time field ({}): name one, as `<field> after …` or \
                 `newest by <field>`",
                shown(&keys, MOST_FIELDS_SHOWN)
            )
        })
}

/// The moment `item`'s field `key` holds, in milliseconds since the epoch.
fn moment(item: &Json, key: &str) -> Option<i64> {
    match lookup(item, key)? {
        Json::String(text) => dates::instant(text),
        Json::Number(number) => dates::epoch(number.as_i64()?),
        _ => None,
    }
}

/// What `items` held, briefly: how many, the values of the fields the rule
/// names, and the span of their times.
fn held(items: &[&Json], clauses: &[Clause]) -> String {
    let mut parts = vec![if items.len() == 1 {
        "1 item".to_owned()
    } else {
        format!("{} items", items.len())
    }];
    let mut named: Vec<&str> = Vec::new();
    for field in clauses.iter().filter_map(Clause::field) {
        if !named.contains(&field) {
            named.push(field);
        }
    }
    for field in &named {
        let mut values: Vec<String> = Vec::new();
        for value in items.iter().filter_map(|item| lookup(item, field)) {
            let text = brief(&text_of(value));
            if !values.contains(&text) {
                values.push(text);
            }
        }
        parts.push(format!("{field}: {}", shown(&values, MOST_SHOWN)));
    }
    if clauses.iter().any(Clause::timed)
        && let Ok(key) = time_field(items, None)
    {
        let mut timed: Vec<(i64, String)> = items
            .iter()
            .filter_map(|item| {
                let when = moment(item, &key)?;
                Some((when, text_of(lookup(item, &key)?)))
            })
            .collect();
        timed.sort();
        if let (Some((_, first)), Some((_, last))) = (timed.first(), timed.last())
            && !named.contains(&key.as_str())
        {
            parts.push(format!("{key} {first} to {last}"));
        }
    }
    if named.is_empty() && !clauses.iter().any(Clause::timed) {
        parts.push(format!(
            "fields {}",
            shown(&fields_of(items), MOST_FIELDS_SHOWN)
        ));
    }
    parts.join("; ")
}

/// The first `most` of `values`, and how many more there are.
fn shown(values: &[String], most: usize) -> String {
    let text = values
        .iter()
        .take(most)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > most {
        format!("{text} and {} more", values.len().saturating_sub(most))
    } else {
        text
    }
}

/// `text` cut to [`MOST_SHOWN_CHARS`] characters.
pub(super) fn brief(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MOST_SHOWN_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MOST_SHOWN_CHARS).collect();
    cut.push('…');
    cut
}
