//! Facts: what a record states about itself to be checked against a
//! request, by convention a read named [`FACTS`] that takes nothing
//! (`#[resource(facts = path)]`), and what changed between two of them
//! ([`changes`]).
//!
//! A check compares data rather than pictures: a task's check turn is shown
//! the facts of the records the task changed, and what changed since an
//! earlier task exported one; a planning model's `read_view` compares two
//! answers of one read (the facts now and as an export made them).

use serde_json::Value;
use tilo_actions::door::ActionDescriptor;

/// The read a record states its facts with.
pub const FACTS: &str = "facts";

/// The most changes [`changes`] lists before it counts the rest.
pub const MOST_CHANGES: usize = 40;

/// The descriptor of `descriptors` that reads the facts of the record's
/// own resource: a read named [`FACTS`] that needs no argument.
#[must_use]
pub fn facts_read(descriptors: &[ActionDescriptor]) -> Option<&ActionDescriptor> {
    descriptors.iter().find(|descriptor| {
        descriptor.address.action == FACTS
            && crate::apps::readable(descriptor)
            && descriptor.params.iter().all(|param| !param.required)
    })
}

/// What differs from `before` to `after`, a line per value, each named by
/// its path (`shots[1].in: 1 -> 0.5`): objects key by key, lists item by
/// item in order, numbers by value. At most [`MOST_CHANGES`] lines, then
/// how many more.
#[must_use]
pub fn changes(before: &Value, after: &Value) -> Vec<String> {
    let mut found = Vec::new();
    walk("", before, after, &mut found);
    if found.len() > MOST_CHANGES {
        let more = found.len().saturating_sub(MOST_CHANGES);
        found.truncate(MOST_CHANGES);
        found.push(format!("… and {more} more"));
    }
    found
}

/// [`changes`] as one line: `nothing changed` when there are none.
#[must_use]
pub fn changes_text(before: &Value, after: &Value) -> String {
    let found = changes(before, after);
    if found.is_empty() {
        "nothing changed".to_owned()
    } else {
        found.join("; ")
    }
}

fn walk(path: &str, before: &Value, after: &Value, found: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, was) in old {
                let at = child(path, key);
                match new.get(key) {
                    Some(now) => walk(&at, was, now, found),
                    None => found.push(format!("{at}: removed (was {})", shown(was))),
                }
            }
            for (key, now) in new {
                if !old.contains_key(key) {
                    found.push(format!("{}: added {}", child(path, key), shown(now)));
                }
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            for index in 0..old.len().max(new.len()) {
                let at = format!("{path}[{index}]");
                match (old.get(index), new.get(index)) {
                    (Some(was), Some(now)) => walk(&at, was, now, found),
                    (Some(was), None) => found.push(format!("{at}: removed (was {})", shown(was))),
                    (None, Some(now)) => found.push(format!("{at}: added {}", shown(now))),
                    (None, None) => {}
                }
            }
        }
        _ if same(before, after) => {}
        _ => found.push(format!(
            "{}: {} -> {}",
            if path.is_empty() {
                "the whole answer"
            } else {
                path
            },
            shown(before),
            shown(after)
        )),
    }
}

/// Whether two leaves are one value: numbers by value, so `3` and `3.0`
/// are the same time.
fn same(before: &Value, after: &Value) -> bool {
    match (before.as_f64(), after.as_f64()) {
        (Some(was), Some(now)) => (was - now).abs() < 1e-9,
        _ => before == after,
    }
}

fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

/// A value as a change line shows it: compact JSON, cut at 200 characters.
fn shown(value: &Value) -> String {
    const MOST: usize = 200;
    let text = crate::planner::compact(value).to_string();
    if text.chars().count() <= MOST {
        return text;
    }
    let mut cut: String = text.chars().take(MOST).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests;
