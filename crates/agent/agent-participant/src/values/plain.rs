//! Arithmetic written as a person says it, for an argument that takes a
//! date or a number: `10 business days after 2026-10-14`, `2026-10-14 + 10
//! business days`, `289 - 50`, `4 x 38.50`, `sum of 120, 80 and 45`, `15%
//! of 200`. The host computes it before the step runs, as it computes
//! `{{= … }}`, so a model that would not reach for that syntax still never
//! counts days or subtracts figures itself.
//!
//! Strict on purpose: a date is ISO, a written month (`14 October 2026`)
//! or numbers only one order reads as a date; `03/04/2026` could be either,
//! and is refused with both readings rather than guessed. Anything else
//! not read whole is refused with the forms that are.

use chrono::NaiveDate;

use super::dates;
use super::expr::{Value, lenient_number};

/// The forms a date argument takes, for a refusal to show.
const DATE_FORMS: &str = "write the date as ISO (2026-10-14), or as arithmetic the host \
     computes: 10 business days after 2026-10-14, 2026-10-14 + 10 business days, 3 weeks \
     before 2026-10-14, 2026-10-14 + 1 month";

/// The forms a number argument takes, for a refusal to show.
const NUMBER_FORMS: &str = "write a number (239), or arithmetic the host computes: 289 - 50, \
     4 x 38.50, (120 + 80) / 2, sum of 120, 80 and 45, average of 3412.50, 3412.50 and \
     3980, 15% of 200";

/// What an argument takes, by its type or schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wants {
    /// A date; `time` when a date and time (a time the text gives is kept).
    Date { time: bool },
    /// A number; `whole` when an integer.
    Number { whole: bool },
}

/// What `param` takes, when that is a date or a number.
pub(crate) fn param_wants(param: &tilo_actions::ActionParam) -> Option<Wants> {
    use tilo_actions::ParamType;
    match param.ty {
        ParamType::DateTime => Some(Wants::Date { time: true }),
        ParamType::Number => Some(Wants::Number { whole: false }),
        ParamType::Integer => Some(Wants::Number { whole: true }),
        _ => param.schema.as_deref().and_then(schema_wants),
    }
}

/// What a value `schema` describes takes, when that is a date (`format:
/// date` or `date-time`, or an ISO date's pattern) or a number.
pub(crate) fn schema_wants(schema: &serde_json::Value) -> Option<Wants> {
    let kinds: Vec<&str> = match schema.get("type") {
        Some(serde_json::Value::String(kind)) => vec![kind.as_str()],
        Some(serde_json::Value::Array(kinds)) => kinds
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|kind| *kind != "null")
            .collect(),
        _ => return None,
    };
    match kinds.as_slice() {
        ["number"] => Some(Wants::Number { whole: false }),
        ["integer"] => Some(Wants::Number { whole: true }),
        ["string"] => {
            match schema.get("format").and_then(serde_json::Value::as_str) {
                Some("date") => return Some(Wants::Date { time: false }),
                Some("date-time") => return Some(Wants::Date { time: true }),
                _ => {}
            }
            let pattern = schema.get("pattern").and_then(serde_json::Value::as_str)?;
            let rest = pattern.strip_prefix(r"^\d{4}-\d{2}-\d{2}")?;
            Some(Wants::Date {
                time: !rest.starts_with('$'),
            })
        }
        _ => None,
    }
}

/// What a place in a step's arguments takes, as its parameter's schema
/// says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// A date or a number.
    Typed(Wants),
    /// Anything: nothing types it (a cell, a record's field by its header).
    Untyped,
}

/// Fill in what `arguments` write as arithmetic where `params` take a date
/// or a number, at the top level and in the fields and items their schemas
/// type.
///
/// # Errors
///
/// The argument whose text is neither a plain value nor arithmetic read
/// whole, named with the forms that are.
pub(crate) fn fill(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    params: &[tilo_actions::ActionParam],
) -> Result<(), String> {
    visit_arguments(arguments, params, &mut |value, place| match place {
        Place::Typed(wants) => fill_leaf(value, wants),
        Place::Untyped => Ok(()),
    })
}

