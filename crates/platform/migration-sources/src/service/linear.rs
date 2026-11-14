//! Linear as a migration source: its issues, through Linear's GraphQL API (`POST /graphql`
//! with a personal API key in `Authorization`), grouped by team for the review.
//!
//! An issue keeps its title, description, state, labels, assignee, comments, the issues its
//! relations name and its parent, and its page on Linear as the issue's source.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::http::{self, Key};
use super::landing::{IncomingComment, IncomingIssue};
use super::{ReviewGroup, ReviewItem, ServiceError};

const ISSUES: &str = r"
query TiloMigration($after: String) {
  issues(first: 100, after: $after, includeArchived: false) {
    pageInfo { hasNextPage endCursor }
    nodes {
      id
      identifier
      title
      description
      url
      createdAt
      team { id key name }
      state { name type }
      assignee { name }
      labels { nodes { name } }
      parent { id }
      relations { nodes { relatedIssue { id } } }
      comments(first: 100) { nodes { body createdAt user { name } } }
    }
  }
}
";

fn query(server: &str, key: &str, body: &Value) -> Result<Value, ServiceError> {
    let answer = http::post_json(&format!("{server}/graphql"), body, key, Key::Raw)?;
    if answer.get("errors").is_some_and(|errors| !errors.is_null()) {
        let refused = answer
            .pointer("/errors/0/extensions/code")
            .and_then(Value::as_str)
            .is_some_and(|code| code.eq_ignore_ascii_case("AUTHENTICATION_ERROR"));
        return Err(if refused {
            ServiceError::Refused
        } else {
            ServiceError::Unavailable("Linear refused the query".into())
        });
    }
    Ok(answer)
}

/// The workspace the key belongs to, as Linear names it.
pub fn check(server: &str, key: &str) -> Result<String, ServiceError> {
    let answer = query(
        server,
        key,
        &json!({ "query": "query { viewer { name } organization { name } }" }),
    )?;
    let organization = answer
        .pointer("/data/organization/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(if organization.is_empty() {
        "Linear".to_owned()
    } else {
        format!("Linear: {organization}")
    })
}

fn all_issues(
    server: &str,
    key: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, ServiceError> {
    let mut issues = Vec::new();
    let mut after: Option<String> = None;
    loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err(ServiceError::Cancelled);
        }
        let answer = query(
            server,
            key,
            &json!({ "query": ISSUES, "variables": { "after": after } }),
        )?;
        let connection = answer
            .pointer("/data/issues")
            .ok_or_else(|| ServiceError::Unavailable("Linear answered without issues".into()))?;
        issues.extend(
            connection
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        if issues.len() > super::MAX_REVIEW_ITEMS {
            return Err(ServiceError::TooLarge);
        }
        let more = connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        after = connection
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !more || after.is_none() {
            return Ok(issues);
        }
    }
}

