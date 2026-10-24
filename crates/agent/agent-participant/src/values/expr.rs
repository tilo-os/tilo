//! The expressions a step's argument computes with (`{{= … }}`): numbers,
//! text, dates and lists of them, the arithmetic and functions a task's
//! figures need, and references to what documents hold as operands.
//!
//! It is evaluated as it is parsed: an expression is a few dozen
//! characters, and a tree would only be walked once.

use super::dates;

/// A value an expression computes with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Number(f64),
    Text(String),
    Date(chrono::NaiveDate),
    Bool(bool),
    List(Vec<Value>),
}

impl Value {
    /// `json` as a value: an array as the list of its non-empty elements,
    /// however nested (a range's rows).
    pub(crate) fn from_json(json: &serde_json::Value) -> Result<Self, String> {
        Ok(match json {
            serde_json::Value::Number(number) => {
                Self::Number(number.as_f64().ok_or("a number out of range")?)
            }
            serde_json::Value::String(text) => Self::Text(text.clone()),
            serde_json::Value::Bool(flag) => Self::Bool(*flag),
            serde_json::Value::Null => Self::Text(String::new()),
            serde_json::Value::Array(items) => {
                let mut flat = Vec::new();
                for item in items {
                    match Self::from_json(item)? {
                        Self::List(inner) => flat.extend(inner),
                        Self::Text(text) if text.trim().is_empty() => {}
                        one => flat.push(one),
                    }
                }
                Self::List(flat)
            }
            serde_json::Value::Object(_) => {
                return Err(
                    "an object, not a value: point the reference at one of its fields".into(),
                );
            }
        })
    }

    /// The value as an argument takes it: a whole number as an integer, a
    /// date as its ISO text.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Number(number) => number_json(*number),
            Self::Text(text) => serde_json::Value::String(text.clone()),
            Self::Date(days) => serde_json::Value::String(dates::iso(*days)),
            Self::Bool(flag) => serde_json::Value::Bool(*flag),
            Self::List(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_json).collect())
            }
        }
    }

    /// The value written into text.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Number(number) => number_text(*number),
            Self::Text(text) => text.clone(),
            Self::Date(days) => dates::iso(*days),
            Self::Bool(flag) => flag.to_string(),
            Self::List(items) => items.iter().map(Self::text).collect::<Vec<_>>().join(", "),
        }
    }

    pub(crate) fn number(&self) -> Result<f64, String> {
        match self {
            Self::Number(number) => Ok(*number),
            Self::Text(text) => {
                lenient_number(text).ok_or_else(|| format!("`{text}` is not a number"))
            }
            Self::Bool(flag) => Ok(if *flag { 1.0 } else { 0.0 }),
            Self::Date(_) => Err("a date is not a number".into()),
            Self::List(items) => match items.as_slice() {
                [one] => one.number(),
                _ => Err(format!(
                    "a list of {} values is not one number: use sum, avg, min or max",
                    items.len()
                )),
            },
        }
    }

    pub(crate) fn date(&self) -> Result<chrono::NaiveDate, String> {
        match self {
            Self::Date(days) => Ok(*days),
            Self::Text(text) => {
                dates::parse(text).ok_or_else(|| format!("`{text}` is not a date like 2026-10-23"))
            }
            Self::List(items) if items.len() == 1 => {
                items.first().map_or(Err(String::new()), Self::date)
            }
            other => Err(format!("{} is not a date", other.text())),
        }
    }

    fn is_date(&self) -> bool {
        match self {
            Self::Date(_) => true,
            Self::Text(text) => lenient_number(text).is_none() && dates::parse(text).is_some(),
            _ => false,
        }
    }

    /// The numbers a list function takes from `self`: every element of a
    /// list, blanks left out.
    fn numbers(&self, out: &mut Vec<f64>) -> Result<(), String> {
        match self {
            Self::List(items) => items.iter().try_for_each(|item| item.numbers(out)),
            Self::Text(text) if text.trim().is_empty() => Ok(()),
            one => {
                out.push(one.number()?);
                Ok(())
            }
        }
    }
}

/// `number` rounded to ten decimals, which drops binary noise
/// (`0.1 + 0.2`) a person never means.
fn tidy(number: f64) -> f64 {
    if number.abs() >= 1e15 {
        return number;
    }
    let scaled = (number * 1e10).round() / 1e10;
    if scaled == 0.0 { 0.0 } else { scaled }
}

