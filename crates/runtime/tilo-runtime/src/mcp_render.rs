//! The text an agent reads: how a read, an action and its answer render.
use super::*;

/// A cursor [`read_cursor`] issued, taken apart: the characters already
/// answered and the app's cursor for the page they are in. `None` for a
/// cursor of the app's own.
pub(super) fn parse_read_cursor(cursor: &str) -> Option<(usize, Option<String>)> {
    let rest = cursor.strip_prefix(READ_CURSOR)?;
    let (answered, app_cursor) = match rest.split_once('+') {
        Some((answered, app_cursor)) => (
            answered,
            Some(app_cursor.to_owned()).filter(|cursor| !cursor.is_empty()),
        ),
        None => (rest, None),
    };
    Some((answered.parse().ok()?, app_cursor))
}

/// Drop the first `answered` characters of `text`, and the line break the
/// cut before them stopped at. How many characters went.
pub(super) fn skip_answered(text: &mut String, answered: usize) -> usize {
    if answered == 0 {
        return 0;
    }
    let mut end = text
        .char_indices()
        .nth(answered)
        .map_or(text.len(), |(at, _)| at);
    let mut skipped = text.get(..end).map_or(0, |head| head.chars().count());
    if text.get(end..).is_some_and(|rest| rest.starts_with('\n')) {
        end = end.saturating_add(1);
        skipped = skipped.saturating_add(1);
    }
    text.replace_range(..end, "");
    skipped
}

/// What a read of `output` answers: its text from character `already` on,
/// cut at `max_chars` with the cursor for the rest, else ending in the app's
/// cursor for its next page (`app_cursor`) when it gave one.
pub(super) fn read_answer(
    read: &ResourceRead,
    already: usize,
    app_cursor: Option<&str>,
    max_chars: Option<usize>,
) -> String {
    let mut text = read.render();
    let skipped = skip_answered(&mut text, already);
    let limit = max_chars.unwrap_or(READ_CHARS).clamp(200, 200_000);
    let cut = cut_at_line(&mut text, limit);
    if let Some(left) = cut {
        let cursor = read_cursor(skipped.saturating_add(text.chars().count()), app_cursor);
        let _ = write!(text, "\n… {left} more characters: cursor \"{cursor}\"");
    }
    if let Some(cursor) = read.cursor.as_ref().filter(|_| cut.is_none()) {
        let _ = write!(text, "\n… more: cursor \"{cursor}\"");
    }
    if text.is_empty() {
        text.push_str("(empty)");
    }
    text
}

/// How long to wait before a directory level's `read`th read (0 the first)
/// while it keeps changing: the first change is read past at once, then from
/// 5 ms, twice as long each time, up to [`RESTART_PAUSE_MAX`].
pub(super) fn restart_pause(read: u32) -> std::time::Duration {
    let Some(doublings) = read.checked_sub(2) else {
        return std::time::Duration::ZERO;
    };
    std::time::Duration::from_millis(5)
        .checked_mul(1_u32.checked_shl(doublings).unwrap_or(u32::MAX))
        .map_or(RESTART_PAUSE_MAX, |pause| pause.min(RESTART_PAUSE_MAX))
}

pub(super) fn first_sentence(text: &str) -> String {
    let text = text.trim();
    let sentence = text
        .split_once(". ")
        .map_or(text, |(first, _)| first)
        .trim_end_matches('.');
    if sentence.chars().count() <= DESCRIPTION_CHARS {
        sentence.to_owned()
    } else {
        let head: String = sentence.chars().take(DESCRIPTION_CHARS).collect();
        format!("{}…", head.trim_end())
    }
}

pub(super) fn short(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(chars).collect();
    format!("{}…", head.trim_end())
}

/// `(p, q?, kind:a|b)`, from the action's parameters.
pub(super) fn params_line(action: &ActionDescriptor) -> String {
    if action.params.is_empty() {
        return String::new();
    }
    let params = action
        .params
        .iter()
        .map(page::param)
        .collect::<Vec<_>>()
        .join(", ");
    format!("({params})")
}

