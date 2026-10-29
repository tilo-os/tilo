//! Filter columns (search.md, "Who indexes"): per filter of a type, and per other field a `filter`
//! compares, a list of the type's resources ordered by the field's value.
//!
//! A column is kept twice:
//!
//! * per collection, [`scope_list`]: entries `value ‖ SEP ‖ order key` with the item's id, so
//!   the items holding one value are one run in the collection's order, and a range of values
//!   one run in the values' order;
//! * across the workspace, for the type's filters, [`type_list`]: entries `value ‖ SEP ‖ path`
//!   with the resource's path; an identifier column also reversed ([`reversed_list`]), so a
//!   pattern that ends in what it names (`*@x.com`) is a run too.
//!
//! A value is written ([`write`]) so that the written values order as the values do: a number
//! as fixed-width hex with its sign flipped, a day or an instant as ISO text, a text by its
//! letters without regard to case and then as written. A resource that holds no value is
//! written as the empty value, which comes first; a set holds one entry per member.
//!
//! Equality, ranges, membership and identifier patterns are runs of a list ([`access`]): a
//! filtered query's first page reads the directory and the pages of its run, and how many a
//! value holds is the difference of two positions, so facet counts are read from the same
//! columns. Link paths (`from.contact.org`) are planned as columns of the linked type joined
//! through the reference column (`Ref` values are resource ids): not kept yet.

use tilolang::pred::{CmpOp, Lit, Operand, Pred};
use tilolang_runtime::store::{FieldValue, lit_of};

use super::IndexError;
use super::lists::Entry;
use crate::schema::{ColumnKind, ColumnSchema, FieldSchema, TypeSchema};

/// Separates an entry's value from what orders the entries of one value.
pub const SEP: char = '\u{0}';
/// Comes after the separator: what a run of one value ends before.
const AFTER: char = '\u{1}';
/// Comes after every written value.
const LAST: char = char::MAX;

/// The list of the column `column` of the items of the collection `field` of `parent`.
#[must_use]
pub fn scope_list(parent: &str, field: &str, column: &str) -> String {
    format!("{}{column}", scope_prefix(parent, field))
}

/// What starts the names of the columns of the items of the collection `field` of `parent`.
#[must_use]
pub fn scope_prefix(parent: &str, field: &str) -> String {
    format!("c\u{1}{parent}\u{1}{field}\u{1}")
}

/// The list of the column `column` of every resource of the type `ty`.
#[must_use]
pub fn type_list(ty: &str, column: &str) -> String {
    format!("w\u{1}{ty}\u{1}{column}")
}

/// The list of the column `column` of every resource of the type `ty`, each value written
/// backwards: what a pattern ending in what it names reads.
#[must_use]
pub fn reversed_list(ty: &str, column: &str) -> String {
    format!("w\u{1}{ty}\u{1}{column}\u{1}r")
}

/// An entry of a collection's column: the item `id`, of order key `key`, holding `value`.
#[must_use]
pub fn scope_entry(value: &str, key: &str, id: &str) -> Entry {
    (format!("{value}{SEP}{key}"), id.to_owned())
}

/// An entry of a type's column: the resource at `path` holding `value`.
#[must_use]
pub fn type_entry(value: &str, path: &str) -> Entry {
    (format!("{value}{SEP}{path}"), path.to_owned())
}

/// What orders an entry among those of its value: an item's order key, a resource's path.
#[must_use]
pub fn after_value(key: &str) -> &str {
    key.split_once(SEP).map_or(key, |(_, rest)| rest)
}

/// A value written backwards.
#[must_use]
pub fn reversed(value: &str) -> String {
    value.chars().rev().collect()
}

fn int(i: i64) -> String {
    format!("{:016x}", i.cast_unsigned() ^ (1 << 63))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "an Int compared with a Float column compares as the Float nearest to it, as the predicate does"
)]
const fn int_as_float(i: i64) -> f64 {
    i as f64
}

fn float(f: f64) -> String {
    let bits = (f + 0.0).to_bits();
    let ordered = if bits >> 63 == 1 {
        !bits
    } else {
        bits | (1 << 63)
    };
    format!("{ordered:016x}")
}