fn number_json(number: f64) -> serde_json::Value {
    let number = tidy(number);
    if number.fract() == 0.0 && number.abs() < 9e15 {
        #[allow(clippy::cast_possible_truncation, reason = "a whole number below 2^53")]
        return serde_json::Value::from(number as i64);
    }
    serde_json::Number::from_f64(number).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// `number` as text: a whole number without a point, else its decimals
/// without trailing zeros.
pub(crate) fn number_text(number: f64) -> String {
    match number_json(number) {
        serde_json::Value::Number(number) => number.to_string(),
        _ => "NaN".into(),
    }
}

/// The number `text` spells as a person writes one: `1200`, `1,200.50`,
/// `$1,200`, `-€3`, `12%` (0.12), `(450)` (-450).
pub(crate) fn lenient_number(text: &str) -> Option<f64> {
    let mut text = text.trim();
    let mut sign = 1.0;
    if let Some(inner) = text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        sign = -1.0;
        text = inner.trim();
    }
    if let Some(rest) = text.strip_prefix('-') {
        sign = -sign;
        text = rest.trim_start();
    } else if let Some(rest) = text.strip_prefix('+') {
        text = rest.trim_start();
    }
    let text = text.trim_start_matches(['$', '€', '£', '¥']).trim();
    let (text, scale) = match text.strip_suffix('%') {
        Some(rest) => (rest.trim_end(), 0.01),
        None => (text, 1.0),
    };
    let digits: String = text.chars().filter(|c| *c != ',' && *c != '_').collect();
    if digits.is_empty()
        || !digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+')
    {
        return None;
    }
    let number: f64 = digits.parse().ok()?;
    number.is_finite().then_some(sign * number * scale)
}