pub(super) fn risk_note(action: &ActionDescriptor) -> String {
    match page::risk_note(&action.contract) {
        Some(note) => format!(" [{note}]"),
        None => String::new(),
    }
}

/// One action in a listing, as a program calls it (`session.show_overview()  Show the
/// overview`), or as the door's own tools take it (`#rename(value)  Rename note`) where no
/// program can.
pub(super) fn action_line(action: &ActionDescriptor, called: Option<&str>) -> String {
    let mut line = called.map_or_else(
        || format!("#{}{}", action.address.action, params_line(action)),
        str::to_owned,
    );
    let label = action.action_label.trim();
    if !label.is_empty() && !label.eq_ignore_ascii_case(&action.address.action.replace('-', " ")) {
        let _ = write!(line, "  {}", short(label, LABEL_CHARS));
    }
    line.push_str(&risk_note(action));
    line
}

/// The whole of one action: what it does and what it takes, and how a program calls it.
pub(super) fn action_page(path: &str, action: &ActionDescriptor, called: Option<&str>) -> String {
    let mut page = format!("{path}#{}", action.address.action);
    if let Some(call) = called {
        let _ = write!(page, "\n{call}");
    }
    let label = action.action_label.trim();
    if !label.is_empty() {
        let _ = write!(page, "  {label}");
    }
    page.push_str(&risk_note(action));
    if let Some(description) = action
        .description
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        let _ = write!(page, "\n{}", first_sentence(description));
    }
    if action.params.is_empty() {
        page.push_str("\nno parameters");
    }
    for param in &action.params {
        let _ = write!(page, "\n  {}", page::param(param));
        let description = param.description.trim();
        if !description.is_empty() && !description.eq_ignore_ascii_case(param.name.as_ref()) {
            let _ = write!(page, " — {}", first_sentence(description));
        }
    }
    page
}

/// Whether `value` is empty of meaning: null, an empty string, array or
/// object.
pub(super) fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

/// `value` as compact JSON, without the nulls and empty containers that say
/// nothing.
pub(super) fn strip_blank(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), strip_blank(item)))
                .filter(|(_, item)| !is_blank(item))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_blank).collect()),
        other => other.clone(),
    }
}

/// One block of a document as a markdown line ending in its `{#id}`.
pub(super) fn block_line(block: &Value) -> Option<String> {
    let text = block
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = block.get("id").and_then(Value::as_str)?;
    let kind = block.get("kind");
    let level = |name: &str| {
        kind.and_then(|kind| kind.get(name))
            .and_then(Value::as_u64)
            .map(|level| usize::try_from(level).unwrap_or(0).min(12))
    };
    let indent = |level: usize| "  ".repeat(level);
    let line = if let Some(heading) = level("heading") {
        format!("{} {text} ^{id}", "#".repeat(heading.clamp(1, 6)))
    } else if let Some(depth) = level("bullet") {
        format!("{}- {text} ^{id}", indent(depth))
    } else if let Some(depth) = level("numbered") {
        format!("{}1. {text} ^{id}", indent(depth))
    } else if let Some(depth) = level("checklist") {
        let done = kind
            .and_then(|kind| kind.get("done"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        format!(
            "{}- [{}] {text} ^{id}",
            indent(depth),
            if done { 'x' } else { ' ' }
        )
    } else if kind.is_some_and(|kind| kind.get("code").is_some()) {
        let language = kind
            .and_then(|kind| kind.get("code"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        format!("```{language} ^{id}\n{text}\n```")
    } else {
        match kind.and_then(Value::as_str) {
            Some("quote") => format!("> {text} ^{id}"),
            Some("divider") => format!("--- ^{id}"),
            _ => format!("{text} ^{id}"),
        }
    };
    Some(line)
}

/// What an app's read or an action's output is, as the least text that says
/// it ([`ResourceRead::render`]).
pub(super) fn render_value(value: &Value) -> String {
    ResourceRead::of(value).render()
}
