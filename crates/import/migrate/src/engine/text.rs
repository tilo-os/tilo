//! A migration's answers as sentences for a person reading a terminal: the text view of the same
//! sources, reviews and statuses the other views show.

use std::fmt::Write as _;

use serde_json::Value;

use super::counted;

/// `answer` as sentences for a person reading the terminal.
#[must_use]
pub fn sentences(answer: &Value) -> String {
    let mut out = String::new();
    if let Some(said) = answer.get("said").and_then(Value::as_str) {
        out.push_str(said);
        out.push('\n');
        return out;
    }
    if let Some(found) = answer.get("sources").and_then(Value::as_array) {
        for source in found {
            let name = source["source"].as_str().unwrap_or_default();
            let title = source["title"].as_str().unwrap_or_default();
            if let Some(why) = source.get("unavailable").and_then(Value::as_str) {
                let _ = writeln!(out, "{name:12} {title}: {why}");
            } else {
                let here = source
                    .get("here")
                    .and_then(Value::as_str)
                    .map(|here| format!(" [{here}]"))
                    .unwrap_or_default();
                let _ = writeln!(out, "{name:12} {title}{here}");
                if let Some(next) = source.get("next").and_then(Value::as_str) {
                    let _ = writeln!(out, "{:12}   next: {next}", "");
                }
            }
        }
        return out;
    }
    if let Some(items) = answer.get("items").and_then(Value::as_array) {
        review_sentences(answer, items, &mut out);
        return out;
    }
    if let Some(all) = answer.get("migrations").and_then(Value::as_array) {
        if all.is_empty() {
            out.push_str("nothing reviewed yet: `tilo migrate sources`\n");
        }
        for status in all {
            status_sentence(status, &mut out);
        }
        return out;
    }
    if answer.get("state").is_some() {
        status_sentence(answer, &mut out);
        return out;
    }
    out.push_str(&answer.to_string());
    out.push('\n');
    out
}

/// One migration's `status` as a line, and the step to take next.
fn status_sentence(status: &Value, out: &mut String) {
    let _ = write!(
        out,
        "{}: {}",
        status["source"].as_str().unwrap_or_default(),
        status["state"].as_str().unwrap_or_default()
    );
    if let Some(brought) = status.get("brought").and_then(Value::as_object)
        && !brought.is_empty()
    {
        let parts: Vec<String> = brought
            .iter()
            .map(|(kind, count)| {
                let count = count
                    .as_u64()
                    .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
                counted(count, kind)
            })
            .collect();
        let _ = write!(out, ", brought {}", parts.join(", "));
    }
    if let Some(already) = status.get("already_there").and_then(Value::as_u64)
        && already > 0
    {
        let _ = write!(out, ", {already} already there");
    }
    if let Some(failure) = status.get("failure").and_then(Value::as_str) {
        let _ = write!(out, ": {failure}");
    }
    out.push('\n');
    if let Some(next) = status.get("next").and_then(Value::as_str) {
        let _ = writeln!(out, "next: {next}");
    }
}

/// A review's `items`, of `answer`, as lines for a person reading the terminal.
fn review_sentences(answer: &Value, items: &[Value], out: &mut String) {
    let _ = writeln!(out, "{}", answer["title"].as_str().unwrap_or_default());
    for item in items {
        let indent = if item.get("within").is_some() {
            "    "
        } else {
            "  "
        };
        let key = item["key"].as_str().unwrap_or_default();
        let title = item["title"].as_str().unwrap_or_default();
        let _ = write!(
            out,
            "{indent}[{}] {key}",
            if item["chosen"] == true { "x" } else { " " },
        );
        // A title that says more than the key does (what choosing it does) is shown.
        if !title.is_empty() && title != key && !key.ends_with(title) {
            let _ = write!(out, "  {title}");
        }
        let counts = item["counts"].as_str().unwrap_or_default();
        if !counts.is_empty() {
            let count = item["count"]
                .as_u64()
                .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
            let _ = write!(out, "  {}", counted(count, counts));
        }
        out.push('\n');
        if let Some(note) = item.get("note").and_then(Value::as_str) {
            let _ = writeln!(out, "{indent}    {note}");
        }
    }
    if let Some(problems) = answer.get("unreadable").and_then(Value::as_array) {
        for problem in problems.iter().filter_map(Value::as_str) {
            let _ = writeln!(out, "  incomplete: {problem}");
        }
    }
    if let Some(next) = answer.get("next").and_then(Value::as_str) {
        let _ = writeln!(out, "next: {next}");
    }
}
