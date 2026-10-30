//! Queries (search.md, "A query" and "What a type declares"; agents.md, "Finding"): what people
//! type in search, `find("from:*@x.com is:unread received:-7d")`, and what code writes,
//! `query(Message, from: "*@x.com", unread: true, received: -7d)`, are one value, a [`Query`].
//! [`resolve`] reads it against a type's filters into the predicate and the order a store answers
//! from its index.
//!
//! The tokens:
//! * `key:value` sets the filter `key`: `status:open`, `due:today`, `size:>10`,
//!   `received:2026-10-01..`, `tag:urgent`, `invoice:none`, `from:*@x.com`;
//! * `is:key` turns a toggle on: `is:unread`;
//! * an identifier or a pattern alone (`*@x.com`, `jane@x.com`, `tel:+49*`,
//!   `discord:janedoe`) matches any identifier field;
//! * `sort:-received` orders by a field, `-` putting the latest or last first;
//! * `in:<resource>` looks inside one resource;
//! * a type's name first (`Thread from:...`) searches that type;
//! * any other word, `"a phrase"` and `~"fuzzy text"` is text searched for.
//!
//! Values: `true`, `false`; numbers; `a..b`, `a..`, `..b`, `>x`, `>=x`, `<x`, `<=x`; days
//! (`2026-10-24`), months (`2026-10`), times (`2026-10-24T09:30:00Z`); `today`, `now`, each
//! plus or minus a duration (`today+7d`); a duration alone, from now back (`-7d`) or forward
//! (`+7d`); `none` and `any` for a filter that may hold no value; `a,b` for several cases of
//! an enum.

use std::cell::Cell;

use tilolang::calendar::{DAY, calendar_span, day_text, instant, time_text};
use tilolang::identifier;
use tilolang::meta::{EnumMeta, FilterKind, FilterMeta, TypeMeta};
use tilolang::pred::{CmpOp, Float, Lit, Operand, Pred};

/// A query, typed or built in code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// The type a typed search names first (`Thread from:...`), when it names one.
    pub ty: Option<String>,
    /// What is searched for as text: words and phrases.
    pub text: Vec<String>,
    /// The filters set, in order.
    pub terms: Vec<Term>,
    /// `sort:-received`: a field, `-` first for the latest or last first.
    pub sort: Option<String>,
    /// `in:t91`: the resource the results are inside.
    pub scope: Option<String>,
}

/// One filter a query sets: `key:value`, `is:key` as `key:true`, or (no key) an identifier
/// alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub key: Option<String>,
    pub value: String,
}

impl Term {
    #[must_use]
    pub fn new(key: &str, value: &str) -> Self {
        Self {
            key: Some(key.to_owned()),
            value: value.to_owned(),
        }
    }
}

/// The words of a typed search, quotes keeping a phrase together: `from:"Jane Doe"` and
/// `~"83(b) election"` are one word each.
fn words(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if quoted {
        return Err(format!("`{input}` opens a quote it does not close"));
    }
    if !word.is_empty() {
        out.push(word);
    }
    Ok(out)
}

/// A word without the quotes around it: `"Jane Doe"` as `Jane Doe`, `~"YC"` as `YC`.
fn unquoted(word: &str) -> &str {
    let word = word.strip_prefix('~').unwrap_or(word);
    word.strip_prefix('"')
        .and_then(|w| w.strip_suffix('"'))
        .unwrap_or(word)
}

fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

fn is_type_name(word: &str) -> bool {
    let last = word.rsplit('.').next().unwrap_or(word);
    last.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// The query a typed search writes.
///
/// # Errors
/// A message saying what is wrong with it: a quote not closed, a token with no value.
pub fn find(input: &str) -> Result<Query, String> {
    let mut query = Query::default();
    for (at, word) in words(input)?.into_iter().enumerate() {
        if at == 0 && is_type_name(&word) {
            query.ty = Some(word);
            continue;
        }
        if word.starts_with('"') || word.starts_with("~\"") {
            query.text.push(unquoted(&word).to_owned());
            continue;
        }
        if let Some((key, value)) = word.split_once(':')
            && is_key(key)
        {
            let value = unquoted(value);
            if value.is_empty() {
                return Err(format!("`{word}` has no value: write `{key}:<value>`"));
            }
            match key {
                "is" => query.terms.push(Term::new(value, "true")),
                "sort" => query.sort = Some(value.to_owned()),
                "in" => query.scope = Some(value.to_owned()),
                _ => query.terms.push(Term::new(key, value)),
            }
            continue;
        }
        if word.contains('@') || word.starts_with('+') {
            query.terms.push(Term {
                key: None,
                value: word,
            });
            continue;
        }
        query.text.push(word);
    }
    Ok(query)
}

/// What a store answers a query with: the predicate over a resource's fields (by field id)
/// and the order, a field and whether the latest or last comes first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub pred: Pred<String>,
    pub order: Option<Order>,
}

