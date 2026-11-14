//! Add the issues a migration left for this app: Linear's issues.
//!
//! The migration writes each issue it brings into this app's own data area
//! (`personal-data-incoming/issues`). Opening the tracker adds them through its own verbs:
//! `create` with the description, status, assignee and the issue's page as its source, then
//! `add-label`, `comment` for each message of its thread, `move-under` for its parent and `link`
//! for the issues its relations name. An issue is known by its source page, so one the tracker
//! already holds is left as the person has it; an entry is removed once its issue is in.

use std::collections::BTreeMap;

use serde_json::Value;
use ui::application::HostContext;
use ui::tilo_resource::{Id, NonBlank, Replica};

use crate::model::{Status, Tracker, tracker};

const INBOX: &str = "personal-data-incoming/issues";
const VERSION: u64 = 1;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const ISSUE_KIND: &str = "tasks.tracker.issues";

pub(crate) fn import(context: &HostContext) -> Result<(), String> {
    let files = context.files();
    let data = files.data().map_err(|error| error.to_string())?;
    let Ok(inbox) = tilo_files::VPath::new(INBOX) else {
        return Ok(());
    };
    let Ok(listed) = tilo_files::blocking::list(&data, &inbox) else {
        return Ok(());
    };
    let mut entries = Vec::new();
    let mut names = Vec::new();
    for entry in listed {
        if entry.kind != tilo_files::Kind::File
            || !std::path::Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let path = inbox.join(&name);
        let Ok(Some(bytes)) = tilo_files::blocking::read_bounded(&data, &path, MAX_ENTRY_BYTES)
        else {
            continue;
        };
        let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if parsed.get("version").and_then(Value::as_u64) != Some(VERSION) {
            continue;
        }
        entries.push(parsed);
        names.push(path);
    }
    if entries.is_empty() {
        return Ok(());
    }
    let author = context.author().0.clone();
    context.migrate_record(move |doc| apply(doc, &entries, &author))?;
    for name in names {
        let _ = tilo_files::blocking::remove(&data, &name);
    }
    Ok(())
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn texts(value: &Value, name: &str) -> Vec<String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The tracker's issues by their source page.
fn by_page(replica: &Replica<Tracker>) -> Result<BTreeMap<String, Id>, String> {
    Ok(replica
        .state()
        .map_err(|error| error.to_string())?
        .issues
        .iter()
        .filter_map(|(id, issue)| issue.url.clone().map(|url| (url, id.clone())))
        .collect())
}

/// Add the `entries` the tracker does not hold yet.
pub(crate) fn apply(
    doc: &mut automerge::AutoCommit,
    entries: &[Value],
    author: &str,
) -> Result<bool, String> {
    let mut replica = Replica::<Tracker>::from_doc(doc.clone()).as_author(author);
    let held = by_page(&replica)?;
    let fresh: Vec<&Value> = entries
        .iter()
        .filter(|entry| {
            let url = text(entry, "url");
            !url.is_empty()
                && !held.contains_key(url)
                && NonBlank::parse(text(entry, "title")).is_some()
        })
        .collect();
    if fresh.is_empty() {
        return Ok(false);
    }
    for entry in &fresh {
        let Some(title) = NonBlank::parse(text(entry, "title")) else {
            continue;
        };
        let status = serde_json::from_value::<Status>(entry["status"].clone()).ok();
        replica
            .apply_confirmed(
                Tracker::CREATE.with(tracker::Create {
                    title,
                    parent: None,
                    body: Some(described(entry)),
                    status,
                    assignee: entry
                        .get("assignee")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    url: Some(text(entry, "url").to_owned()),
                }),
            )
            .map_err(|error| error.to_string())?;
    }
    let issues = by_page(&replica)?;
    for entry in &fresh {
        if let Some(issue) = issues.get(text(entry, "url")) {
            fill(&mut replica, entry, issue, &issues, entries)?;
        }
    }
    *doc = replica.doc_mut().clone();
    Ok(true)
}

/// Give the new `issue` what `entry` holds beyond its fields: labels, thread, parent and the
/// issues it relates to, found among `issues` by their source pages.
fn fill(
    replica: &mut Replica<Tracker>,
    entry: &Value,
    issue: &Id,
    issues: &BTreeMap<String, Id>,
    entries: &[Value],
) -> Result<(), String> {
    for label in texts(entry, "labels") {
        if let Some(label) = NonBlank::parse(&label) {
            replica
                .apply_confirmed(Tracker::ADD_LABEL.with(tracker::AddLabel {
                    issue: issue.clone(),
                    label,
                }))
                .map_err(|error| error.to_string())?;
        }
    }
    for comment in entry
        .get("comments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let said = comment_text(comment);
        if let Some(said) = NonBlank::parse(&said) {
            replica
                .apply_confirmed(Tracker::COMMENT.with(tracker::Comment {
                    issue: issue.clone(),
                    text: said,
                }))
                .map_err(|error| error.to_string())?;
        }
    }
    if let Some(parent) = entry
        .get("parent")
        .and_then(Value::as_str)
        .and_then(|page| issues.get(page))
    {
        replica
            .apply_confirmed(Tracker::MOVE_UNDER.with(tracker::MoveUnder {
                issue: issue.clone(),
                parent: Some(parent.clone()),
            }))
            .map_err(|error| error.to_string())?;
    }
    for page in texts(entry, "links") {
        let Some(related) = issues.get(&page) else {
            continue;
        };
        let (Some(app), Some(resource)) = (
            NonBlank::parse(crate::model::APP_ID),
            NonBlank::parse(&crate::model::issue_resource(related)),
        ) else {
            continue;
        };
        let label = entries
            .iter()
            .find(|other| text(other, "url") == page)
            .map_or_else(
                || page.clone(),
                |other| text(other, "identifier").to_owned(),
            );
        replica
            .apply_confirmed(Tracker::LINK.with(tracker::Link {
                issue: issue.clone(),
                app,
                kind: ISSUE_KIND.to_owned(),
                resource,
                label,
                record: None,
            }))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The description, ending with where the issue came from, so whoever reads it can open the
/// original.
fn described(entry: &Value) -> String {
    let body = text(entry, "body").trim();
    let source = match text(entry, "source") {
        "linear" => "Linear",
        other => other,
    };
    let named = match text(entry, "identifier") {
        "" => format!("Opened from {source}"),
        identifier => format!("{identifier} on {source}"),
    };
    let link = format!("[{named}]({})", text(entry, "url"));
    if body.is_empty() {
        link
    } else {
        format!("{body}\n\n{link}")
    }
}

/// A comment as the thread keeps it: who wrote it on the source and when, then its words.
fn comment_text(comment: &Value) -> String {
    let author = text(comment, "author").trim();
    let day = text(comment, "at").get(..10).unwrap_or_default();
    let said = text(comment, "text").trim();
    // The thread shows a message as written, so who wrote it reads as plain words.
    let head = match (author.is_empty(), day.is_empty()) {
        (false, false) => format!("{author}, {day}: "),
        (false, true) => format!("{author}: "),
        _ => String::new(),
    };
    let mut whole = format!("{head}{said}");
    if whole.chars().count() > crate::model::MAX_MESSAGE_CHARS {
        whole = whole
            .chars()
            .take(crate::model::MAX_MESSAGE_CHARS)
            .collect();
    }
    whole
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(identifier: &str, extra: Value) -> Value {
        let mut entry = json!({
            "version": 1, "source": "linear", "key": identifier, "identifier": identifier,
            "title": format!("Issue {identifier}"), "body": "About it.", "status": "backlog",
            "labels": [], "assignee": null, "comments": [], "links": [], "parent": null,
            "url": format!("https://tracker.example/acme/issue/{identifier}")
        });
        if let (Some(entry), Value::Object(extra)) = (entry.as_object_mut(), extra) {
            entry.extend(extra);
        }
        entry
    }

    #[test]
    fn migrated_issues_keep_state_labels_assignee_thread_links_and_source_once() {
        let entries = vec![
            entry(
                "ENG-2",
                json!({"status": "todo", "parent": "https://tracker.example/acme/issue/ENG-1"}),
            ),
            entry(
                "ENG-1",
                json!({
                    "status": "in-progress",
                    "assignee": "person:Ada Lovelace",
                    "labels": ["Migration", "P1"],
                    "comments": [{"author": "Grace", "at": "2026-01-03T00:00:00.000Z", "text": "On it."}],
                    "links": ["https://tracker.example/acme/issue/ENG-2"]
                }),
            ),
        ];
        let mut replica = Replica::<Tracker>::new();
        let mut doc = replica.doc_mut().clone();
        assert!(apply(&mut doc, &entries, "person:ada").unwrap());
        let tracker = <Tracker as ui::tilo_resource::Resource>::read(&doc).unwrap();
        let find = |identifier: &str| {
            tracker
                .issues
                .iter()
                .find(|(_, issue)| {
                    issue.url.as_deref()
                        == Some(format!("https://tracker.example/acme/issue/{identifier}").as_str())
                })
                .map(|(id, issue)| (id.clone(), issue.clone()))
                .unwrap()
        };
        let (first_id, first) = find("ENG-1");
        assert_eq!(first.title, "Issue ENG-1");
        let body = format!("{:?}", first.body);
        assert!(
            body.contains("About it.") && body.contains("https://tracker.example/acme/issue/ENG-1"),
            "{body}"
        );
        assert_eq!(first.status, Status::InProgress);
        assert_eq!(
            first.assignee.as_ref().map(|a| a.0.as_str()),
            Some("person:Ada Lovelace")
        );
        assert!(
            first.labels.contains(&"Migration".to_owned())
                && first.labels.contains(&"P1".to_owned())
        );
        let thread: Vec<String> = first.comments.iter().map(|(_, m)| m.text.clone()).collect();
        assert_eq!(thread, ["Grace, 2026-01-03: On it."]);
        let (second_id, second) = find("ENG-2");
        assert_eq!(second.status, Status::Todo);
        assert_eq!(second.parent, Some(first_id));
        let links: Vec<(String, String)> = first
            .links
            .iter()
            .map(|(_, link)| (link.label.clone(), link.resource.clone()))
            .collect();
        assert_eq!(
            links,
            [("ENG-2".to_owned(), crate::model::issue_resource(&second_id))]
        );

        assert!(
            !apply(&mut doc, &entries, "person:ada").unwrap(),
            "nothing lands twice"
        );
        let again = <Tracker as ui::tilo_resource::Resource>::read(&doc).unwrap();
        assert_eq!(again.issues.len(), 2);
    }
}
