//! The messages a record of a message app holds, read app-agnostically
//! from its Automerge document: what news and waits compare and tell.
//!
//! A message is a map holding who sent it beside what it says. Chat,
//! Messages and Mail keep theirs so; a chat's roster, read marks and the
//! order keys of a collection's items are not messages, so a member
//! introducing itself or reading a channel adds no line.

use std::collections::HashMap;

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value, ValueRef};

/// Keys naming a message's sender as it shows them.
const SENDER_KEYS: &[&str] = &["from", "sender"];

/// The key naming a message's sender by participant id.
const AUTHOR_KEY: &str = "author";

/// Keys holding what a message says, in the order its line shows them.
const SAID_KEYS: &[&str] = &["subject", "text", "body"];

/// Marks the participant id a line names its sender by: shown by name, or
/// left out, by [`shown_line`], never read as words of the message.
const PARTICIPANT_MARK: char = '\u{1f}';

/// The most values of one document visited for its messages.
const MOST_VISITED: usize = 200_000;

/// How deep into nested objects messages are looked for.
const MOST_DEPTH: usize = 48;

/// The most bytes of lines read from one document: a record past it is
/// read to there, never cut in the middle, so a line is never taken for
/// new because a cut moved.
const MOST_BYTES: usize = 256 * 1024;

/// The messages of `doc`, one line each: `sender: what it says`, where
/// what it says is its subject, text and body on one line. A sender named
/// by participant id is kept marked for [`shown_line`].
#[must_use]
pub fn message_text(doc: &impl ReadDoc) -> String {
    let mut walk = Walk {
        lines: String::new(),
        budget: MOST_VISITED,
    };
    walk.object(doc, &automerge::ROOT, ObjType::Map, 0);
    walk.lines
}

/// `line`, one of [`message_text`], as a person reads it: a sender named by
/// participant id shown by the name `name` gives it, or left out when it
/// gives none.
#[must_use]
pub fn shown_line(line: &str, name: impl Fn(&str) -> Option<String>) -> String {
    let Some((id, said)) = line
        .strip_prefix(PARTICIPANT_MARK)
        .and_then(|rest| rest.split_once(PARTICIPANT_MARK))
    else {
        return line.to_owned();
    };
    match name(id) {
        Some(name) => format!("{name}: {said}"),
        None => said.to_owned(),
    }
}

/// Whether `line` names its sender by participant id.
#[must_use]
pub fn names_participant(line: &str) -> bool {
    line.starts_with(PARTICIPANT_MARK)
}

/// The names `doc` gives participants: each map holding an `id` and a
/// `name` (a chat's roster).
#[must_use]
pub fn participant_names(doc: &impl ReadDoc) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let mut budget = MOST_VISITED;
    gather_names(doc, &automerge::ROOT, 0, (&mut budget, &mut names));
    names
}

fn gather_names(
    doc: &impl ReadDoc,
    object: &ObjId,
    depth: usize,
    (budget, names): (&mut usize, &mut HashMap<String, String>),
) {
    if *budget == 0 || depth > MOST_DEPTH {
        return;
    }
    if let (Some(id), Some(name)) = (string_at(doc, object, "id"), string_at(doc, object, "name"))
        && !id.is_empty()
        && !name.trim().is_empty()
    {
        names.insert(id, name.trim().to_owned());
    }
    for (kind, child) in child_objects(doc, object) {
        *budget = budget.saturating_sub(1);
        if *budget == 0 {
            return;
        }
        if matches!(kind, ObjType::Map | ObjType::Table | ObjType::List) {
            gather_names(doc, &child, depth.saturating_add(1), (budget, names));
        }
    }
}

struct Walk {
    lines: String,
    budget: usize,
}

impl Walk {
    fn full(&self) -> bool {
        self.budget == 0 || self.lines.len() >= MOST_BYTES
    }

    fn object(&mut self, doc: &impl ReadDoc, object: &ObjId, kind: ObjType, depth: usize) {
        if self.full() || depth > MOST_DEPTH {
            return;
        }
        if matches!(kind, ObjType::Map | ObjType::Table) && self.message(doc, object) {
            return;
        }
        for (kind, child) in child_objects(doc, object) {
            if self.full() {
                return;
            }
            self.budget = self.budget.saturating_sub(1);
            if matches!(kind, ObjType::Map | ObjType::Table | ObjType::List) {
                self.object(doc, &child, kind, depth.saturating_add(1));
            }
        }
    }

    /// Add the line of `object` when it is a message: who sent it beside
    /// what it says. Returns whether it is one; what a message holds
    /// besides (its reactions, its earlier texts) is not looked into.
    fn message(&mut self, doc: &impl ReadDoc, object: &ObjId) -> bool {
        let written = SENDER_KEYS
            .iter()
            .find_map(|key| string_at(doc, object, key));
        let author = string_at(doc, object, AUTHOR_KEY);
        if written.is_none() && author.is_none() {
            return false;
        }
        let said: Vec<String> = SAID_KEYS
            .iter()
            .filter_map(|key| string_at(doc, object, key))
            .map(|said| said.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|said| !said.is_empty())
            .collect();
        if said.is_empty() {
            return true;
        }
        let said = said.join(" — ");
        let sender = written.map(|from| from.trim().to_owned());
        let line = match (sender, author) {
            (Some(from), _) if !from.is_empty() => format!("{from}: {said}"),
            (_, Some(id)) if !id.trim().is_empty() && !id.contains(PARTICIPANT_MARK) => {
                format!("{PARTICIPANT_MARK}{}{PARTICIPANT_MARK}{said}", id.trim())
            }
            _ => said,
        };
        if !self.lines.is_empty() {
            self.lines.push('\n');
        }
        self.lines.push_str(&line);
        true
    }
}