/// An order of a query's results.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Order {
    /// The field, by id.
    pub field: String,
    /// How it merges, as a store reads it.
    pub kind: crate::meta::FieldKind,
    /// The latest, largest or last in the alphabet first.
    pub descending: bool,
}

/// `pred` (over the fields of `ty`, by id; `app` the package declaring `ty`) as a store reads
/// it: each field with how it merges and what an unwritten one holds. `None` when it reads a
/// field `ty` lacks, or one no filter reads.
#[must_use]
pub fn store_pred(pred: &Pred<String>, ty: &TypeMeta, app: &str) -> Option<crate::store::Pred> {
    pred.resolve(&mut |id: &String| {
        let field = ty.fields.iter().find(|f| f.id == *id)?;
        let parsed = crate::meta::parse_ty(&field.ty, app).ok()?;
        let kind = crate::meta::field_kind(field, &parsed);
        crate::store::FieldRead::of(&field.id, kind, &parsed, field.default.as_ref())
    })
}

/// How finely a query reads the running clock where nothing it compares says when its answer
/// changes (`now`, `-7d`): to the minute. A view's query reads its clock to the minute, so it
/// reads the same throughout one; a view that read the instant runs again at the next minute.
pub const INSTANT_GRAIN: i64 = 60;

/// The time a query is answered at, which `today`, `now` and `-7d` are read against, and when
/// what was read of it next reads differently ([`Self::changes_at`]).
#[derive(Clone, Debug)]
pub struct Clock {
    /// Seconds since 1970-01-01T00:00:00Z.
    now: i64,
    /// How many seconds the person's clock is ahead of UTC: the zone `today` and a day such as
    /// `2026-10-24` are days in.
    zone: i64,
    /// The first instant after `now` at which what was read of the clock reads differently.
    changes: Cell<Option<i64>>,
}

impl Clock {
    /// The clock at the ISO-8601 UTC instant `now`, read in UTC.
    ///
    /// # Errors
    /// `now` is not such an instant.
    pub fn at(now: &str) -> Result<Self, String> {
        Ok(Self {
            now: instant(now).ok_or_else(|| format!("`{now}` is not a time"))?,
            zone: 0,
            changes: Cell::new(None),
        })
    }

    /// The first instant after its time at which what was read of it reads differently, in
    /// seconds since 1970 (UTC): the person's next midnight for `today`, the next minute for
    /// `now` ([`INSTANT_GRAIN`]). `None` while nothing read it: an answer that read no time
    /// holds whenever it is asked.
    #[must_use]
    pub fn changes_at(&self) -> Option<i64> {
        self.changes.get()
    }

    fn changes_by(&self, at: i64) {
        let at = at.max(self.now.saturating_add(1));
        self.changes
            .set(Some(self.changes.get().map_or(at, |seen| seen.min(at))));
    }

    /// Read the person's day of the instant `offset` seconds from now: it reads differently once
    /// that instant reaches their next midnight.
    fn read_day(&self, offset: i64) {
        let next_midnight = self
            .day_of(self.now.saturating_add(offset))
            .saturating_add(1)
            .saturating_mul(DAY)
            .saturating_sub(self.zone);
        self.changes_by(next_midnight.saturating_sub(offset));
    }

    /// Read the instant itself (`now`, `now - 7d`): it reads differently at the next minute.
    fn read_instant(&self) {
        let next = self
            .now
            .div_euclid(INSTANT_GRAIN)
            .saturating_add(1)
            .saturating_mul(INSTANT_GRAIN);
        self.changes_by(next);
    }

