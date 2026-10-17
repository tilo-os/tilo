//! The Python behaviours the reclaim decisions were defined with: what `int()`
//! and `str()` make of a JSON value, what `==` means between numbers, and how
//! a command line reads in a `CalledProcessError`.

use serde_json::Value;

/// `type(value) is int`: a JSON integer, never a boolean or a float.
#[must_use]
pub fn as_int(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    }
}

/// `value == number` in Python, where `True == 1` and `1.0 == 1`.
#[must_use]
pub fn equals_number(value: &Value, number: i64) -> bool {
    match value {
        Value::Bool(flag) => i64::from(*flag) == number,
        Value::Number(n) => as_int(value).map_or_else(
            || {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "the numbers compared are small constants"
                )]
                let target = number as f64;
                #[allow(
                    clippy::float_cmp,
                    reason = "Python compares an int and a float exactly"
                )]
                n.as_f64().is_some_and(|float| float == target)
            },
            |int| int == i128::from(number),
        ),
        _ => false,
    }
}

/// `str(value)` of a JSON scalar (`None` for a missing value).
#[must_use]
pub fn str_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// `str(value or '')`: an empty string for any falsy value.
#[must_use]
pub fn str_or_empty(value: Option<&Value>) -> String {
    if is_falsy(value) {
        String::new()
    } else {
        str_of(value)
    }
}

/// Whether Python would treat the value as false.
#[must_use]
pub fn is_falsy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => true,
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::String(text)) => text.is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(map)) => map.is_empty(),
        Some(Value::Bool(true)) => false,
    }
}

/// `int(value)` of a JSON value: integers, floats truncated, booleans,
/// decimal strings.
///
/// # Errors
/// Anything else, worded like Python's refusal.
pub fn int_of(value: Option<&Value>) -> Result<i128, String> {
    match value {
        Some(Value::Bool(flag)) => Ok(i128::from(*flag)),
        Some(number @ Value::Number(n)) => as_int(number).map_or_else(
            || {
                let float = n.as_f64().unwrap_or(0.0);
                if float.is_finite() {
                    #[allow(clippy::cast_possible_truncation, reason = "int() truncates a float")]
                    Ok(float.trunc() as i128)
                } else {
                    Err("cannot convert float infinity to integer".to_owned())
                }
            },
            Ok,
        ),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            let (negative, digits) = match trimmed.as_bytes().first() {
                Some(b'-') => (true, trimmed.get(1..).unwrap_or_default()),
                Some(b'+') => (false, trimmed.get(1..).unwrap_or_default()),
                _ => (false, trimmed),
            };
            let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
            if cleaned.is_empty()
                || !cleaned.bytes().all(|b| b.is_ascii_digit())
                || digits.starts_with('_')
                || digits.ends_with('_')
                || digits.contains("__")
            {
                return Err(format!("invalid literal for int() with base 10: {text:?}"));
            }
            let value: i128 = cleaned.parse().unwrap_or(i128::MAX);
            Ok(if negative {
                value.saturating_neg()
            } else {
                value
            })
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            match other {
                None | Some(Value::Null) => "NoneType",
                Some(Value::Array(_)) => "list",
                _ => "dict",
            }
        )),
    }
}

/// `repr()` of a string list, as a `CalledProcessError` shows its command.
#[must_use]
pub fn repr_list(items: &[&str]) -> String {
    let shown: Vec<String> = items
        .iter()
        .map(|item| {
            let quote = if item.contains('\'') && !item.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::new();
            out.push(quote);
            for c in item.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        })
        .collect();
    format!("[{}]", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn int_and_str_read_like_python() {
        assert_eq!(int_of(Some(&json!(5))), Ok(5));
        assert_eq!(int_of(Some(&json!("7"))), Ok(7));
        assert_eq!(int_of(Some(&json!(2.9))), Ok(2));
        assert_eq!(int_of(Some(&json!(true))), Ok(1));
        assert!(int_of(Some(&json!("x"))).is_err());
        assert!(int_of(None).is_err());
        assert_eq!(str_of(Some(&json!(12))), "12");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(Some(&json!("GUID"))), "GUID");
        assert!(equals_number(&json!(512.0), 512));
        assert!(!equals_number(&json!("512"), 512));
        assert_eq!(repr_list(&["sfdisk", "-N", "6"]), "['sfdisk', '-N', '6']");
    }
}
