//! Linear as a migration source: its issues, through Linear's GraphQL API (`POST /graphql`
//! with a personal API key in `Authorization`), grouped by team for the review.
//!
//! An issue keeps its title, description, state, labels, assignee, priority, estimate, due
//! date, team and project, who opened it and when, its comments with theirs, how it relates to
//! the other issues and its parent, and its page on Linear as the issue's source.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::http::{self, Key};
use super::landing::{
    IncomingAuthor, IncomingComment, IncomingGroup, IncomingIssue, IncomingRelation,
};
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
      updatedAt
      priority
      estimate
      dueDate
      creator { name email }
      team { id key name }
      project { id name }
      state { name type }
      assignee { name email }
      labels { nodes { name } }
      parent { id identifier url }
      relations { nodes { type relatedIssue { id identifier url } } }
      inverseRelations { nodes { type issue { id identifier url } } }
      comments(first: 100) {
        pageInfo { hasNextPage endCursor }
        nodes { body createdAt updatedAt url user { name email } }
      }
    }
  }
}
";

/// One issue's comments past the first page the issues query brings.
const COMMENTS: &str = r"
query TiloMigrationComments($id: String!, $after: String) {
  issue(id: $id) {
    comments(first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes { body createdAt updatedAt url user { name email } }
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
    let mut source = all_issues(server, key, Some(cancel))?;
    fill_comments(server, key, &mut source, chosen, cancel)?;
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

/// The comments past the first page of the issues query, appended to every chosen issue that has
/// more: all such issues are asked at once, and one issue's further pages one after another.
fn fill_comments(
    server: &str,
    key: &str,
    issues: &mut [Value],
    chosen: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<(), ServiceError> {
    let overfull: Vec<(usize, Value)> = issues
        .iter()
        .enumerate()
        .filter(|(_, issue)| chosen.contains(text(issue, "id")))
        .filter(|(_, issue)| {
            issue
                .pointer("/comments/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|(at, issue)| (at, issue.clone()))
        .collect();
    let more = http::each(&overfull, |(_, issue)| {
        further_comments(server, key, issue, cancel)
    });
    for ((at, _), answer) in overfull.iter().zip(more) {
        for comment in answer? {
            if let Some(nodes) = issues[*at]
                .pointer_mut("/comments/nodes")
                .and_then(Value::as_array_mut)
            {
                nodes.push(comment);
            }
        }
    }
    Ok(())
}

fn further_comments(
    server: &str,
    key: &str,
    issue: &Value,
    cancel: &AtomicBool,
) -> Result<Vec<Value>, ServiceError> {
    let id = text(issue, "id");
    let mut cursor = issue
        .pointer("/comments/pageInfo/endCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut seen = BTreeSet::new();
    let mut comments = Vec::new();
    loop {
        let Some(after) = cursor.take() else {
            return Ok(comments);
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(ServiceError::Cancelled);
        }
        // A connection that hands back a cursor it already gave ends here rather than twice.
        if !seen.insert(after.clone()) {
            return Ok(comments);
        }
        let answer = query(
            server,
            key,
            &json!({ "query": COMMENTS, "variables": { "id": id, "after": after } }),
        )?;
        let connection = answer
            .pointer("/data/issue/comments")
            .ok_or_else(|| ServiceError::Unavailable("Linear answered without comments".into()))?;
        comments.extend(
            connection
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let more = connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if more {
            cursor = connection
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }
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
        // Someone named on Linear need not be in the workspace: named, not a member.
        .map(|name| format!("outside:{name}"));
    let relations = relations(issue, pages);
    let group = |pointer: &str| {
        issue.pointer(pointer).and_then(|group| {
            let key = text(group, "id");
            (!key.is_empty()).then(|| IncomingGroup {
                key: key.to_owned(),
                name: text(group, "name").trim().to_owned(),
            })
        })
    };
    IncomingIssue {
        source: "linear".into(),
        key: text(issue, "id").to_owned(),
        identifier: text(issue, "identifier").to_owned(),
        title: text(issue, "title").to_owned(),
        body: with_outside(text(issue, "description"), &outside(issue, pages)),
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
            .map(|comment| {
                let user = author(comment.get("user"));
                IncomingComment {
                    author: user
                        .as_ref()
                        .map(|user| user.name.clone())
                        .unwrap_or_default(),
                    handle: user.and_then(|user| user.handle),
                    at: text(comment, "createdAt").to_owned(),
                    updated_at: text(comment, "updatedAt").to_owned(),
                    url: text(comment, "url").to_owned(),
                    text: text(comment, "body").to_owned(),
                }
            })
            .collect(),
        links: relations
            .iter()
            .map(|relation| relation.page.clone())
            .collect(),
        parent: issue
            .pointer("/parent/id")
            .and_then(Value::as_str)
            .and_then(|parent| pages.get(parent))
            .map(|page| (*page).to_owned()),
        url: text(issue, "url").to_owned(),
        author: author(issue.get("creator")),
        created_at: text(issue, "createdAt").to_owned(),
        updated_at: text(issue, "updatedAt").to_owned(),
        team: group("/team"),
        project: group("/project"),
        priority: priority(issue.get("priority").and_then(Value::as_u64)),
        estimate: issue
            .get("estimate")
            .and_then(Value::as_f64)
            .filter(|points| points.is_finite() && *points >= 0.0)
            .map(|points| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a finite, non-negative estimate, rounded and capped to whole points"
                )]
                let whole = points.round().min(f64::from(u32::MAX)) as u32;
                whole
            }),
        due: issue
            .get("dueDate")
            .and_then(Value::as_str)
            .filter(|due| !due.is_empty())
            .map(str::to_owned),
        relations,
    }
}

/// The issues `issue` sits under or relates to that this import does not bring (another team's,
/// archived, or left out in the review), each as how it relates and a link to its page on
/// Linear: what of them the issue keeps, as it cannot point at them in Tasks.
fn outside(issue: &Value, pages: &BTreeMap<&str, &str>) -> Vec<String> {
    let named = |other: &Value, how: &str| -> Option<String> {
        let page = text(other, "url");
        (!page.is_empty() && !pages.contains_key(text(other, "id"))).then(|| {
            let name = match text(other, "identifier") {
                "" => page,
                identifier => identifier,
            };
            format!("{how} [{name}]({page})")
        })
    };
    let nodes = |pointer: &str| -> Vec<Value> {
        issue
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut outside: Vec<String> = Vec::new();
    if let Some(parent) = issue.get("parent").filter(|parent| !parent.is_null()) {
        outside.extend(named(parent, "Sub-issue of"));
    }
    for relation in nodes("/relations/nodes") {
        let how = match text(&relation, "type") {
            "blocks" => "Blocks",
            "duplicate" => "Duplicate of",
            _ => "Related to",
        };
        if let Some(other) = relation.get("relatedIssue") {
            outside.extend(named(other, how));
        }
    }
    for relation in nodes("/inverseRelations/nodes") {
        let how = match text(&relation, "type") {
            "blocks" => "Blocked by",
            "duplicate" => "Duplicated by",
            _ => "Related to",
        };
        if let Some(other) = relation.get("issue") {
            outside.extend(named(other, how));
        }
    }
    outside.dedup();
    outside
}

/// `description` with the issues it relates to outside the import (`outside`) said after it.
fn with_outside(description: &str, outside: &[String]) -> String {
    if outside.is_empty() {
        return description.to_owned();
    }
    let said = format!("On Linear: {}", outside.join(", "));
    if description.trim().is_empty() {
        said
    } else {
        format!("{}\n\n{said}", description.trim_end())
    }
}

/// How `issue` relates to the reviewed issues (`pages`): the relations it names, and those
/// other issues name with it, each once.
fn relations(issue: &Value, pages: &BTreeMap<&str, &str>) -> Vec<IncomingRelation> {
    let nodes = |pointer: &str| -> Vec<Value> {
        issue
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut relations: Vec<IncomingRelation> = Vec::new();
    for relation in nodes("/relations/nodes") {
        let kind = match text(&relation, "type") {
            "blocks" => "blocks",
            "duplicate" => "duplicate-of",
            _ => "related",
        };
        if let Some(page) = relation
            .pointer("/relatedIssue/id")
            .and_then(Value::as_str)
            .and_then(|related| pages.get(related))
        {
            relations.push(IncomingRelation {
                kind: kind.into(),
                page: (*page).to_owned(),
            });
        }
    }
    for relation in nodes("/inverseRelations/nodes") {
        let kind = match text(&relation, "type") {
            "blocks" => "blocked-by",
            "duplicate" => "duplicated-by",
            _ => "related",
        };
        if let Some(page) = relation
            .pointer("/issue/id")
            .and_then(Value::as_str)
            .and_then(|related| pages.get(related))
            && !relations.iter().any(|known| known.page == *page)
        {
            relations.push(IncomingRelation {
                kind: kind.into(),
                page: (*page).to_owned(),
            });
        }
    }
    relations
}

/// A Linear user (`creator`, a comment's `user`) as an author: their name, and their address
/// as the handle.
fn author(user: Option<&Value>) -> Option<IncomingAuthor> {
    let user = user?;
    let name = text(user, "name").trim();
    if name.is_empty() {
        return None;
    }
    let handle = text(user, "email").trim();
    Some(IncomingAuthor {
        name: name.to_owned(),
        handle: (!handle.is_empty()).then(|| handle.to_owned()),
    })
}

/// Linear's priority (0 none, 1 urgent … 4 low) by name.
fn priority(level: Option<u64>) -> Option<String> {
    let named = match level? {
        1 => "urgent",
        2 => "high",
        3 => "medium",
        4 => "low",
        _ => return None,
    };
    Some(named.to_owned())
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