/// Fill in each value of `arguments` that nothing types and that is only
/// arithmetic ([`untyped`]): `10 business days after 2026-10-14` in a
/// row's cell becomes the date, `289 - 50` the number. Anything else is
/// left as written.
pub(crate) fn fill_untyped(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    params: &[tilo_actions::ActionParam],
) {
    let filled = visit_arguments(arguments, params, &mut |value, place| {
        if place == Place::Untyped
            && let serde_json::Value::String(text) = value
            && let Some(computed) = untyped(text)
        {
            *value = computed;
        }
        Ok(())
    });
    debug_assert!(filled.is_ok(), "filling untyped values refuses nothing");
}

/// Call `act` on each place of `arguments` that `params` type as a date or
/// a number, and on each string nothing types. A parameter the action
/// does not declare, and text a parameter takes, are left alone.
fn visit_arguments(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    params: &[tilo_actions::ActionParam],
    act: &mut impl FnMut(&mut serde_json::Value, Place) -> Result<(), String>,
) -> Result<(), String> {
    use tilo_actions::ParamType;
    for (name, value) in arguments.iter_mut() {
        let Some(param) = params.iter().find(|param| param.name.as_ref() == name) else {
            continue;
        };
        let named = |why: String| format!("`{name}`: {why}");
        if let Some(wants) = param_wants(param) {
            act(value, Place::Typed(wants)).map_err(named)?;
        } else if let Some(schema) = param.schema.as_deref() {
            visit(value, Some(schema), act).map_err(named)?;
        } else if matches!(param.ty, ParamType::Object | ParamType::Array) {
            visit(value, None, act).map_err(named)?;
        }
    }
    Ok(())
}

/// Call `act` on the places within `value` that `schema` (`None`: no
/// schema at all) types as a date or a number, and on the strings it
/// leaves untyped.
fn visit(
    value: &mut serde_json::Value,
    schema: Option<&serde_json::Value>,
    act: &mut impl FnMut(&mut serde_json::Value, Place) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(schema) = schema {
        if let Some(wants) = schema_wants(schema) {
            return act(value, Place::Typed(wants));
        }
        // A type defined elsewhere, or one of several: what its places
        // take is not read here, so none of them counts as untyped.
        if ["$ref", "anyOf", "oneOf", "allOf"]
            .iter()
            .any(|key| schema.get(key).is_some())
        {
            return Ok(());
        }
    }
    match value {
        serde_json::Value::String(_) if schema.is_none_or(says_nothing) => {
            act(value, Place::Untyped)
        }
        serde_json::Value::Object(fields) => {
            for (name, field) in fields.iter_mut() {
                let inner = schema.and_then(|schema| {
                    schema
                        .get("properties")
                        .and_then(|properties| properties.get(name))
                        .or_else(|| {
                            schema
                                .get("additionalProperties")
                                .filter(|inner| inner.is_object())
                        })
                });
                visit(field, inner, act).map_err(|why| format!("{name}: {why}"))?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let inner = schema
                .and_then(|schema| schema.get("items"))
                .filter(|inner| inner.is_object());
            items
                .iter_mut()
                .try_for_each(|item| visit(item, inner, act))
        }
        _ => Ok(()),
    }
}

/// Whether `schema` says nothing of what its value is (`true`, `{}`, a
/// description alone).
fn says_nothing(schema: &serde_json::Value) -> bool {
    match schema {
        serde_json::Value::Bool(any) => *any,
        serde_json::Value::Object(fields) => !["type", "enum", "const", "format", "pattern"]
            .iter()
            .any(|key| fields.contains_key(*key)),
        _ => false,
    }
}

fn fill_leaf(value: &mut serde_json::Value, wants: Wants) -> Result<(), String> {
    if let serde_json::Value::String(text) = value
        && let Some(filled) = typed(text, wants)?
    {
        *value = filled;
    }
    Ok(())
}

/// `text`, given where `wants` is taken, as the value it computes: `None`
/// when it is a plain value already (left for the app to check).
///
/// # Errors
///
/// Text that is no plain value and no arithmetic read whole.
pub(crate) fn typed(text: &str, wants: Wants) -> Result<Option<serde_json::Value>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match wants {
        Wants::Number { whole } => {
            if lenient_number(trimmed).is_some() {
                return Ok(None);
            }
            let Value::Number(number) = evaluate(trimmed)? else {
                return Err(format!("`{trimmed}` is not a number: {NUMBER_FORMS}"));
            };
            if whole && number.fract() != 0.0 {
                return Err(format!(
                    "`{trimmed}` comes to {}, not a whole number",
                    super::expr::number_text(number)
                ));
            }
            Ok(Some(Value::Number(number).to_json()))
        }
        Wants::Date { time } => {
            // ISO, with at most a time after it: `dates::parse` alone
            // would drop what follows a space.
            if dates::parse(trimmed).is_some() && dates::instant(trimmed).is_some() {
                return Ok(None);
            }
            let tokens = tokens(trimmed, Reading::Lenient)?;
            let Some((day, clock)) = date_form(&tokens) else {
                return Err(format!("`{trimmed}` is not a date: {DATE_FORMS}"));
            };
            let mut iso = dates::iso(day);
            if time && let Some(clock) = clock {
                iso.push_str(&clock);
            }
            Ok(Some(serde_json::Value::String(iso)))
        }
    }
}