/// `lit` as a column of the kind `kind` writes it, in the values' order; the empty value for
/// none. `None` for a value of another kind.
#[must_use]
pub fn write(kind: ColumnKind, lit: &Lit) -> Option<String> {
    Some(match (kind, lit) {
        (_, Lit::Absent) => String::new(),
        (ColumnKind::Bool, Lit::Bool(b)) => if *b { "1" } else { "0" }.to_owned(),
        (ColumnKind::Int, Lit::Int(i)) => int(*i),
        // A set holds its members as their text.
        // optional-decode: a member that is not a number is a value of another kind (`None`).
        (ColumnKind::Int, Lit::Text(t)) => int(t.parse().ok()?),
        (ColumnKind::Float, Lit::Float(f)) => float(f.0),
        (ColumnKind::Float, Lit::Int(i)) => float(int_as_float(*i)),
        // optional-decode: a member that is not a number is a value of another kind (`None`).
        (ColumnKind::Float, Lit::Text(t)) => float(t.parse().ok()?),
        (ColumnKind::Text, x) => {
            let t = x.as_text()?;
            format!("{}\u{2}{t}", t.to_lowercase())
        }
        (ColumnKind::Date, Lit::Date(t) | Lit::Text(t))
        | (ColumnKind::Time, Lit::Time(t) | Lit::Text(t)) => t.clone(),
        (ColumnKind::Enum, Lit::Variant(v) | Lit::Text(v))
        | (ColumnKind::Ref, Lit::Ref(v) | Lit::Text(v)) => v.clone(),
        (ColumnKind::Ident, Lit::Ident(t) | Lit::Text(t)) => t.to_lowercase(),
        _ => return None,
    })
}

/// What a field holds as a predicate compares it, `value` being what the index holds of it (a
/// collection as [`FieldValue::Counter`] of its items).
#[must_use]
pub fn lit(field: &FieldSchema, value: FieldValue) -> Lit {
    lit_of(&field.id, value, &Lit::Absent).unwrap_or(Lit::Absent)
}

/// The values the column holds of a resource of the type `schema` whose fields `value` reads
/// (as the index holds them, a collection as the [`FieldValue::Counter`] of its items): one
/// per member of a set, the empty value for none.
///
/// # Errors
/// A field cannot be read.
pub fn values(
    column: &ColumnSchema,
    schema: &TypeSchema,
    value: &mut dyn FnMut(&FieldSchema) -> Result<FieldValue, IndexError>,
) -> Result<Vec<String>, IndexError> {
    if let Some(pred) = &column.pred {
        let mut failed = None;
        let holds = pred.eval(&mut |id: &String| -> Result<Lit, String> {
            let Some(field) = schema.field(id) else {
                return Ok(Lit::Absent);
            };
            match value(field) {
                Ok(v) => Ok(lit(field, v)),
                Err(e) => {
                    failed = Some(e);
                    Ok(Lit::Absent)
                }
            }
        });
        if let Some(e) = failed {
            return Err(e);
        }
        return Ok(vec![
            if holds.unwrap_or(false) { "1" } else { "0" }.to_owned(),
        ]);
    }
    let Some(field) = column
        .field
        .as_deref()
        .and_then(|id| schema.field(id).or_else(|| super::reflect::field(id)))
    else {
        return Ok(vec![String::new()]);
    };
    let held = value(field)?;
    let lit = if super::reflect::is_reflected(&field.id) {
        super::reflect::at_lit(&held)
    } else {
        lit(field, held)
    };
    Ok(match lit {
        Lit::Set(members) => {
            let mut out: Vec<String> = members
                .iter()
                .filter_map(|m| write(column.kind, m))
                .collect();
            out.sort();
            out.dedup();
            if out.is_empty() {
                out.push(String::new());
            }
            out
        }
        one => vec![write(column.kind, &one).unwrap_or_default()],
    })
}

/// Whether the index counts the entries of each value of the column (a toggle, an enum, one
/// value per resource): where a value's run is, and its facet count, are read from the counts.
#[must_use]
pub fn counted(column: &ColumnSchema) -> bool {
    matches!(column.kind, ColumnKind::Bool | ColumnKind::Enum) && !column.many
}

/// The value a run of one value is of.
#[must_use]
pub fn run_value((from, _): &(String, String)) -> Option<&str> {
    from.strip_suffix(SEP)
}

/// Whether the column reads the field `field`.
#[must_use]
pub fn reads(column: &ColumnSchema, field: &str) -> bool {
    column.field.as_deref() == Some(field)
        || column
            .pred
            .as_ref()
            .is_some_and(|pred| pred.fields().iter().any(|f| f.as_str() == field))
}

