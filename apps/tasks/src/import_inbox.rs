//! Add the issues a migration left for this app: Linear's issues.
//!
//! The migration writes each issue it brings into this app's own data area
//! (`personal-data-incoming/issues`). Opening the tracker adds them through its own verbs:
//! `create` with the description, status, assignee, labels, the issue's page as its source and
//! its origin (who opened it there, when, and the name it had there, `ENG-12`), then `comment`
//! for each message of its thread with its own origin, `move-under` for its parent and `link` for
//! the issues its relations name. An issue is known by its source page, so one the tracker
//! already holds is left as the person has it; an entry is removed once its issue is in.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;
use ui::application::ProcessContext;
use ui::framework::Chain;
use ui::tilo_resource::{Id, Invoke, NonBlank, Origin};

use crate::issue_list::{IssueList, Issues, issue_list};
use crate::model::{Status, Tracker};

const INBOX: &str = "personal-data-incoming/issues";
const VERSION: u64 = 1;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const ISSUE_KIND: &str = "tasks.issues.issues";

pub(crate) fn import(context: &ProcessContext) -> Result<(), String> {
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
        let Ok(bytes) = tilo_files::blocking::read_bounded(&data, &path, MAX_ENTRY_BYTES) else {
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
    let author = context.author().clone();
    // The issues go into the parts beside the tracker, through their own
    // verbs, as the window would carry them out.
    let chain =
        Chain::<IssueList>::detached_in(context.records().clone(), crate::APP_ID, &author, || {
            Rc::new(Tracker::default())
        })
        .ok_or_else(|| "the tracker's issues cannot be opened".to_owned())?;
    apply(&chain, &entries)?;
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
fn by_page(chain: &Chain<IssueList>) -> BTreeMap<String, Id> {
    let parts = chain.parts();
    Issues::of(&parts)
        .iter()
        .filter_map(|(id, issue)| issue.url.clone().map(|url| (url, id.clone())))
        .collect()
}

/// Carry `invoke` out on the issues.
fn invoke(chain: &Chain<IssueList>, invoke: &Invoke<IssueList>) -> Result<(), String> {
    chain
        .invoke(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Add the `entries` the tracker does not hold yet.
pub(crate) fn apply(chain: &Chain<IssueList>, entries: &[Value]) -> Result<bool, String> {
    let held = by_page(chain);
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
        invoke(
            chain,
            &IssueList::CREATE.with(crate::model::NewIssue {
                body: Some(text(entry, "body").trim().to_owned()),
                status,
                assignee: entry
                    .get("assignee")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                url: Some(text(entry, "url").to_owned()),
                origin: origin(
                    entry.get("author"),
                    entry,
                    text(entry, "url"),
                    Some(text(entry, "identifier")),
                    text(entry, "created_at"),
                    text(entry, "updated_at"),
                ),
                ..crate::model::NewIssue::titled(title)
            }),
        )?;
    }
    let issues = by_page(chain);
    for entry in &fresh {
        if let Some(issue) = issues.get(text(entry, "url")) {
            fill(chain, entry, issue, &issues, entries)?;
        }
    }
    Ok(true)
}

/// Give the new `issue` what `entry` holds beyond its fields: labels, thread, parent and the
/// issues it relates to, found among `issues` by their source pages.
fn fill(
    chain: &Chain<IssueList>,
    entry: &Value,
    issue: &Id,
    issues: &BTreeMap<String, Id>,
    entries: &[Value],
) -> Result<(), String> {
    for label in texts(entry, "labels") {
        if let Some(label) = NonBlank::parse(&label) {
            invoke(
                chain,
                &IssueList::ADD_LABEL.with(issue_list::AddLabel {
                    issue: issue.clone(),
                    label,
                }),
            )?;
        }
    }
    for comment in entry
        .get("comments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let said = comment_text(comment);
        let author = (!text(comment, "author").trim().is_empty()).then(|| {
            serde_json::json!({"name": text(comment, "author"), "handle": comment.get("handle")})
        });
        let url = match text(comment, "url") {
            "" => text(entry, "url"),
            url => url,
        };
        if let Some(said) = NonBlank::parse(&said) {
            invoke(
                chain,
                &IssueList::COMMENT.with(issue_list::Comment {
                    issue: issue.clone(),
                    text: said,
                    origin: origin(
                        author.as_ref(),
                        entry,
                        url,
                        None,
                        text(comment, "at"),
                        text(comment, "updated_at"),
                    ),
                }),
            )?;
        }
    }
    if let Some(parent) = entry
        .get("parent")
        .and_then(Value::as_str)
        .and_then(|page| issues.get(page))
    {
        invoke(
            chain,
            &IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                issue: issue.clone(),
                parent: Some(ui::tilo_resource::items::Ref::from(parent.clone())),
            }),
        )?;
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
        invoke(
            chain,
            &IssueList::LINK.with(issue_list::Link {
                issue: issue.clone(),
                app,
                kind: ISSUE_KIND.to_owned(),
                resource,
                label,
                record: None,
            }),
        )?;
    }
    Ok(())
}

/// Where an item of `entry` was first written: its author there (`{name, handle}`), the tool,
/// its page, the name it had there and when it was written and last changed. `None` when the
/// source does not say who wrote it.
fn origin(
    author: Option<&Value>,
    entry: &Value,
    url: &str,
    reference: Option<&str>,
    created: &str,
    updated: &str,
) -> Option<Origin> {
    let author = author?;
    let name = text(author, "name").trim();
    if name.is_empty() {
        return None;
    }
    let millis = |at: &str| {
        at.trim()
            .parse::<jiff::Timestamp>()
            .ok()
            .map(jiff::Timestamp::as_millisecond)
    };
    let created_ms = millis(created);
    let source = match text(entry, "source") {
        "linear" => "Linear",
        "forgejo" => "Forgejo",
        other => other,
    };
    let kept = |text: &str| (!text.trim().is_empty()).then(|| text.trim().to_owned());
    Origin {
        author: name.to_owned(),
        handle: author.get("handle").and_then(Value::as_str).and_then(kept),
        source: source.to_owned(),
        url: kept(url),
        reference: reference.and_then(kept),
        created_ms,
        updated_ms: millis(updated)
            .filter(|updated| created_ms.is_none_or(|made| *updated >= made)),
    }
    .checked()
    .ok()
}

/// A comment's words as the thread keeps them; who wrote it there and when is its origin.
fn comment_text(comment: &Value) -> String {
    let mut whole = text(comment, "text").trim().to_owned();
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
                    "assignee": "outside:Ada Lovelace",
                    "labels": ["Migration", "P1"],
                    "author": {"name": "Grace Hopper", "handle": "grace@example.com"},
                    "created_at": "2026-01-02T09:00:00.000Z",
                    "updated_at": "2026-01-04T09:00:00.000Z",
                    "comments": [{"author": "Grace", "at": "2026-01-03T00:00:00.000Z", "text": "On it."}],
                    "links": ["https://tracker.example/acme/issue/ENG-2"]
                }),
            ),
        ];
        let chain = Chain::<IssueList>::detached(
            crate::APP_ID,
            &ui::tilo_resource::MemberId::person("ada"),
            || Rc::new(Tracker::default()),
        )
        .unwrap();
        assert!(apply(&chain, &entries).unwrap());
        let parts = chain.parts();
        let issues = Issues::of(&parts);
        let find = |identifier: &str| {
            issues
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
        assert!(body.contains("About it."), "{body}");
        let origin = first.origin.clone().expect("who opened it there");
        assert_eq!(origin.author, "Grace Hopper");
        assert_eq!(origin.handle.as_deref(), Some("grace@example.com"));
        assert_eq!(origin.source, "Linear");
        assert_eq!(origin.reference.as_deref(), Some("ENG-1"));
        assert_eq!(
            origin.url.as_deref(),
            Some("https://tracker.example/acme/issue/ENG-1")
        );
        assert_eq!(origin.created_ms, Some(1_767_344_400_000));
        assert_eq!(first.status, Status::InProgress);
        assert_eq!(
            first.assignee.as_ref().map(|a| a.0.as_str()),
            Some("outside:Ada Lovelace")
        );
        assert!(
            first.labels.contains(&"Migration".to_owned())
                && first.labels.contains(&"P1".to_owned())
        );
        let thread: Vec<String> = first.comments.iter().map(|(_, m)| m.text.clone()).collect();
        assert_eq!(thread, ["On it."], "who wrote it and when is its origin");
        let said = first.comments.iter().next().unwrap().1;
        assert_eq!(
            said.origin.as_ref().map(|origin| origin.author.as_str()),
            Some("Grace")
        );
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

        assert!(!apply(&chain, &entries).unwrap(), "nothing lands twice");
        let parts = chain.parts();
        assert_eq!(Issues::of(&parts).len(), 2);
    }
}
