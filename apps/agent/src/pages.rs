//! Long results, read a page at a time.
//!
//! An operation can answer with much more than one observation should
//! carry: a chapter, a long note, a file's text, a page of search results.
//! Cutting it off would leave the model guessing at the rest, so a long
//! value is kept for the task and shown one page at a time: the observation
//! carries the first page and says where the rest is (`continues`), and
//! [`READ_RESULT`] reads on from any offset. Nothing here performs
//! anything; it only shows again what an operation already returned to
//! this task.
//!
//! When the value is an object whose bulk is one long text field (a read's
//! `markdown`, a file's `text`), only that field is paged, so the object's
//! other fields — a cursor, a count, a title — arrive whole and as JSON.

use std::collections::VecDeque;

use serde_json::{Map, Value, json};

/// The tool that reads a kept result on from an offset.
pub const READ_RESULT: &str = "read_result";

/// The most characters of one page.
pub const PAGE_CHARS: usize = 4_000;
/// Results one task keeps for paging; past it the oldest is forgotten.
pub const MOST_KEPT: usize = 16;
/// The most characters of one result that are kept. Past it the rest is
/// dropped, and the last page says so.
pub const MOST_KEPT_CHARS: usize = 512 * 1024;

/// One kept result: the whole text that was paged.
struct Kept {
    id: String,
    text: String,
    chars: usize,
    /// The result was longer than [`MOST_KEPT_CHARS`] and is cut there.
    cut: bool,
}

/// The long results of one task, by the id the observation gave them.
#[derive(Default)]
pub struct Pages {
    kept: VecDeque<Kept>,
    issued: usize,
}

impl Pages {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `value` as one observation shows it, and where the rest is when it
    /// does not fit one page.
    ///
    /// A value that fits is returned unchanged with no continuation.
    pub fn first_page(&mut self, value: &Value) -> (Value, Option<Value>) {
        if let Value::String(text) = value {
            return self.page_text(text, None);
        }
        let compact = value.to_string();
        if compact.chars().count() <= PAGE_CHARS {
            return (value.clone(), None);
        }
        if let Value::Object(object) = value
            && let Some((field, text)) = longest_text(object)
        {
            let mut rest = object.clone();
            rest.remove(field);
            let rest_chars = Value::Object(rest.clone()).to_string().chars().count();
            if rest_chars <= PAGE_CHARS / 2 {
                let room = PAGE_CHARS - rest_chars;
                let (page, continues) = self.page_text_within(text, Some(field), room);
                rest.insert(field.clone(), page);
                return (Value::Object(rest), continues);
            }
        }
        self.page_text(&compact, None)
    }

    fn page_text(&mut self, text: &str, field: Option<&String>) -> (Value, Option<Value>) {
        self.page_text_within(text, field, PAGE_CHARS)
    }

    fn page_text_within(
        &mut self,
        text: &str,
        field: Option<&String>,
        room: usize,
    ) -> (Value, Option<Value>) {
        let chars = text.chars().count();
        if chars <= room {
            return (Value::String(text.to_owned()), None);
        }
        let id = self.keep(text);
        let mut continues = Map::new();
        continues.insert("result".into(), Value::String(id));
        if let Some(field) = field {
            continues.insert("field".into(), Value::String(field.clone()));
        }
        continues.insert("from".into(), json!(room));
        continues.insert("total".into(), json!(chars));
        continues.insert(
            "how".into(),
            Value::String(format!(
                "This is the first {room} characters; call `{READ_RESULT}` with this `result` \
                 and `from` to read on."
            )),
        );
        (
            Value::String(slice_chars(text, 0, room).to_owned()),
            Some(Value::Object(continues)),
        )
    }

    /// Keep `text`, returning the id the observation names it by.
    fn keep(&mut self, text: &str) -> String {
        self.issued += 1;
        let id = format!("r{}", self.issued);
        let chars = text.chars().count();
        let cut = chars > MOST_KEPT_CHARS;
        let text = if cut {
            slice_chars(text, 0, MOST_KEPT_CHARS).to_owned()
        } else {
            text.to_owned()
        };
        if self.kept.len() >= MOST_KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(Kept {
            id: id.clone(),
            chars: chars.min(MOST_KEPT_CHARS),
            text,
            cut,
        });
        id
    }

    /// Answer one [`READ_RESULT`] call: the page of a kept result at `from`.
    #[must_use]
    pub fn read(&self, input: &Value) -> String {
        let Some(id) = input.get("result").and_then(Value::as_str).map(str::trim) else {
            return format!("`{READ_RESULT}` needs the `result` id a `continues` gave.");
        };
        let Some(kept) = self.kept.iter().find(|kept| kept.id == id) else {
            let known: Vec<&str> = self.kept.iter().map(|kept| kept.id.as_str()).collect();
            return if known.is_empty() {
                format!("There is no result {id:?}: no long result is kept in this task.")
            } else {
                format!(
                    "There is no result {id:?} in this task. Kept results: {}.",
                    known.join(", ")
                )
            };
        };
        let from = input
            .get("from")
            .and_then(Value::as_u64)
            .and_then(|from| usize::try_from(from).ok())
            .unwrap_or(0)
            .min(kept.chars);
        let to = from.saturating_add(PAGE_CHARS).min(kept.chars);
        let mut page = Map::new();
        page.insert("result".into(), Value::String(kept.id.clone()));
        page.insert("from".into(), json!(from));
        page.insert(
            "text".into(),
            Value::String(slice_chars(&kept.text, from, to).to_owned()),
        );
        page.insert(
            "next".into(),
            if to < kept.chars {
                json!(to)
            } else {
                Value::Null
            },
        );
        page.insert("total".into(), json!(kept.chars));
        if kept.cut && to == kept.chars {
            page.insert(
                "note".into(),
                Value::String(format!(
                    "The result was longer than {MOST_KEPT_CHARS} characters and is kept only \
                     up to here; ask the operation for a narrower part to read past it."
                )),
            );
        }
        Value::Object(page).to_string()
    }
}