/// Where a predicate is answered from: runs of one list of a column, `[from, to)` each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    /// The column, by name.
    pub column: String,
    /// Whether the runs are of the column's values written backwards (across the workspace
    /// alone).
    pub reversed: bool,
    pub runs: Vec<(String, String)>,
    /// Whether the runs hold exactly what the predicate holds of, or more, to be checked.
    pub exact: bool,
    /// Whether a resource may stand in the runs more than once (a set's members).
    pub repeats: bool,
}

fn one_value(value: &str) -> (String, String) {
    (format!("{value}{SEP}"), format!("{value}{AFTER}"))
}

/// Whether two values compare as the column orders them: a day against a time compares by the
/// day, which the column does not.
fn same_kind(kind: ColumnKind, lit: &Lit) -> bool {
    !matches!(
        (kind, lit),
        (ColumnKind::Date, Lit::Time(_)) | (ColumnKind::Time, Lit::Date(_))
    )
}

fn flip(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

/// How `pred` is answered from the columns of `schema` (`id` naming the field of each of its
/// field operands), when one answers it: the runs of one column for a comparison, membership,
/// presence or pattern; several for one of several. `None` when it is read item by item.
/// `whole_type` says whether the lists across the workspace are read (they alone are kept
/// backwards).
pub fn access<F>(
    schema: &TypeSchema,
    pred: &Pred<F>,
    id: &dyn Fn(&F) -> String,
    whole_type: bool,
) -> Option<Vec<Access>> {
    // A named filter: its column.
    for column in schema.columns.iter().filter(|c| c.pred.is_some()) {
        let mapped = pred.resolve(&mut |f: &F| Some(id(f)));
        if mapped.as_ref() == column.pred.as_ref() {
            return Some(vec![run(column, one_value("1"), true)]);
        }
        if let Some(Pred::Not(inner)) = &mapped
            && Some(&**inner) == column.pred.as_ref()
        {
            return Some(vec![run(column, one_value("0"), true)]);
        }
    }
    let column_of = |f: &F| schema.column_of(&id(f));
    match pred {
        Pred::Cmp(op, Operand::Field(f), Operand::Lit(v)) => {
            comparison(column_of(f)?, *op, v).map(|a| vec![a])
        }
        Pred::Cmp(op, Operand::Lit(v), Operand::Field(f)) => {
            comparison(column_of(f)?, flip(*op), v).map(|a| vec![a])
        }
        Pred::Not(inner) => match &**inner {
            Pred::Cmp(CmpOp::Eq, Operand::Field(f), Operand::Lit(Lit::Bool(b)))
            | Pred::Cmp(CmpOp::Eq, Operand::Lit(Lit::Bool(b)), Operand::Field(f)) => {
                let column = column_of(f).filter(|c| c.kind == ColumnKind::Bool && !c.many)?;
                Some(vec![run(
                    column,
                    one_value(if *b { "0" } else { "1" }),
                    true,
                )])
            }
            Pred::Present(f) => {
                let column = column_of(f)?;
                Some(vec![run(column, one_value(""), true)])
            }
            _ => None,
        },
        Pred::Present(f) => {
            let column = column_of(f)?;
            Some(vec![run(
                column,
                (AFTER.to_string(), LAST.to_string()),
                true,
            )])
        }
        Pred::Has(f, v) => {
            let column = column_of(f)?;
            let w = write(column.kind, v)?;
            Some(vec![run(column, one_value(&w), true)])
        }
        Pred::Like(f, pattern) => {
            let column = column_of(f).filter(|c| c.kind == ColumnKind::Ident)?;
            Some(vec![pattern_run(column, pattern, whole_type)])
        }
        Pred::Or(each) => {
            let mut out: Vec<Access> = Vec::new();
            for p in each {
                for a in access(schema, p, id, whole_type)? {
                    match out
                        .iter_mut()
                        .find(|held| held.column == a.column && held.reversed == a.reversed)
                    {
                        Some(held) => {
                            held.runs.extend(a.runs);
                            held.exact &= a.exact;
                            held.repeats = true;
                        }
                        None => out.push(a),
                    }
                }
            }
            if out.len() > 1 {
                for a in &mut out {
                    a.repeats = true;
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// A comparison of the column's field with a value: one value, or a range of an ordered
/// column.
fn comparison(column: &ColumnSchema, op: CmpOp, v: &Lit) -> Option<Access> {
    if column.many || !same_kind(column.kind, v) {
        return None;
    }
    let w = write(column.kind, v)?;
    let ordered = matches!(
        column.kind,
        ColumnKind::Int | ColumnKind::Float | ColumnKind::Date | ColumnKind::Time
    );
    let r = match op {
        CmpOp::Eq => one_value(&w),
        CmpOp::Ne => return None,
        _ if !ordered || matches!(v, Lit::Absent) => return None,
        CmpOp::Lt => (AFTER.to_string(), w),
        CmpOp::Le => (AFTER.to_string(), format!("{w}{AFTER}")),
        CmpOp::Gt => (format!("{w}{AFTER}"), LAST.to_string()),
        CmpOp::Ge => (w, LAST.to_string()),
    };
    Some(run(column, r, true))
}

/// An identifier pattern: the run of what it starts with, or (across the workspace) of what it
/// ends with written backwards, whichever names more; exact when its one `*` is at that end.
fn pattern_run(column: &ColumnSchema, pattern: &str, whole_type: bool) -> Access {
    let pattern = pattern.to_lowercase();
    if !pattern.contains('*') {
        return run(column, one_value(&pattern), true);
    }
    let prefix = tilolang::identifier::prefix(&pattern);
    let suffix = tilolang::identifier::suffix(&pattern);
    let stars = pattern.matches('*').count();
    if whole_type && suffix.len() > prefix.len() {
        let back = reversed(suffix);
        let mut a = run(column, (back.clone(), format!("{back}{LAST}")), false);
        a.reversed = true;
        a.exact = stars == 1 && pattern.starts_with('*');
        return a;
    }
    let exact = stars == 1 && pattern.ends_with('*');
    run(
        column,
        (prefix.to_owned(), format!("{prefix}{LAST}")),
        exact,
    )
}

fn run(column: &ColumnSchema, r: (String, String), exact: bool) -> Access {
    Access {
        column: column.name.clone(),
        reversed: false,
        runs: vec![r],
        exact,
        repeats: column.many,
    }
}

/// The values of a column whose counts a facet shows, when they are known: a toggle's two,
/// an enum's cases.
#[must_use]
pub fn facet_values(column: &ColumnSchema) -> Vec<String> {
    match column.kind {
        ColumnKind::Bool => vec!["1".to_owned(), "0".to_owned()],
        ColumnKind::Enum => column.cases.clone(),
        _ => Vec::new(),
    }
}

/// The run of the entries of one value.
#[must_use]
pub fn value_run(value: &str) -> (String, String) {
    one_value(value)
}

/// Whether `run` is the run of one value, whose entries are in the order of what follows the
/// value (an order key, a path).
#[must_use]
pub fn is_value_run((from, to): &(String, String)) -> bool {
    from.strip_suffix(SEP)
        .is_some_and(|value| *to == format!("{value}{AFTER}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_values_order_as_the_values_do() {
        let ints: Vec<String> = [-5i64, -1, 0, 3, 1000]
            .iter()
            .map(|i| write(ColumnKind::Int, &Lit::Int(*i)).unwrap())
            .collect();
        let mut sorted = ints.clone();
        sorted.sort();
        assert_eq!(ints, sorted);
        let floats: Vec<String> = [-2.5f64, -0.0, 0.0, 1.5]
            .iter()
            .map(|f| write(ColumnKind::Float, &Lit::Float(tilolang::pred::Float(*f))).unwrap())
            .collect();
        let mut sorted = floats.clone();
        sorted.sort();
        assert_eq!(floats, sorted);
        assert_eq!(floats[1], floats[2], "-0 is 0");
        let texts: Vec<String> = ["apple", "Banana", "cherry"]
            .iter()
            .map(|t| write(ColumnKind::Text, &Lit::Text((*t).to_owned())).unwrap())
            .collect();
        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(texts, sorted, "without regard to case");
        assert_eq!(write(ColumnKind::Bool, &Lit::Absent).as_deref(), Some(""));
        assert_eq!(
            write(ColumnKind::Ident, &Lit::Ident("mailto:Jane@X.com".into())).as_deref(),
            Some("mailto:jane@x.com")
        );
    }
}