/// Evaluate `source`, resolving each reference it names through `resolve`
/// (the reference's text, without the braces of `{…}`).
pub(crate) fn evaluate(
    source: &str,
    resolve: &mut dyn FnMut(&str) -> Result<Value, String>,
) -> Result<Value, String> {
    let mut parser = Parser {
        chars: source.chars().collect(),
        at: 0,
        resolve,
    };
    let value = parser.sum()?;
    parser.space();
    if parser.at < parser.chars.len() {
        return Err(format!(
            "unexpected `{}` at character {}",
            parser.rest(),
            parser.at.saturating_add(1)
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    resolve: &'a mut dyn FnMut(&str) -> Result<Value, String>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn rest(&self) -> String {
        self.chars
            .get(self.at..)
            .unwrap_or_default()
            .iter()
            .take(12)
            .collect()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at = self.at.saturating_add(1);
        }
    }

    fn eat(&mut self, wanted: char) -> bool {
        self.space();
        if self.peek() == Some(wanted) {
            self.at = self.at.saturating_add(1);
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> Result<Value, String> {
        let mut value = self.product()?;
        loop {
            if self.eat('+') {
                let right = self.product()?;
                value = add(&value, &right, 1.0)?;
            } else if self.eat('-') {
                let right = self.product()?;
                value = add(&value, &right, -1.0)?;
            } else {
                return Ok(value);
            }
        }
    }

    fn product(&mut self) -> Result<Value, String> {
        let mut value = self.unary()?;
        loop {
            if self.eat('*') {
                value = Value::Number(value.number()? * self.unary()?.number()?);
            } else if self.eat('/') {
                let divisor = self.unary()?.number()?;
                if divisor == 0.0 {
                    return Err("division by zero".into());
                }
                value = Value::Number(value.number()? / divisor);
            } else {
                return Ok(value);
            }
        }
    }

    fn unary(&mut self) -> Result<Value, String> {
        if self.eat('-') {
            return Ok(Value::Number(-self.unary()?.number()?));
        }
        if self.eat('+') {
            return self.unary();
        }
        let base = self.postfix()?;
        if self.eat('^') {
            let exponent = self.unary()?.number()?;
            return Ok(Value::Number(base.number()?.powf(exponent)));
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Value, String> {
        let mut value = self.primary()?;
        while self.eat('%') {
            value = Value::Number(value.number()? / 100.0);
        }
        Ok(value)
    }

    fn primary(&mut self) -> Result<Value, String> {
        self.space();
        match self.peek() {
            None => Err("the expression ends where a value was expected".into()),
            Some('(') => {
                self.at = self.at.saturating_add(1);
                let value = self.sum()?;
                if !self.eat(')') {
                    return Err("a `(` is never closed".into());
                }
                Ok(value)
            }
            Some('"') => self.quoted('"').map(Value::Text),
            Some('{') => {
                let reference = self.braced()?;
                (self.resolve)(&reference)
            }
            Some('\'') => {
                // A quoted sheet name: `'Q3 Budget'!C7`.
                let name = self.quoted('\'')?;
                let cell = self.cell_after_bang()?;
                (self.resolve)(&format!("'{name}'!{cell}"))
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_alphanumeric() || c == '_' => {
                let name = self.word();
                self.space();
                match self.peek() {
                    Some('(') => {
                        self.at = self.at.saturating_add(1);
                        let arguments = self.arguments()?;
                        call(&name, &arguments)
                    }
                    Some('!') => {
                        let cell = self.cell_after_bang()?;
                        (self.resolve)(&format!("{name}!{cell}"))
                    }
                    _ => match name.to_ascii_lowercase().as_str() {
                        "true" => Ok(Value::Bool(true)),
                        "false" => Ok(Value::Bool(false)),
                        _ => Err(format!(
                            "unknown name `{name}`: quote text as \"{name}\", or name a cell as Sheet1!B2"
                        )),
                    },
                }
            }
            Some(c) => Err(format!("unexpected `{c}`")),
        }
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            self.at = self.at.saturating_add(1);
        }
        self.chars
            .get(start..self.at)
            .unwrap_or_default()
            .iter()
            .collect()
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.at = self.at.saturating_add(1);
        }
        if self.peek().is_some_and(|c| c == 'e' || c == 'E') {
            self.at = self.at.saturating_add(1);
            if self.peek().is_some_and(|c| c == '-' || c == '+') {
                self.at = self.at.saturating_add(1);
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.at = self.at.saturating_add(1);
            }
        }
        let text: String = self
            .chars
            .get(start..self.at)
            .unwrap_or_default()
            .iter()
            .collect();
        text.parse()
            .map(Value::Number)
            .map_err(|_| format!("`{text}` is not a number"))
    }

    fn quoted(&mut self, quote: char) -> Result<String, String> {
        self.at = self.at.saturating_add(1);
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(format!("a {quote} is never closed")),
                Some(c) if c == quote => {
                    self.at = self.at.saturating_add(1);
                    // A doubled quote stands for itself.
                    if self.peek() == Some(quote) {
                        self.at = self.at.saturating_add(1);
                        text.push(quote);
                    } else {
                        return Ok(text);
                    }
                }
                Some(c) => {
                    self.at = self.at.saturating_add(1);
                    text.push(c);
                }
            }
        }
    }

    /// The text inside `{…}`, braces nested within it kept.
    fn braced(&mut self) -> Result<String, String> {
        self.at = self.at.saturating_add(1);
        let start = self.at;
        let mut depth = 0_usize;
        while let Some(c) = self.peek() {
            match c {
                '{' => depth = depth.saturating_add(1),
                '}' if depth == 0 => {
                    let text: String = self
                        .chars
                        .get(start..self.at)
                        .unwrap_or_default()
                        .iter()
                        .collect();
                    self.at = self.at.saturating_add(1);
                    return Ok(text
                        .trim()
                        .trim_start_matches('{')
                        .trim_end_matches('}')
                        .trim()
                        .to_owned());
                }
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.at = self.at.saturating_add(1);
        }
        Err("a `{` is never closed".into())
    }

    /// `!B2` or `!B2:C9` after a sheet's name, without the `!`.
    fn cell_after_bang(&mut self) -> Result<String, String> {
        if !self.eat('!') {
            return Err("a sheet's name is followed by !cell, as in Sheet1!B2".into());
        }
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == ':' || c == '$')
        {
            self.at = self.at.saturating_add(1);
        }
        let cell: String = self
            .chars
            .get(start..self.at)
            .unwrap_or_default()
            .iter()
            .collect();
        if cell.is_empty() {
            return Err("a cell like B2 follows the !".into());
        }
        Ok(cell)
    }

    fn arguments(&mut self) -> Result<Vec<Value>, String> {
        let mut arguments = Vec::new();
        if self.eat(')') {
            return Ok(arguments);
        }
        loop {
            arguments.push(self.sum()?);
            if self.eat(')') {
                return Ok(arguments);
            }
            if !self.eat(',') && !self.eat(';') {
                return Err("arguments are separated by commas and closed by `)`".into());
            }
        }
    }
}

/// `left + sign * right`: numbers add; a date moves by days; two dates
/// subtract to the days between them.
fn add(left: &Value, right: &Value, sign: f64) -> Result<Value, String> {
    if left.is_date() && right.is_date() && sign < 0.0 {
        return Ok(Value::Number(as_f64(dates::between(
            right.date()?,
            left.date()?,
        ))));
    }
    if left.is_date() {
        return moved(left.date()?, sign * right.number()?);
    }
    if right.is_date() && sign > 0.0 {
        return moved(right.date()?, left.number()?);
    }
    Ok(Value::Number(left.number()? + sign * right.number()?))
}

/// `date` moved by `count` days.
fn moved(date: chrono::NaiveDate, count: f64) -> Result<Value, String> {
    dates::add_days(date, days(count)?)
        .map(Value::Date)
        .ok_or_else(|| "a date out of range".to_owned())
}

#[allow(clippy::cast_precision_loss, reason = "a count of days")]
const fn as_f64(days: i64) -> f64 {
    days as f64
}

/// The date ranges `arguments` give as start and end in turn, each start
/// at or before its end; a list (a range of cells' rows) gives its dates
/// in the same order.
fn ranges(
    function: &str,
    arguments: &[Value],
) -> Result<Vec<(chrono::NaiveDate, chrono::NaiveDate)>, String> {
    fn flat(value: &Value, out: &mut Vec<chrono::NaiveDate>) -> Result<(), String> {
        match value {
            Value::List(items) => items.iter().try_for_each(|item| flat(item, out)),
            one => {
                out.push(one.date()?);
                Ok(())
            }
        }
    }
    let mut found = Vec::new();
    for argument in arguments {
        flat(argument, &mut found)?;
    }
    if found.is_empty() || found.len() % 2 != 0 {
        return Err(format!(
            "{function} takes ranges as pairs of dates, start then end; it was given {}",
            found.len()
        ));
    }
    found
        .chunks(2)
        .map(|pair| match pair {
            [start, end] if start <= end => Ok((*start, *end)),
            [start, end] => Err(format!(
                "{function}: the range from {} ends before it starts, on {}",
                dates::iso(*start),
                dates::iso(*end)
            )),
            _ => Err(format!("{function} takes ranges as pairs of dates")),
        })
        .collect()
}

/// A whole count, of days or of decimal places.
fn days(number: f64) -> Result<i64, String> {
    if number.fract() != 0.0 || number.abs() > 1e7 {
        return Err(format!(
            "{} is not a whole number of days",
            number_text(number)
        ));
    }
    #[allow(clippy::cast_possible_truncation, reason = "checked whole and small")]
    Ok(number as i64)
}

fn call(name: &str, arguments: &[Value]) -> Result<Value, String> {
    let lower = name.to_ascii_lowercase();
    if let Some(date) = date_call(&lower, arguments)? {
        return Ok(date);
    }
    let numbers = || -> Result<Vec<f64>, String> {
        let mut out = Vec::new();
        for argument in arguments {
            argument.numbers(&mut out)?;
        }
        Ok(out)
    };
    let one = |at: usize| {
        arguments
            .get(at)
            .ok_or_else(|| format!("{lower} takes {} arguments", at.saturating_add(1)))
    };
    let exactly = |count: usize| {
        if arguments.len() == count {
            Ok(())
        } else {
            Err(format!(
                "{lower} takes {count} argument{}",
                if count == 1 { "" } else { "s" }
            ))
        }
    };
    Ok(match lower.as_str() {
        "sum" => Value::Number(numbers()?.iter().sum()),
        "avg" | "average" | "mean" => {
            let numbers = numbers()?;
            if numbers.is_empty() {
                return Err(format!("{lower} of no values"));
            }
            #[allow(clippy::cast_precision_loss, reason = "a count of values")]
            let count = numbers.len() as f64;
            Value::Number(numbers.iter().sum::<f64>() / count)
        }
        "min" | "max" => {
            let numbers = numbers()?;
            let pick = if lower == "min" { f64::min } else { f64::max };
            Value::Number(
                numbers
                    .into_iter()
                    .reduce(pick)
                    .ok_or_else(|| format!("{lower} of no values"))?,
            )
        }
        "count" => {
            #[allow(clippy::cast_precision_loss, reason = "a count of values")]
            let count = numbers()?.len() as f64;
            Value::Number(count)
        }
        "round" | "roundup" | "rounddown" => {
            let places = match arguments.get(1) {
                Some(places) => days(places.number()?)?,
                None => 0,
            };
            let number = one(0)?.number()?;
            let scale = 10_f64.powi(i32::try_from(places).map_err(|_| "too many places")?);
            let scaled = tidy(number * scale);
            let rounded = match lower.as_str() {
                "roundup" => scaled.abs().ceil() * scaled.signum(),
                "rounddown" => scaled.trunc(),
                // Half away from zero, as a spreadsheet and a person round.
                _ => scaled.round(),
            };
            Value::Number(rounded / scale)
        }
        "abs" => {
            exactly(1)?;
            Value::Number(one(0)?.number()?.abs())
        }
        "floor" | "ceil" | "ceiling" => {
            exactly(1)?;
            let number = one(0)?.number()?;
            Value::Number(if lower == "floor" {
                number.floor()
            } else {
                number.ceil()
            })
        }
        "percent" | "pct" => {
            exactly(2)?;
            let whole = one(1)?.number()?;
            if whole == 0.0 {
                return Err("percent of a whole of 0".into());
            }
            Value::Number(one(0)?.number()? / whole)
        }
        _ => {
            return Err(format!(
                "unknown function `{name}`: use sum, avg, min, max, count, round, abs, floor, \
                 ceil, percent, date, today, add_days, workday, add_months, add_years, \
                 days_between, networkdays, days_inclusive, days_exclusive or days_within"
            ));
        }
    })
}

/// The date functions; `None` for a name that is not one.
fn date_call(lower: &str, arguments: &[Value]) -> Result<Option<Value>, String> {
    let one = |at: usize| {
        arguments
            .get(at)
            .ok_or_else(|| format!("{lower} takes {} arguments", at.saturating_add(1)))
    };
    let exactly = |count: usize| {
        if arguments.len() == count {
            Ok(())
        } else {
            Err(format!(
                "{lower} takes {count} argument{}",
                if count == 1 { "" } else { "s" }
            ))
        }
    };
    Ok(Some(match lower {
        "date" => match arguments {
            [text] => Value::Date(text.date()?),
            [year, month, day] => Value::Date(
                dates::from_parts(
                    days(year.number()?)?,
                    days(month.number()?)?,
                    days(day.number()?)?,
                )
                .ok_or("no such date")?,
            ),
            _ => return Err("date takes an ISO date, or a year, month and day".into()),
        },
        "today" => {
            exactly(0)?;
            Value::Date(dates::today())
        }
        "add_days" | "adddays" => {
            exactly(2)?;
            moved(one(0)?.date()?, one(1)?.number()?)?
        }
        "add_business_days" | "addbusinessdays" | "workday" | "business_days" => {
            exactly(2)?;
            Value::Date(
                dates::add_business_days(one(0)?.date()?, days(one(1)?.number()?)?)
                    .ok_or("a date out of range")?,
            )
        }
        "days_between" | "daysbetween" | "days" => {
            exactly(2)?;
            Value::Number(as_f64(dates::between(one(0)?.date()?, one(1)?.date()?)))
        }
        "add_months" | "addmonths" | "edate" => {
            exactly(2)?;
            Value::Date(
                dates::add_months(one(0)?.date()?, days(one(1)?.number()?)?)
                    .ok_or("a date out of range")?,
            )
        }
        "add_years" | "addyears" => {
            exactly(2)?;
            let years = days(one(1)?.number()?)?;
            Value::Date(
                dates::add_months(one(0)?.date()?, years.saturating_mul(12))
                    .ok_or("a date out of range")?,
            )
        }
        "business_days_between" | "networkdays" => {
            exactly(2)?;
            Value::Number(as_f64(dates::business_days_between(
                one(0)?.date()?,
                one(1)?.date()?,
            )))
        }
        _ => return range_call(lower, arguments),
    }))
}

/// The functions totalling days over ranges of dates; `None` for a name
/// that is not one.
fn range_call(lower: &str, arguments: &[Value]) -> Result<Option<Value>, String> {
    let first = |at: usize| {
        arguments
            .get(at)
            .ok_or_else(|| format!("{lower} takes a window, then ranges"))
    };
    Ok(Some(match lower {
        "days_inclusive" | "days_exclusive" => {
            let ranges = ranges(lower, arguments)?;
            let extra = i64::from(lower == "days_inclusive");
            Value::Number(as_f64(ranges.iter().try_fold(
                0_i64,
                |total, (start, end)| {
                    total
                        .checked_add(dates::between(*start, *end).saturating_add(extra))
                        .ok_or("too many days")
                },
            )?))
        }
        "days_within" => {
            let (from, to) = (first(0)?.date()?, first(1)?.date()?);
            if to < from {
                return Err(format!(
                    "days_within takes its window first, earliest first: {} is after {}",
                    dates::iso(from),
                    dates::iso(to)
                ));
            }
            let ranges = ranges(lower, arguments.get(2..).unwrap_or_default())?;
            Value::Number(as_f64(ranges.iter().fold(0_i64, |total, (start, end)| {
                let (start, end) = ((*start).max(from), (*end).min(to));
                if end < start {
                    total
                } else {
                    total.saturating_add(dates::between(start, end).saturating_add(1))
                }
            })))
        }
        _ => return Ok(None),
    }))
}
