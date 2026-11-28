//! What was typed after an action's path, turned into its arguments: `name=value`,
//! `--name=value` and `--name value` name a parameter, each value the JSON it reads as or else
//! text (`count=3` is a number, `title=Groceries` text), and `name=@-` reads that one value
//! from standard input, so a secret (an API key) never stands in a command line, where every
//! process on the machine can read it. A lone value goes to the one parameter still required,
//! or the only one, or the one of a type it can be (`dark` is no object). The walk binds labels
//! to the values the app takes and checks the rest against what the action takes.

use serde_json::{Map, Value};

/// The value of an option written `name=@-`: read from standard input, as text.
const FROM_INPUT: &str = "@-";

/// `words` bound to the parameters `described` (the action as `tilo_ls path#action` lists it)
/// takes, `name=@-` read from `input`. An action this side has not seen takes any name.
///
/// # Errors
/// The sentence that says what was wrong and what the action takes.
pub fn arguments(
    described: &Value,
    words: &[String],
    input: &mut dyn std::io::Read,
) -> Result<Map<String, Value>, String> {
    let params: Vec<&Value> = described
        .get("params")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let named =
        |key: &str| params.is_empty() || params.iter().any(|param| field(param, "name") == key);
    let mut args = Map::new();
    let mut read = false;
    let mut bare: Vec<&str> = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let (key, value) = match word.strip_prefix("--") {
            Some(flag) => match flag.split_once('=') {
                Some((key, value)) => (key, value),
                None => (
                    flag,
                    words
                        .next()
                        .ok_or_else(|| format!("--{flag} needs a value after it."))?
                        .as_str(),
                ),
            },
            None => match word.split_once('=') {
                Some((key, value)) if named(key) => (key, value),
                _ if params.is_empty() => return Err(format!("Write “{word}” as name=value.")),
                _ => {
                    bare.push(word);
                    continue;
                }
            },
        };
        let value = if value == FROM_INPUT {
            if std::mem::replace(&mut read, true) {
                return Err(format!(
                    "Only one value is read from standard input; “{key}” is a second."
                ));
            }
            let mut text = String::new();
            input.read_to_string(&mut text).map_err(|error| {
                format!("“{key}” could not be read from standard input: {error}")
            })?;
            Value::String(text.trim_end_matches(['\n', '\r']).to_owned())
        } else {
            read_value(value)
        };
        args.insert(key.to_owned(), value);
    }
    if bare.is_empty() {
        return Ok(args);
    }
    let value = read_value(&bare.join(" "));
    let open = |param: &&Value| !args.contains_key(field(param, "name"));
    let required: Vec<&Value> = params
        .iter()
        .copied()
        .filter(open)
        .filter(|param| param.get("required") == Some(&Value::Bool(true)))
        .collect();
    let fitting: Vec<&Value> = params
        .iter()
        .copied()
        .filter(open)
        .filter(|param| can_be(field(param, "ty"), &value))
        .collect();
    let lone = match (required.as_slice(), params.as_slice(), fitting.as_slice()) {
        ([one], _, _) | ([], [one], _) | ([], _, [one]) if open(one) => *one,
        _ => {
            let takes: Vec<String> = params
                .iter()
                .map(|param| format!("{} ({})", field(param, "name"), field(param, "ty")))
                .collect();
            return Err(format!(
                "Say which option “{}” is for: “{}” takes {}.",
                bare.join(" "),
                field(described, "label"),
                takes.join(", ")
            ));
        }
    };
    args.insert(field(lone, "name").to_owned(), value);
    Ok(args)
}

/// The text field `name` of `value`, empty when it has none.
fn field<'v>(value: &'v Value, name: &str) -> &'v str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// Whether a value typed alone could be one of type `ty`: a word is never an object, a list,
/// a number or a yes-or-no unless it reads as one.
fn can_be(ty: &str, value: &Value) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        _ => true,
    }
}

/// A value as typed: the JSON it reads as (`3`, `true`, `[1]`), else text.
fn read_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bound(
        described: &Value,
        words: &[&str],
        input: &[u8],
    ) -> Result<Map<String, Value>, String> {
        let words: Vec<String> = words.iter().map(|&word| word.to_owned()).collect();
        arguments(described, &words, &mut &input[..])
    }

    #[test]
    fn loose_options_are_json_where_they_parse() {
        let args = bound(&Value::Null, &["count=3", "--title=Groceries"], b"").expect("args");
        assert_eq!(args["count"], 3);
        assert_eq!(args["title"], "Groceries");
        assert_eq!(
            bound(&Value::Null, &["oops"], b"").unwrap_err(),
            "Write “oops” as name=value."
        );
    }

    #[test]
    fn a_value_written_at_dash_is_read_from_standard_input_as_text() {
        let args =
            bound(&Value::Null, &["key=@-", "provider=p1"], b"sk-secret-123\n").expect("args");
        assert_eq!(args["key"], "sk-secret-123");
        assert_eq!(args["provider"], "p1");
        assert_eq!(
            bound(&Value::Null, &["a=@-", "b=@-"], b"1").unwrap_err(),
            "Only one value is read from standard input; “b” is a second."
        );
    }

    /// The parameters an action lists, as `tilo_ls` types them.
    fn takes(params: &Value) -> Value {
        json!({"id": "set", "label": "Set it", "params": params})
    }

    #[test]
    fn a_lone_value_goes_to_the_one_parameter_it_can_be_for() {
        let volume = takes(&json!([{"name": "level", "ty": "integer", "required": true}]));
        assert_eq!(bound(&volume, &["40"], b"").unwrap()["level"], 40);
        let mode = takes(&json!([
            {"name": "mode", "ty": "text", "required": false},
            {"name": "options", "ty": "object", "required": false},
        ]));
        assert_eq!(bound(&mode, &["dark"], b"").unwrap()["mode"], "dark");
        let rename = takes(&json!([
            {"name": "note", "ty": "resource", "required": true},
            {"name": "title", "ty": "text", "required": true},
        ]));
        let named = bound(&rename, &["--note", "notes/Todo", "Groceries", "list"], b"").unwrap();
        assert_eq!(named["note"], "notes/Todo");
        assert_eq!(
            named["title"], "Groceries list",
            "the words left are one value"
        );
        assert_eq!(
            bound(&rename, &["Groceries"], b"").unwrap_err(),
            "Say which option “Groceries” is for: “Set it” takes note (resource), title (text)."
        );
        assert_eq!(
            bound(&rename, &["--title"], b"").unwrap_err(),
            "--title needs a value after it."
        );
        assert_eq!(
            bound(&takes(&json!([])), &["x"], b"").unwrap_err(),
            "Write “x” as name=value.",
            "an action that takes nothing has no option a lone value is for"
        );
    }
}