/// The objects map or list `object` holds.
fn child_objects(doc: &impl ReadDoc, object: &ObjId) -> Vec<(ObjType, ObjId)> {
    let object_of = |value: ValueRef<'_>, id: ObjId| match value {
        ValueRef::Object(kind) => Some((kind, id)),
        ValueRef::Scalar(_) => None,
    };
    match doc.object_type(object) {
        Ok(ObjType::Map | ObjType::Table) => doc
            .map_range(object, ..)
            .filter_map(|item| object_of(item.value.clone(), item.id()))
            .collect(),
        Ok(ObjType::List) => doc
            .list_range(object, ..)
            .filter_map(|item| object_of(item.value.clone(), item.id()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The string at `key` of map `object`: a string value or a text object.
fn string_at(doc: &impl ReadDoc, object: &ObjId, key: &str) -> Option<String> {
    match doc.get(object, key).ok()?? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        (Value::Object(ObjType::Text), id) => doc.text(&id).ok(),
        (Value::Object(_), _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    /// A collection item as `tilo-resource` keeps it: its order key beside
    /// its value.
    fn item(doc: &mut automerge::AutoCommit, at: &ObjId, id: &str, order: &str) -> ObjId {
        let entry = doc.put_object(at, id, ObjType::Map).unwrap();
        doc.put(&entry, "k", order).unwrap();
        doc.put_object(&entry, "v", ObjType::Map).unwrap()
    }

    #[test]
    fn messages_are_read_with_their_sender_and_nothing_else_is() {
        let mut doc = automerge::AutoCommit::new();
        let people = doc
            .put_object(automerge::ROOT, "people", ObjType::Map)
            .unwrap();
        let ben = item(&mut doc, &people, "ben-5920f2", "B");
        doc.put(&ben, "id", "ben-5920f2").unwrap();
        doc.put(&ben, "name", "Ben").unwrap();
        doc.put(&ben, "initials", "B").unwrap();
        let messages = doc
            .put_object(automerge::ROOT, "messages", ObjType::Map)
            .unwrap();
        let post = item(&mut doc, &messages, "m1", "j");
        doc.put(&post, "author", "rae-cf1531").unwrap();
        doc.put(&post, "channel", "c_logins").unwrap();
        doc.put(&post, "text", "LR-2041\ncode entered").unwrap();
        let history = doc.put_object(&post, "history", ObjType::List).unwrap();
        let earlier = doc.insert_object(&history, 0, ObjType::Map).unwrap();
        doc.put(&earlier, "author", "rae-cf1531").unwrap();
        doc.put(&earlier, "text", "LR-2041 typo").unwrap();
        let sms = item(&mut doc, &messages, "m2", "k");
        doc.put(&sms, "from", "Northwind Bank").unwrap();
        doc.put(&sms, "body", "Your code is 615204.").unwrap();
        doc.put(&sms, "date", "2026-10-14T08:31:05Z").unwrap();
        let mail = item(&mut doc, &messages, "m3", "l");
        doc.put(&mail, "from", "Dana <dana@example.test>").unwrap();
        doc.put(&mail, "subject", "Offsite").unwrap();
        let body = doc.put_object(&mail, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "Book the venue.").unwrap();
        let written_here = item(&mut doc, &messages, "m4", "m");
        doc.put(&written_here, "from", "").unwrap();
        doc.put(&written_here, "body", "On my way").unwrap();

        let text = message_text(&doc);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "\u{1f}rae-cf1531\u{1f}LR-2041 code entered",
                "Northwind Bank: Your code is 615204.",
                "Dana <dana@example.test>: Offsite — Book the venue.",
                "On my way",
            ]
        );
        let names = participant_names(&doc);
        assert_eq!(names.get("ben-5920f2").map(String::as_str), Some("Ben"));
        let name = |id: &str| (id == "rae-cf1531").then(|| "Rae".to_owned());
        assert_eq!(shown_line(lines[0], name), "Rae: LR-2041 code entered");
        assert_eq!(shown_line(lines[0], |_| None), "LR-2041 code entered");
        assert_eq!(shown_line(lines[1], name), lines[1]);
        assert!(names_participant(lines[0]) && !names_participant(lines[1]));
    }

    #[test]
    fn a_roster_and_read_marks_hold_no_message() {
        let mut doc = automerge::AutoCommit::new();
        let people = doc
            .put_object(automerge::ROOT, "people", ObjType::Map)
            .unwrap();
        let ops = item(&mut doc, &people, "ops-cf14dc", "i");
        doc.put(&ops, "id", "ops-cf14dc").unwrap();
        doc.put(&ops, "name", "Mara Lind (IT)").unwrap();
        let read = doc
            .put_object(automerge::ROOT, "read", ObjType::Map)
            .unwrap();
        let marks = doc.put_object(&read, "ops-cf14dc", ObjType::Map).unwrap();
        let mark = doc.put_object(&marks, "c_logins", ObjType::Map).unwrap();
        doc.put(&mark, "part", 0_i64).unwrap();
        doc.put(&mark, "seen", 1_i64).unwrap();
        assert_eq!(message_text(&doc), "");
    }
}