/// The value plain arithmetic `text` computes, a date or a number.
///
/// # Errors
///
/// Text not read whole as one of the forms, with the forms.
pub(crate) fn evaluate(text: &str) -> Result<Value, String> {
    let tokens = tokens(text, Reading::Lenient)?;
    if let Some((day, _)) = date_form(&tokens) {
        return Ok(Value::Date(day));
    }
    number_form(&tokens).map(Value::Number).ok_or_else(|| {
        format!(
            "`{}` is not read: {NUMBER_FORMS}; or {DATE_FORMS}",
            text.trim()
        )
    })
}

/// `text`, a value nothing types, as what it computes when it is nothing
/// but arithmetic: a date moved by days, weeks or months (`10 business
/// days after 2026-10-14`, `2026-10-14 + 3 weeks`) as its ISO date, and
/// numbers worked (`289 - 50`, `4 x 38.50`, `sum of 120, 80 and 45`, `15%
/// of 200`) as a number. `None` for anything else, left as written: text
/// that says more than the arithmetic (`Call 319 - 75 today`), a plain
/// value, and what is read only [`Reading::Strict`]ly.
pub(crate) fn untyped(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if !trimmed.contains(|c: char| c.is_ascii_digit()) || lenient_number(trimmed).is_some() {
        return None;
    }
    let tokens = tokens(trimmed, Reading::Strict).ok()?;
    if tokens
        .iter()
        .any(|token| *token == Token::Word("today".into()))
    {
        return None;
    }
    if let Some((day, clock)) = date_form(&tokens) {
        // A date alone is what it says already.
        if tokens.len() == 1 {
            return None;
        }
        let mut iso = dates::iso(day);
        iso.push_str(clock.as_deref().unwrap_or_default());
        return Some(serde_json::Value::String(iso));
    }
    number_form(&tokens).map(|number| Value::Number(number).to_json())
}

/// How strictly text is read as arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// Where a parameter takes a date or a number: a date may be written
    /// with its month by name, or in numbers only one order reads.
    Lenient,
    /// Where nothing types the value, so text that only looks like
    /// arithmetic must stay text: dates are ISO, and each run of digits
    /// is one number or date whole, so `555-1234`, `1920x1080`, `3/4` and
    /// `03/04/2026` are not sums.
    Strict,
}

/// One piece of plain arithmetic.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    /// A date, and the time written after it (`T09:00`), kept as written.
    Date(NaiveDate, Option<String>),
    /// `+ - * / % ( ) ,`
    Mark(char),
    /// A word, lower case.
    Word(String),
}