    /// The same clock read by someone whose clock is `zone` seconds ahead of UTC: where their
    /// `today` starts and ends.
    #[must_use]
    pub fn in_zone(self, zone: i64) -> Self {
        Self { zone, ..self }
    }

    /// The person's day of the instant `at`, in days since 1970-01-01.
    fn day_of(&self, at: i64) -> i64 {
        at.saturating_add(self.zone).div_euclid(DAY)
    }
}

/// `query` read against the filters of `ty` (`enums` naming the enums its filters compare)
/// at the time `clock`.
///
/// # Errors
/// A message saying which token does not fit: a filter the type lacks, a value its filter does
/// not take, text (which search answers, not a query of one type).
pub fn resolve(
    query: &Query,
    ty: &TypeMeta,
    enums: &[EnumMeta],
    clock: &Clock,
) -> Result<Resolved, String> {
    if let Some(word) = query.text.first() {
        return Err(format!(
            "`{word}` is text to search for; a query of {} sets its filters (`is:`, `key:value`), and text is searched across types",
            ty.name
        ));
    }
    let mut all = Vec::new();
    for term in &query.terms {
        all.push(term_pred(term, ty, enums, clock)?);
    }
    let order = match &query.sort {
        Some(sort) => Some(order_of(sort, ty)?),
        None => None,
    };
    let pred = match all.len() {
        0 => Pred::Const(true),
        1 => all.remove(0),
        _ => Pred::And(all),
    };
    Ok(Resolved { pred, order })
}

fn filter_keys(ty: &TypeMeta) -> String {
    let keys: Vec<&str> = ty.search.filters.iter().map(|f| f.key.as_str()).collect();
    if keys.is_empty() {
        format!("{} has no filters", ty.name)
    } else {
        format!("the filters of {}: {}", ty.name, keys.join(", "))
    }
}

fn term_pred(
    term: &Term,
    ty: &TypeMeta,
    enums: &[EnumMeta],
    clock: &Clock,
) -> Result<Pred<String>, String> {
    let Some(key) = &term.key else {
        return any_identifier(&term.value, ty);
    };
    if let Some(filter) = ty.search.filter(key) {
        return filter_pred(filter, &term.value, enums, clock)
            .map_err(|why| format!("`{key}:{}`: {why}", term.value));
    }
    // `discord:janedoe`, `mailto:jane@x.com`: an identifier, not a filter, where the type
    // holds identifiers.
    let holds_identifiers = ty
        .search
        .filters
        .iter()
        .any(|f| f.kind == FilterKind::Identifier);
    if holds_identifiers
        && !key.contains('.')
        && identifier::normalize_pattern(&format!("{key}:{}", term.value)).is_ok()
    {
        return any_identifier(&format!("{key}:{}", term.value), ty);
    }
    if key.contains('.') {
        return Err(format!(
            "`{key}` goes through a reference; filters on paths are not answered yet. {}",
            filter_keys(ty)
        ));
    }
    Err(format!("`{key}` is no filter; {}", filter_keys(ty)))
}

/// An identifier or a pattern alone: whether any identifier field of the type holds a match.
/// A type with no identifier field holds none.
fn any_identifier(value: &str, ty: &TypeMeta) -> Result<Pred<String>, String> {
    let pattern = identifier::normalize_pattern(value)?;
    let each: Vec<Pred<String>> = ty
        .search
        .filters
        .iter()
        .filter(|f| f.kind == FilterKind::Identifier)
        .filter_map(|f| Some(identifier_pred(f.field.clone()?, f.many, &pattern)))
        .collect();
    Ok(match each.len() {
        0 => Pred::Const(false),
        1 => each.into_iter().next().unwrap_or(Pred::Const(false)),
        _ => Pred::Or(each),
    })
}

fn identifier_pred(field: String, many: bool, pattern: &str) -> Pred<String> {
    if pattern.contains('*') {
        Pred::Like(field, pattern.to_owned())
    } else if many {
        Pred::Has(field, Lit::Ident(pattern.to_owned()))
    } else {
        Pred::Cmp(
            CmpOp::Eq,
            Operand::Field(field),
            Operand::Lit(Lit::Ident(pattern.to_owned())),
        )
    }
}

