use serde_json::Value;

/// Read provider-authored reasoning metadata for same-conversation replay.
/// This is separate from user-visible final content; absent/null stays absent.
/// # Errors
/// Rejects non-string reasoning content rather than guessing or coercing it.
pub fn reasoning_content(message: &Value) -> Result<Option<&str>, String> {
    match message.get("reasoning_content") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err("the model's reasoning_content was not a string".into()),
    }
}