/// The longest text field of `object`, when it is long enough to page.
fn longest_text(object: &Map<String, Value>) -> Option<(&String, &str)> {
    object
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|text| (key, text)))
        .max_by_key(|(_, text)| text.len())
        .filter(|(_, text)| text.chars().count() > PAGE_CHARS / 2)
}

/// Characters `from..to` of `text`.
fn slice_chars(text: &str, from: usize, to: usize) -> &str {
    let byte = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(at, _)| at);
    let start = byte(from);
    text.get(start..byte(to).max(start)).unwrap_or_default()
}

/// The [`READ_RESULT`] tool definition.
#[must_use]
pub fn read_result_definition() -> agent_provider_config::protocol::ToolDefinition {
    agent_provider_config::protocol::ToolDefinition {
        name: READ_RESULT.into(),
        description:
            "Read on in a long result. An invoke whose result did not fit shows its first \
             page and a `continues` with the result's id and where to go on from; this \
             returns the next page and where the one after it starts (`next`, null at the \
             end). Read on only as far as the request needs."
                .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "result": {
                    "type": "string",
                    "description": "The `result` id a `continues` gave."
                },
                "from": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "The character to read from: a `continues`' `from`, or a page's `next`."
                }
            },
            "required": ["result", "from"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_value_is_shown_whole() {
        let mut pages = Pages::new();
        let value = json!({"title": "Notes", "count": 3});
        assert_eq!(pages.first_page(&value), (value, None));
    }

    /// A long text is shown a page at a time and every character is reached
    /// exactly once, multi-byte ones included.
    #[test]
    fn a_long_text_is_read_page_by_page_to_its_end() {
        let mut pages = Pages::new();
        let text: String = (0..PAGE_CHARS * 2 + 10)
            .map(|i| if i % 7 == 0 { 'é' } else { 'a' })
            .collect();
        let (first, continues) = pages.first_page(&Value::String(text.clone()));
        let continues = continues.expect("a long text continues");
        let mut read = first.as_str().unwrap().to_owned();
        assert_eq!(read.chars().count(), PAGE_CHARS);
        let mut from = continues["from"].clone();
        let id = continues["result"].clone();
        while let Some(at) = from.as_u64() {
            let page: Value =
                serde_json::from_str(&pages.read(&json!({"result": id, "from": at}))).unwrap();
            read.push_str(page["text"].as_str().unwrap());
            from = page["next"].clone();
        }
        assert_eq!(read, text);
    }

    /// Only the long field of an object is paged; the rest stays JSON.
    #[test]
    fn an_objects_long_field_is_paged_and_the_rest_kept_whole() {
        let mut pages = Pages::new();
        let markdown = "word ".repeat(PAGE_CHARS);
        let value = json!({"markdown": markdown, "nextCursor": "c2", "blocks": 40});
        let (first, continues) = pages.first_page(&value);
        assert_eq!(first["nextCursor"], "c2");
        assert_eq!(first["blocks"], 40);
        let continues = continues.unwrap();
        assert_eq!(continues["field"], "markdown");
        let shown = first["markdown"].as_str().unwrap().chars().count();
        assert_eq!(continues["from"], json!(shown));
        assert!(first.to_string().chars().count() <= PAGE_CHARS + 64);
        let page: Value = serde_json::from_str(
            &pages.read(&json!({"result": continues["result"], "from": shown})),
        )
        .unwrap();
        assert!(markdown[shown..].starts_with(page["text"].as_str().unwrap()));
    }

    /// A long structure with no dominant text is paged as its JSON text.
    #[test]
    fn a_long_structure_is_paged_as_json_text() {
        let mut pages = Pages::new();
        let rows: Vec<Value> = (0..2000).map(|i| json!({"row": i, "name": "x"})).collect();
        let value = Value::Array(rows);
        let (first, continues) = pages.first_page(&value);
        assert!(first.as_str().unwrap().starts_with("[{"));
        assert_eq!(continues.unwrap()["total"], json!(value.to_string().len()));
    }

    #[test]
    fn an_unknown_result_names_the_kept_ones() {
        let mut pages = Pages::new();
        assert!(
            pages
                .read(&json!({"result": "r9", "from": 0}))
                .contains("no long result")
        );
        pages.first_page(&Value::String("x".repeat(PAGE_CHARS + 1)));
        assert!(
            pages
                .read(&json!({"result": "r9", "from": 0}))
                .contains("r1")
        );
    }

    #[test]
    fn only_the_latest_results_are_kept() {
        let mut pages = Pages::new();
        for _ in 0..=MOST_KEPT {
            pages.first_page(&Value::String("x".repeat(PAGE_CHARS + 1)));
        }
        assert!(
            pages
                .read(&json!({"result": "r1", "from": 0}))
                .contains("no result")
        );
        let last = format!("r{}", MOST_KEPT + 1);
        assert!(
            pages
                .read(&json!({"result": last, "from": 0}))
                .contains("\"text\"")
        );
    }
}