fn eq(field: &str, lit: Lit) -> Pred<String> {
    Pred::Cmp(
        CmpOp::Eq,
        Operand::Field(field.to_owned()),
        Operand::Lit(lit),
    )
}

fn cmp(field: &str, op: CmpOp, lit: Lit) -> Pred<String> {
    Pred::Cmp(op, Operand::Field(field.to_owned()), Operand::Lit(lit))
}

/// One of several: `Or`, or the one alone.
fn one_of(mut each: Vec<Pred<String>>) -> Pred<String> {
    if each.len() == 1 {
        each.remove(0)
    } else {
        Pred::Or(each)
    }
}

fn filter_pred(
    f: &FilterMeta,
    value: &str,
    enums: &[EnumMeta],
    clock: &Clock,
) -> Result<Pred<String>, String> {
    if f.kind == FilterKind::Predicate {
        // A filter on the time it is asked at ("Today": `due <= today`) reads it now, and
        // reads differently once the day (or the instant) it read moves on.
        let declared = f.pred.as_ref().ok_or("the filter has no predicate")?;
        let reads = declared.clock_reads();
        if reads.day {
            // A day operand counts whole days from the person's day: it moves at midnight.
            clock.read_day(0);
        }
        if reads.instant {
            clock.read_instant();
        }
        let pred = declared.at(clock.now, clock.zone);
        return match value {
            "true" => Ok(pred),
            "false" => Ok(Pred::Not(Box::new(pred))),
            _ => Err("a toggle is `true` or `false`".into()),
        };
    }
    let field = f.field.as_deref().ok_or("the filter is on no field")?;
    if f.optional {
        match value {
            "none" => return Ok(Pred::Not(Box::new(Pred::Present(field.to_owned())))),
            "any" => return Ok(Pred::Present(field.to_owned())),
            _ => {}
        }
    }
    let member = |lit: Lit| {
        if f.many {
            Pred::Has(field.to_owned(), lit)
        } else {
            eq(field, lit)
        }
    };
    match f.kind {
        FilterKind::Predicate => Err("unreachable".into()),
        FilterKind::Bool => match value {
            "true" => Ok(eq(field, Lit::Bool(true))),
            "false" => Ok(eq(field, Lit::Bool(false))),
            _ => Err("a toggle is `true` or `false`".into()),
        },
        FilterKind::Enum => {
            let name =
                f.ty.trim_start_matches("Set<")
                    .trim_start_matches("Option<")
                    .trim_end_matches('>')
                    .trim_start_matches("enum ");
            let last = name.rsplit('.').next().unwrap_or(name);
            let meta = enums
                .iter()
                .find(|e| e.name == last || e.id == name)
                .ok_or_else(|| format!("its enum {name} is unknown"))?;
            let mut each = Vec::new();
            for case in value.split(',') {
                let variant = meta
                    .variants
                    .iter()
                    .find(|v| v.name == case)
                    .ok_or_else(|| {
                        let names: Vec<&str> =
                            meta.variants.iter().map(|v| v.name.as_str()).collect();
                        format!(
                            "{} has no case `{case}`; its cases: {}",
                            meta.name,
                            names.join(", ")
                        )
                    })?;
                each.push(member(Lit::Variant(variant.id.clone())));
            }
            Ok(one_of(each))
        }
        FilterKind::Reference => Ok(member(Lit::Ref(value.to_owned()))),
        FilterKind::Text => Ok(member(Lit::Text(value.to_owned()))),
        FilterKind::Identifier => {
            let pattern = identifier::normalize_pattern(value)?;
            Ok(identifier_pred(field.to_owned(), f.many, &pattern))
        }
        FilterKind::Number => {
            let float = f.ty.contains("Float");
            let lit = |text: &str| -> Result<Lit, String> {
                if float {
                    text.parse::<f64>()
                        .map(|v| Lit::Float(Float(v)))
                        .map_err(|_| format!("`{text}` is not a number"))
                } else {
                    text.parse::<i64>()
                        .map(Lit::Int)
                        .map_err(|_| format!("`{text}` is not a whole number"))
                }
            };
            compare(field, value, &|text| {
                let v = lit(text)?;
                Ok((v.clone(), v, true))
            })
        }
        FilterKind::Date | FilterKind::Time => {
            let day = f.kind == FilterKind::Date;
            compare(field, value, &|text| span_of(text, day, clock))
        }
    }
}

