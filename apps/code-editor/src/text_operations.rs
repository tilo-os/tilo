//! Bounded reads and contextual edits of a collaborative file, never disk paths.
use serde::Serialize;
use tilo_actions::{ActionArgs, ActionParam, ActionSpec, Doc, ExecutionLocus, ParamType};

use crate::actions::{CodeEditorAction, TextEdit, text_digest};
use crate::doc::EditorDoc;

pub(crate) const MAX_READ_CHARS: usize = 4096;
const MAX_READ_BYTES: usize = 8192;
const MAX_REPLY_BYTES: usize = 32768;
const MAX_EDIT_BYTES: usize = 16384;
const CONTEXT_CHARS: usize = 32;

#[derive(Serialize)]
pub(crate) struct TextRead {
    file: String,
    position_units: &'static str,
    text_sha256: String,
    total_chars: usize,
    start_chars: usize,
    next_chars: usize,
    eof: bool,
    text: String,
    before_context: String,
    after_context: String,
}

pub(crate) fn read(
    doc: &Doc,
    file: &str,
    start: usize,
    maximum: usize,
) -> Result<TextRead, String> {
    if maximum == 0 || maximum > MAX_READ_CHARS {
        return Err("read length must be between 1 and 4096 Unicode scalar values".into());
    }
    let record = EditorDoc::file(doc, file).ok_or("file does not exist")?;
    if record.deleted || record.kind != "file" {
        return Err("text read target is not a live file".into());
    }
    let body = EditorDoc::read_buffer_key(doc, file)
        .ok_or("file body is unavailable; open this file first")?;
    let total = body.chars().count();
    if start > total {
        return Err("read position is outside the file".into());
    }
    let mut text = String::new();
    for character in body.chars().skip(start).take(maximum) {
        if text.len().saturating_add(character.len_utf8()) > MAX_READ_BYTES {
            break;
        }
        text.push(character);
    }
    let next = start.saturating_add(text.chars().count());
    let answer = TextRead {
        file: file.into(),
        position_units: "unicode_scalar_values",
        text_sha256: text_digest(&body),
        total_chars: total,
        start_chars: start,
        next_chars: next,
        eof: next == total,
        text,
        before_context: body
            .chars()
            .skip(start.saturating_sub(CONTEXT_CHARS))
            .take(start.min(CONTEXT_CHARS))
            .collect(),
        after_context: body.chars().skip(next).take(CONTEXT_CHARS).collect(),
    };
    if serde_json::to_vec(&answer)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_REPLY_BYTES
    {
        return Err("text read exceeds the aggregate response bound".into());
    }
    Ok(answer)
}

fn integer(args: &ActionArgs, name: &str) -> Result<usize, String> {
    args.integer(name)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value <= crate::disk::MAX_TEXT_BYTES as usize)
        .ok_or_else(|| format!("{name} must be a bounded nonnegative integer"))
}

fn edit(file: &str, args: &ActionArgs) -> Result<CodeEditorAction, String> {
    let text = |name| {
        args.text(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("{name} must be text"))
    };
    let expected = text("expected_base_digest")?;
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("expected_base_digest must be a lowercase SHA-256".into());
    }
    let edit = TextEdit {
        file_id: file.into(),
        expected_base_digest: expected,
        start_chars: integer(args, "start_chars")?,
        delete_chars: integer(args, "delete_chars")?,
        insert: text("insert")?,
        before_context: text("before_context")?,
        deleted_text: text("deleted_text")?,
        after_context: text("after_context")?,
    };
    let bytes = edit
        .insert
        .len()
        .saturating_add(edit.deleted_text.len())
        .saturating_add(edit.before_context.len())
        .saturating_add(edit.after_context.len());
    if bytes > MAX_EDIT_BYTES
        || edit.deleted_text.chars().count() != edit.delete_chars
        || edit.before_context.chars().count() > CONTEXT_CHARS
        || edit.after_context.chars().count() > CONTEXT_CHARS
    {
        return Err("contextual edit exceeds its bounds or deletion count".into());
    }
    Ok(CodeEditorAction::EditText(edit))
}

pub(crate) fn edit_action(file: String) -> ActionSpec {
    ActionSpec::typed("edit-text", "Edit text", "A bounded contextual character splice",
        ExecutionLocus::ClientLocal, vec![
            ActionParam::required("expected_base_digest", ParamType::Text, "Current file SHA-256 from read-text"),
            ActionParam::required("start_chars", ParamType::Integer, "Zero-based Unicode scalar position"),
            ActionParam::required("delete_chars", ParamType::Integer, "Unicode scalar deletion count"),
            ActionParam::required("insert", ParamType::Text, "Replacement text"),
            ActionParam::required("deleted_text", ParamType::Text, "Original text of the selected range"),
            ActionParam::required("before_context", ParamType::Text, "Up to 32 preceding Unicode scalars from read-text"),
            ActionParam::required("after_context", ParamType::Text, "Up to 32 following Unicode scalars from read-text"),
        ], move |args| match edit(&file, args).and_then(|edit| crate::actions::encode(&edit).map_err(|error| error.to_string())) {
            Ok(encoded) => encoded,
            Err(error) => crate::actions::invalid_arguments(&error),
        }).with_description("Edit this file with a bounded character splice. Positions use Unicode scalar values. A stale base rebases only at one untouched contextual range; conflicting or ambiguous edits are refused.")
}
