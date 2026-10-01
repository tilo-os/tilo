//! The first page of a just-opened document, cut to what an `open` reply
//! carries.
use serde_json::Value;

/// Largest document page an `open` answers with, in bytes of JSON. A model
/// reads it again on every later turn; beyond this, the app's read action
/// answers the rest.
pub(super) const CONTENT_BYTES: usize = 6 * 1024;
/// Keys under which an app's read names the page after this one. A cut
/// page's cursor would skip what was cut, so it is dropped.
const CURSOR_KEYS: [&str; 2] = ["nextCursor", "next_cursor"];

fn size(value: &Value) -> usize {
    value.to_string().len()
}

/// Cut `value` until its JSON fits in `budget` bytes: the largest string
/// loses its tail at a line break, the largest array its last elements.
/// Answers whether anything was cut, and then drops the page's cursor.
pub(super) fn fit(value: &mut Value, budget: usize) -> bool {
    let mut cut = false;
    for _ in 0..64 {
        let over = size(value).saturating_sub(budget);
        if over == 0 {
            break;
        }
        cut = true;
        let taken = value
            .pointer_mut(&largest(value))
            .is_some_and(|largest| shrink(largest, over));
        if !taken {
            *value = Value::Null;
            break;
        }
    }
    if cut && let Value::Object(fields) = value {
        for key in CURSOR_KEYS {
            fields.remove(key);
        }
    }
    cut
}

/// The JSON pointer to the string or array holding the most bytes of
/// `value`, else to `value`.
fn largest(value: &Value) -> String {
    let cuttable = |child: &Value| child.is_object() || child.is_array() || child.is_string();
    let inside = match value {
        Value::Object(fields) => fields
            .iter()
            .max_by_key(|(_, child)| size(child))
            .filter(|(_, child)| cuttable(child))
            .map(|(key, child)| (key.replace('~', "~0").replace('/', "~1"), child)),
        // An array whose one element outweighs the rest is cut inside it;
        // any other loses elements.
        Value::Array(items) => {
            let whole = items.iter().map(size).sum::<usize>();
            items
                .iter()
                .enumerate()
                .max_by_key(|(_, item)| size(item))
                .filter(|(_, item)| cuttable(item) && size(item).saturating_mul(2) > whole)
                .map(|(at, item)| (at.to_string(), item))
        }
        _ => None,
    };
    inside.map_or_else(String::new, |(step, child)| {
        format!("/{step}{}", largest(child))
    })
}

/// Take at least `over` bytes out of `value`. Answers false when nothing
/// is left to take.
fn shrink(value: &mut Value, over: usize) -> bool {
    match value {
        Value::String(text) if !text.is_empty() => {
            let mut keep = text.len().saturating_sub(over.saturating_add(1));
            while !text.is_char_boundary(keep) {
                keep = keep.saturating_sub(1);
            }
            let at_line = text
                .get(..keep)
                .and_then(|kept| kept.rfind('\n'))
                .filter(|at| *at >= keep / 2);
            text.truncate(at_line.map_or(keep, |at| at.saturating_add(1)));
            text.push('…');
            true
        }
        Value::Array(items) if !items.is_empty() => {
            let mut taken = 0;
            while taken < over
                && let Some(item) = items.pop()
            {
                taken = taken.saturating_add(size(&item)).saturating_add(1);
            }
            true
        }
        Value::Object(fields) if !fields.is_empty() => {
            fields.clear();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_page_within_budget_is_left_whole() {
        let mut page = json!({"content":{"markdown":"# Title {#b0}\n"},"nextCursor":null});
        let before = page.clone();
        assert!(!fit(&mut page, CONTENT_BYTES));
        assert_eq!(page, before);
    }

    #[test]
    fn long_text_is_cut_at_a_line_and_loses_its_cursor() {
        let markdown = (0..400)
            .map(|n| format!("Paragraph {n} of the report. {{#b{n}}}\n"))
            .collect::<Vec<_>>()
            .concat();
        let mut page = json!({"version":1,"content":{"markdown":markdown},"nextCursor":"c1"});
        assert!(fit(&mut page, CONTENT_BYTES));
        assert!(page.to_string().len() <= CONTENT_BYTES);
        let cut = page["content"]["markdown"].as_str().unwrap();
        assert!(cut.starts_with("Paragraph 0 of the report. {#b0}\n"));
        assert!(cut.ends_with("}\n…"), "cut inside a line");
        assert_eq!(page["version"], 1);
        assert!(page.get("nextCursor").is_none());
    }

    #[test]
    fn many_rows_lose_whole_rows_from_the_end() {
        let rows: Vec<Value> = (0..2000)
            .map(|n| json!([format!("name {n}"), n, "x@example.com"]))
            .collect();
        let mut page =
            json!({"content":{"sheet":"s","range":"A1:C2000","rows":rows,"formulas":{},"bold":[]}});
        assert!(fit(&mut page, CONTENT_BYTES));
        assert!(page.to_string().len() <= CONTENT_BYTES);
        let rows = page["content"]["rows"].as_array().unwrap();
        assert!(rows.len() > 50, "{}", rows.len());
        assert_eq!(rows[0], json!(["name 0", 0, "x@example.com"]));
        assert!(
            rows.iter()
                .all(|row| row.as_array().is_some_and(|r| r.len() == 3))
        );
        assert_eq!(page["content"]["range"], "A1:C2000");
    }
}