/// `x`, `a..b`, `>x`, `>=x`, `<x`, `<=x` over a field, each point read by `point` as the
/// span it stands for: its first value, its last, and whether the last is in it.
fn compare(
    field: &str,
    value: &str,
    point: &dyn Fn(&str) -> Result<Span, String>,
) -> Result<Pred<String>, String> {
    let upto = |end: (Lit, Lit, bool)| cmp(field, if end.2 { CmpOp::Le } else { CmpOp::Lt }, end.1);
    if let Some(rest) = value.strip_prefix(">=") {
        return Ok(cmp(field, CmpOp::Ge, point(rest)?.0));
    }
    if let Some(rest) = value.strip_prefix("<=") {
        return Ok(upto(point(rest)?));
    }
    if let Some(rest) = value.strip_prefix('>') {
        let (_, last, inclusive) = point(rest)?;
        return Ok(cmp(
            field,
            if inclusive { CmpOp::Gt } else { CmpOp::Ge },
            last,
        ));
    }
    if let Some(rest) = value.strip_prefix('<') {
        return Ok(cmp(field, CmpOp::Lt, point(rest)?.0));
    }
    if let Some((lo, hi)) = value.split_once("..") {
        let mut both = Vec::new();
        if !lo.is_empty() {
            both.push(cmp(field, CmpOp::Ge, point(lo)?.0));
        }
        if !hi.is_empty() {
            both.push(upto(point(hi)?));
        }
        return Ok(match both.len() {
            0 => Pred::Present(field.to_owned()),
            1 => both.remove(0),
            _ => Pred::And(both),
        });
    }
    let (first, last, inclusive) = point(value)?;
    if first == last && inclusive {
        return Ok(eq(field, first));
    }
    Ok(Pred::And(vec![
        cmp(field, CmpOp::Ge, first),
        cmp(field, if inclusive { CmpOp::Le } else { CmpOp::Lt }, last),
    ]))
}

/// Whether the instant `at` (ISO-8601 UTC) is in the time `value` as a Time filter reads it at
/// `clock` (`-14d`, `today`, `2026-10-01..`, `>2026-10-01`): what `grep(changed: -14d)` asks
/// of a resource's last change.
///
/// # Errors
/// `value` is no time a filter takes.
pub fn time_holds(value: &str, at: &str, clock: &Clock) -> Result<bool, String> {
    let pred = compare("at", value, &|text| span_of(text, false, clock))?;
    pred.eval(&mut |_: &String| Ok::<Lit, String>(Lit::Time(at.to_owned())))
}

/// What a point of a filter's value stands for: its first value, its last, and whether the
/// last is in it (a day of a Time filter ends before the next day starts).
type Span = (Lit, Lit, bool);