/// Currency marks a number may carry, which say nothing of its value.
const CURRENCY_WORDS: [&str; 10] = [
    "eur", "usd", "gbp", "chf", "jpy", "cad", "aud", "sek", "nok", "dkk",
];

/// `text` as tokens: dates (ISO; read [`Reading::Lenient`]ly, numbers one
/// order reads and a month by name too), numbers (currency marks left
/// out, thousands grouped by commas), marks and words.
///
/// # Errors
///
/// A date whose day and month could be either way, or a decimal comma;
/// read strictly, a run of digits that is not one number or date whole.
fn tokens(text: &str, reading: Reading) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        if c.is_whitespace() || matches!(c, '$' | '€' | '£' | '¥') {
            step(&mut at, 1);
        } else if c.is_ascii_digit()
            || (c == '.'
                && chars
                    .get(at.saturating_add(1))
                    .is_some_and(char::is_ascii_digit))
        {
            let end = run_end(&chars, at, |c| {
                c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '.' | ',' | ':' | '+')
            });
            let word: String = chars.get(at..end).unwrap_or_default().iter().collect();
            let (token, used) = number_or_date(&word, reading)?;
            if reading == Reading::Strict
                && !word
                    .get(used..)
                    .is_some_and(|rest| rest.trim_end_matches(',').is_empty())
            {
                return Err(format!("`{word}` is not one number or date"));
            }
            out.push(token);
            step(&mut at, used.max(1));
        } else if c.is_alphabetic() {
            let end = run_end(&chars, at, char::is_alphabetic);
            let word: String = chars.get(at..end).unwrap_or_default().iter().collect();
            let word = word.to_lowercase();
            if !CURRENCY_WORDS.contains(&word.as_str()) {
                out.push(match word.as_str() {
                    "x" | "times" => Token::Mark('*'),
                    "divided" => Token::Mark('/'),
                    "by" if out.last() == Some(&Token::Mark('/')) => {
                        at = end;
                        continue;
                    }
                    "plus" => Token::Mark('+'),
                    "minus" | "less" => Token::Mark('-'),
                    _ => Token::Word(word),
                });
            }
            at = end;
        } else {
            out.push(Token::Mark(match c {
                '×' => '*',
                '÷' => '/',
                '−' | '–' => '-',
                other => other,
            }));
            step(&mut at, 1);
        }
    }
    Ok(match reading {
        Reading::Lenient => join_written_dates(&out),
        Reading::Strict => out,
    })
}

/// `at` moved on by `by`.
fn step(at: &mut usize, by: usize) {
    *at = at.saturating_add(by);
}

fn run_end(chars: &[char], from: usize, keep: impl Fn(char) -> bool) -> usize {
    let mut end = from;
    while chars.get(end).is_some_and(|c| keep(*c)) {
        step(&mut end, 1);
    }
    end
}