/// The teams as groups, each listing its issues.
pub fn review(server: &str, key: &str) -> Result<Vec<ReviewGroup>, ServiceError> {
    let mut teams: BTreeMap<String, ReviewGroup> = BTreeMap::new();
    for issue in all_issues(server, key, None)? {
        let team = issue.get("team").unwrap_or(&Value::Null);
        let group = teams
            .entry(text(team, "id").to_owned())
            .or_insert_with(|| ReviewGroup {
                key: text(team, "id").to_owned(),
                title: text(team, "name").to_owned(),
                items: Vec::new(),
                chosen: true,
            });
        group.items.push(ReviewItem {
            key: text(&issue, "id").to_owned(),
            title: text(&issue, "title").to_owned(),
            facts: [
                text(&issue, "identifier"),
                issue
                    .pointer("/state/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ]
            .into_iter()
            .filter(|fact| !fact.is_empty())
            .map(str::to_owned)
            .collect(),
        });
    }
    Ok(teams.into_values().collect())
}

pub struct Issues {
    pub issues: Vec<IncomingIssue>,
    pub not_reviewed: usize,
    pub gone: Vec<String>,
}

/// The reviewed, chosen issues as Linear holds them now, parents before their sub-issues.
pub fn issues(
    server: &str,
    key: &str,
    chosen: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<Issues, ServiceError> {
    let source = all_issues(server, key, Some(cancel))?;
    let reviewed_teams: BTreeSet<&str> = source
        .iter()
        .filter(|issue| chosen.contains(text(issue, "id")))
        .filter_map(|issue| issue.pointer("/team/id").and_then(Value::as_str))
        .collect();
    let not_reviewed = source
        .iter()
        .filter(|issue| {
            !chosen.contains(text(issue, "id"))
                && issue
                    .pointer("/team/id")
                    .and_then(Value::as_str)
                    .is_some_and(|team| reviewed_teams.contains(team))
        })
        .count();
    let pages: BTreeMap<&str, &str> = source
        .iter()
        .filter(|issue| chosen.contains(text(issue, "id")))
        .map(|issue| (text(issue, "id"), text(issue, "url")))
        .collect();
    let mut issues: Vec<IncomingIssue> = source
        .iter()
        .filter(|issue| chosen.contains(text(issue, "id")))
        .map(|issue| incoming(issue, &pages))
        .collect();
    let found: BTreeSet<&str> = issues.iter().map(|issue| issue.key.as_str()).collect();
    let gone = chosen
        .iter()
        .filter(|key| !found.contains(key.as_str()))
        .cloned()
        .collect();
    issues = parents_first(issues);
    Ok(Issues {
        issues,
        not_reviewed,
        gone,
    })
}

fn parents_first(issues: Vec<IncomingIssue>) -> Vec<IncomingIssue> {
    let keys: BTreeSet<String> = issues.iter().map(|issue| issue.url.clone()).collect();
    let mut placed = BTreeSet::new();
    let mut ordered = Vec::with_capacity(issues.len());
    let mut rest = issues;
    while !rest.is_empty() {
        let (ready, waiting): (Vec<_>, Vec<_>) = rest.into_iter().partition(|issue| {
            issue
                .parent
                .as_ref()
                .is_none_or(|parent| placed.contains(parent) || !keys.contains(parent))
        });
        if ready.is_empty() {
            ordered.extend(waiting);
            break;
        }
        placed.extend(ready.iter().map(|issue| issue.url.clone()));
        ordered.extend(ready);
        rest = waiting;
    }
    ordered
}

/// `issue` for Tasks, naming the reviewed issues it relates to and sits under by their pages:
/// `pages` maps each reviewed issue's id to its page.
fn incoming(issue: &Value, pages: &BTreeMap<&str, &str>) -> IncomingIssue {
    let nodes = |pointer: &str| -> Vec<Value> {
        issue
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let assignee = issue
        .pointer("/assignee/name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("person:{name}"));
    IncomingIssue {
        source: "linear".into(),
        key: text(issue, "id").to_owned(),
        identifier: text(issue, "identifier").to_owned(),
        title: text(issue, "title").to_owned(),
        body: text(issue, "description").to_owned(),
        status: status(issue.pointer("/state/type").and_then(Value::as_str)).to_owned(),
        labels: nodes("/labels/nodes")
            .iter()
            .map(|label| text(label, "name").trim().to_owned())
            .filter(|label| !label.is_empty())
            .collect(),
        assignee,
        comments: nodes("/comments/nodes")
            .iter()
            .filter(|comment| !text(comment, "body").trim().is_empty())
            .map(|comment| IncomingComment {
                author: comment
                    .pointer("/user/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                at: text(comment, "createdAt").to_owned(),
                text: text(comment, "body").to_owned(),
            })
            .collect(),
        links: nodes("/relations/nodes")
            .iter()
            .filter_map(|relation| relation.pointer("/relatedIssue/id").and_then(Value::as_str))
            .filter_map(|related| pages.get(related))
            .map(|page| (*page).to_owned())
            .collect(),
        parent: issue
            .pointer("/parent/id")
            .and_then(Value::as_str)
            .and_then(|parent| pages.get(parent))
            .map(|page| (*page).to_owned()),
        url: text(issue, "url").to_owned(),
    }
}

/// Linear's state type, as the tracker's status.
fn status(kind: Option<&str>) -> &'static str {
    match kind {
        Some("completed") => "done",
        Some("canceled") => "cancelled",
        Some("started") => "in-progress",
        Some("unstarted") => "todo",
        _ => "backlog",
    }
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}