/// The span of days (`day`) or of instants a point of a Date or Time filter stands for.
fn span_of(text: &str, day: bool, clock: &Clock) -> Result<Span, String> {
    let today = clock.day_of(clock.now);
    // A duration alone, from now: `-7d` back to now, `+7d` on from now.
    if let Some((n, unit)) = tilolang::duration::parse(text)
        && (text.starts_with('-') || text.starts_with('+'))
    {
        let secs = tilolang::duration::seconds(n, unit).ok_or("the duration is too long")?;
        let then = clock.now.saturating_add(secs);
        let (a, b) = (then.min(clock.now), then.max(clock.now));
        if day {
            clock.read_day(0);
            clock.read_day(secs);
        } else {
            clock.read_instant();
        }
        return Ok(if day {
            (
                Lit::Date(day_text(clock.day_of(a))),
                Lit::Date(day_text(clock.day_of(b))),
                true,
            )
        } else {
            (Lit::Time(time_text(a)), Lit::Time(time_text(b)), true)
        });
    }
    // `today`, `now`, either plus or minus a duration.
    // The person's today starts at their midnight.
    let midnight = today.saturating_mul(DAY).saturating_sub(clock.zone);
    for (word, base) in [("today", midnight), ("now", clock.now)] {
        let Some(rest) = text.strip_prefix(word) else {
            continue;
        };
        let offset = if rest.is_empty() {
            0
        } else {
            let (n, unit) = tilolang::duration::parse(rest)
                .ok_or_else(|| format!("`{text}`: write `{word}+7d` or `{word}-1h`"))?;
            tilolang::duration::seconds(n, unit).ok_or("the duration is too long")?
        };
        let at = base.saturating_add(offset);
        let whole_day = word == "today" && offset.rem_euclid(DAY) == 0;
        if word == "today" {
            // Counted from the person's midnight: it moves when their day does.
            clock.read_day(0);
        } else if day {
            clock.read_day(offset);
        } else {
            clock.read_instant();
        }
        return Ok(match (day, whole_day) {
            (true, _) => {
                let d = Lit::Date(day_text(clock.day_of(at)));
                (d.clone(), d, true)
            }
            (false, true) => (
                Lit::Time(time_text(at)),
                Lit::Time(time_text(at.saturating_add(DAY))),
                false,
            ),
            (false, false) => {
                let t = Lit::Time(time_text(at));
                (t.clone(), t, true)
            }
        });
    }
    // `2026-10-24T09:30:00Z`, `2026-10-24`, `2026-10`, `2026`.
    if let Some(at) = instant(text) {
        return Ok(if day {
            let d = Lit::Date(day_text(clock.day_of(at)));
            (d.clone(), d, true)
        } else {
            let t = Lit::Time(time_text(at));
            (t.clone(), t, true)
        });
    }
    let (start, end) = calendar_span(text).ok_or_else(|| {
        format!("`{text}` is not a day (`2026-10-24`), a month (`2026-10`), `today`, `now` or a duration such as `-7d`")
    })?;
    Ok(if day {
        (
            Lit::Date(day_text(start)),
            Lit::Date(day_text(end.saturating_sub(1))),
            true,
        )
    } else {
        (
            Lit::Time(time_text(
                start.saturating_mul(DAY).saturating_sub(clock.zone),
            )),
            Lit::Time(time_text(
                end.saturating_mul(DAY).saturating_sub(clock.zone),
            )),
            false,
        )
    })
}

/// `-received` or `received`: the field by name, and whether the latest or last comes first.
fn order_of(sort: &str, ty: &TypeMeta) -> Result<Order, String> {
    let (name, descending) = match sort.strip_prefix('-') {
        Some(name) => (name, true),
        None => (sort, false),
    };
    let field = ty
        .fields
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| format!("`sort:{sort}`: {} has no field `{name}`", ty.name))?;
    let held = field.ty.trim_start_matches("Option<").trim_end_matches('>');
    if !matches!(
        held,
        "Date" | "Time" | "Int" | "Float" | "Text" | "std.Identifier"
    ) {
        return Err(format!(
            "`sort:{sort}`: `{name}` is {}, which has no order",
            field.ty
        ));
    }
    let kind = crate::meta::parse_ty(&field.ty, "")
        .map_or(crate::meta::FieldKind::Register, |parsed| {
            crate::meta::field_kind(field, &parsed)
        });
    Ok(Order {
        field: field.id.clone(),
        kind,
        descending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_search_reads_as_its_tokens() {
        let q = find("Thread from:*@ycombinator.com is:unread received:-7d sort:-received in:t91 \"83(b)\" ~\"Y C\" jane@x.com").unwrap();
        assert_eq!(q.ty.as_deref(), Some("Thread"));
        assert_eq!(
            q.terms,
            vec![
                Term::new("from", "*@ycombinator.com"),
                Term::new("unread", "true"),
                Term::new("received", "-7d"),
                Term {
                    key: None,
                    value: "jane@x.com".into()
                },
            ]
        );
        assert_eq!(q.sort.as_deref(), Some("-received"));
        assert_eq!(q.scope.as_deref(), Some("t91"));
        assert_eq!(q.text, ["83(b)", "Y C"]);
        assert!(find("from:").is_err());
        assert!(find("\"open").is_err());
    }
}