/// The token `word` (a run of digits and what joins them) starts with and
/// how many of its characters it takes.
fn number_or_date(word: &str, reading: Reading) -> Result<(Token, usize), String> {
    // An ISO date, with a time after it kept as written.
    if let Some(date) = word.get(..10).and_then(dates::parse) {
        let rest = word.get(10..).unwrap_or_default();
        if rest.is_empty() {
            return Ok((Token::Date(date, None), 10));
        }
        if rest.starts_with(['T', 't']) {
            return Ok((Token::Date(date, Some(rest.to_owned())), word.len()));
        }
    }
    let date_end = word.find(|c: char| !(c.is_ascii_digit() || matches!(c, '/' | '.' | '-')));
    let date_part = word.get(..date_end.unwrap_or(word.len())).unwrap_or(word);
    let date_part = date_part.trim_end_matches(['.', '-']);
    if reading == Reading::Lenient
        && let Some((numeric, _)) = dates::numeric_date(date_part)
    {
        return match numeric {
            dates::Numeric::Settled(day) => Ok((Token::Date(day, None), date_part.len())),
            dates::Numeric::Either {
                day_first,
                month_first,
            } => Err(format!(
                "`{date_part}` could be {} or {}: write the date as ISO",
                dates::iso(day_first),
                dates::iso(month_first)
            )),
        };
    }
    // A number: digits, commas grouping thousands, a decimal point.
    let bytes = word.as_bytes();
    let digits_from = |from: usize| {
        bytes
            .get(from..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let mut end = digits_from(0);
    let mut text: String = word.get(..end).unwrap_or_default().to_owned();
    while bytes.get(end) == Some(&b',')
        && bytes
            .get(end.saturating_add(1))
            .is_some_and(u8::is_ascii_digit)
    {
        let group = digits_from(end.saturating_add(1));
        if group != 3 || text.is_empty() {
            return Err(format!(
                "`{word}`: write decimals with a point (38.50) and group thousands by threes"
            ));
        }
        text.push_str(
            word.get(end.saturating_add(1)..end.saturating_add(4))
                .unwrap_or_default(),
        );
        step(&mut end, 4);
    }
    if bytes.get(end) == Some(&b'.')
        && bytes
            .get(end.saturating_add(1))
            .is_some_and(u8::is_ascii_digit)
    {
        let decimals = digits_from(end.saturating_add(1));
        text.push_str(
            word.get(end..end.saturating_add(1).saturating_add(decimals))
                .unwrap_or_default(),
        );
        step(&mut end, decimals.saturating_add(1));
    }
    // An ordinal day, `14th`.
    let ordinal = ["st", "nd", "rd", "th"]
        .iter()
        .find(|suffix| word.get(end..end.saturating_add(2)) == Some(**suffix));
    if ordinal.is_some() && !text.contains('.') {
        let number = text
            .parse()
            .map_err(|_| format!("`{word}` is not a number"))?;
        return Ok((Token::Number(number), end.saturating_add(2)));
    }
    let number = text
        .parse()
        .map_err(|_| format!("`{word}` is not a number"))?;
    Ok((Token::Number(number), end))
}

/// `tokens` with each date written with its month by name (`14 October
/// 2026`, `October 14, 2026`, `14th of Oct 2026`) made one date.
fn join_written_dates(tokens: &[Token]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut at = 0;
    while at < tokens.len() {
        if let Some((date, used)) = written_at(tokens.get(at..).unwrap_or_default()) {
            out.push(Token::Date(date, None));
            step(&mut at, used);
        } else if let Some(token) = tokens.get(at) {
            out.push(token.clone());
            step(&mut at, 1);
        }
    }
    out
}

/// The date written with its month by name at the start of `tokens`, and
/// how many tokens it takes.
fn written_at(tokens: &[Token]) -> Option<(NaiveDate, usize)> {
    let month = |token: &Token| match token {
        Token::Word(word) => dates::month_number(word),
        _ => None,
    };
    let whole = |token: &Token| match token {
        Token::Number(number) if number.fract() == 0.0 && *number >= 1.0 =>
        {
            #[allow(clippy::cast_possible_truncation, reason = "whole and checked small")]
            Some(*number as i64).filter(|number| *number < 10_000)
        }
        _ => None,
    };
    // Day first: `14 October 2026`, `14 of October 2026`.
    if let Some(day) = tokens.first().and_then(whole).filter(|day| *day <= 31) {
        let skip = usize::from(matches!(tokens.get(1), Some(Token::Word(word)) if word == "of"));
        if let Some(month) = tokens.get(skip.saturating_add(1)).and_then(month)
            && let Some(year) = tokens
                .get(skip.saturating_add(2))
                .and_then(whole)
                .filter(|year| *year >= 1000)
        {
            return dates::from_parts(year, month, day).map(|date| (date, skip.saturating_add(3)));
        }
    }
    // Month first: `October 14, 2026`.
    let month = tokens.first().and_then(month)?;
    let day = tokens.get(1).and_then(whole).filter(|day| *day <= 31)?;
    let comma = usize::from(tokens.get(2) == Some(&Token::Mark(',')));
    let year = tokens
        .get(comma.saturating_add(2))
        .and_then(whole)
        .filter(|year| *year >= 1000)?;
    dates::from_parts(year, month, day).map(|date| (date, comma.saturating_add(3)))
}

/// How a count of days moves a date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Days,
    BusinessDays,
    Weeks,
    Months,
    Years,
}

/// The unit `words` start with, and how many words it takes: `business
/// days`, `working days`, `calendar days`, `days`, `weeks`, `months`.
fn unit(tokens: &[Token]) -> Option<(Unit, usize)> {
    let word = |at: usize| match tokens.get(at) {
        Some(Token::Word(word)) => Some(word.as_str()),
        _ => None,
    };
    let single = |word: &str| match word.trim_end_matches('s') {
        "day" => Some(Unit::Days),
        "workday" | "weekday" => Some(Unit::BusinessDays),
        "week" => Some(Unit::Weeks),
        "month" => Some(Unit::Months),
        "year" => Some(Unit::Years),
        _ => None,
    };
    match (word(0)?, word(1)) {
        ("business" | "working" | "work", Some(next)) if single(next) == Some(Unit::Days) => {
            Some((Unit::BusinessDays, 2))
        }
        ("calendar", Some(next)) if single(next) == Some(Unit::Days) => Some((Unit::Days, 2)),
        (first, _) => single(first).map(|unit| (unit, 1)),
    }
}

/// The count `token` is, in words or digits: whole only.
fn count(token: &Token) -> Option<i64> {
    const WORDS: [&str; 21] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    match token {
        Token::Number(number) if number.fract() == 0.0 && number.abs() < 100_000.0 =>
        {
            #[allow(clippy::cast_possible_truncation, reason = "whole and checked small")]
            Some(*number as i64)
        }
        Token::Word(word) if word == "a" || word == "an" => Some(1),
        Token::Word(word) => WORDS
            .iter()
            .position(|name| name == word)
            .and_then(|at| i64::try_from(at).ok()),
        _ => None,
    }
}

/// `date` moved by `count` of `unit`.
fn moved(date: NaiveDate, count: i64, unit: Unit) -> Option<NaiveDate> {
    match unit {
        Unit::Days => dates::add_days(date, count),
        Unit::BusinessDays => dates::add_business_days(date, count),
        Unit::Weeks => dates::add_days(date, count.checked_mul(7)?),
        Unit::Months => dates::add_months(date, count),
        Unit::Years => dates::add_months(date, count.checked_mul(12)?),
    }
}

/// The date `tokens` are, whole: a date alone, `<n> <unit> after|before
/// <date>`, or `<date> +|- <n> <unit>`; with the time written after the
/// date.
fn date_form(tokens: &[Token]) -> Option<(NaiveDate, Option<String>)> {
    let date = |token: Option<&Token>| match token {
        Some(Token::Date(date, clock)) => Some((*date, clock.clone())),
        Some(Token::Word(word)) if word == "today" => Some((dates::today(), None)),
        _ => None,
    };
    if let [one] = tokens {
        return date(Some(one));
    }
    // `<date> + <n> <unit>`.
    if let Some((day, clock)) = date(tokens.first()) {
        let sign: i64 = match tokens.get(1)? {
            Token::Mark('+') => 1,
            Token::Mark('-') => -1,
            _ => return None,
        };
        let count = count(tokens.get(2)?)?;
        let (unit, used) = unit(tokens.get(3..)?)?;
        return (tokens.len() == used.saturating_add(3))
            .then(|| moved(day, sign.saturating_mul(count), unit))
            .flatten()
            .map(|day| (day, clock));
    }
    // `<n> <unit> after <date>`.
    let count = count(tokens.first()?)?;
    let (unit, used) = unit(tokens.get(1..)?)?;
    let sign: i64 = match tokens.get(used.saturating_add(1))? {
        Token::Word(word) if matches!(word.as_str(), "after" | "from" | "past") => 1,
        Token::Word(word) if matches!(word.as_str(), "before" | "prior") => -1,
        _ => return None,
    };
    let rest = tokens.get(used.saturating_add(2)..)?;
    let rest = match rest.first() {
        Some(Token::Word(word)) if word == "to" || word == "the" => rest.get(1..)?,
        _ => rest,
    };
    let [last] = rest else { return None };
    let (day, clock) = date(Some(last))?;
    moved(day, sign.saturating_mul(count), unit).map(|day| (day, clock))
}

/// The number `tokens` compute, whole: `sum of a, b and c` (or total,
/// average, mean, min, max), `<p>% of <x>`, or arithmetic.
fn number_form(tokens: &[Token]) -> Option<f64> {
    if let [Token::Word(function), Token::Word(of), rest @ ..] = tokens
        && of == "of"
    {
        let items: Vec<f64> = rest
            .split(|token| matches!(token, Token::Mark(',')) || *token == Token::Word("and".into()))
            .filter(|part| !part.is_empty())
            .map(arithmetic)
            .collect::<Option<_>>()?;
        if items.is_empty() {
            return None;
        }
        let sum: f64 = items.iter().sum();
        #[allow(clippy::cast_precision_loss, reason = "a count of values")]
        let count = items.len() as f64;
        return match function.as_str() {
            "sum" | "total" => Some(sum),
            "average" | "mean" => Some(sum / count),
            "min" | "minimum" | "lowest" => items.into_iter().reduce(f64::min),
            "max" | "maximum" | "highest" => items.into_iter().reduce(f64::max),
            _ => None,
        };
    }
    if let [
        Token::Number(percent),
        Token::Mark('%'),
        Token::Word(of),
        rest @ ..,
    ] = tokens
        && of == "of"
    {
        return arithmetic(rest).map(|whole| whole * percent / 100.0);
    }
    // Arithmetic with no operator is a number written oddly, not a sum.
    let operates = tokens
        .iter()
        .any(|token| matches!(token, Token::Mark('+' | '-' | '*' | '/')));
    if !operates {
        return None;
    }
    arithmetic(tokens)
}

/// `tokens` as arithmetic, whole: numbers, `+ - * /`, a `%` after a
/// number, parentheses.
fn arithmetic(tokens: &[Token]) -> Option<f64> {
    let mut at = 0;
    let value = sum(tokens, &mut at)?;
    (at == tokens.len() && value.is_finite()).then_some(value)
}

fn sum(tokens: &[Token], at: &mut usize) -> Option<f64> {
    let mut value = product(tokens, at)?;
    loop {
        match tokens.get(*at) {
            Some(Token::Mark('+')) => {
                step(at, 1);
                value += product(tokens, at)?;
            }
            Some(Token::Mark('-')) => {
                step(at, 1);
                value -= product(tokens, at)?;
            }
            _ => return Some(value),
        }
    }
}

fn product(tokens: &[Token], at: &mut usize) -> Option<f64> {
    let mut value = factor(tokens, at)?;
    loop {
        match tokens.get(*at) {
            Some(Token::Mark('*')) => {
                step(at, 1);
                value *= factor(tokens, at)?;
            }
            Some(Token::Mark('/')) => {
                step(at, 1);
                let divisor = factor(tokens, at)?;
                if divisor == 0.0 {
                    return None;
                }
                value /= divisor;
            }
            _ => return Some(value),
        }
    }
}

fn factor(tokens: &[Token], at: &mut usize) -> Option<f64> {
    let value = match tokens.get(*at)? {
        Token::Mark('-') => {
            step(at, 1);
            return factor(tokens, at).map(|value| -value);
        }
        Token::Mark('(') => {
            step(at, 1);
            let value = sum(tokens, at)?;
            if tokens.get(*at) != Some(&Token::Mark(')')) {
                return None;
            }
            step(at, 1);
            value
        }
        Token::Number(number) => {
            step(at, 1);
            *number
        }
        _ => return None,
    };
    if tokens.get(*at) == Some(&Token::Mark('%')) {
        step(at, 1);
        return Some(value / 100.0);
    }
    Some(value)
}
