//! Forgejo (and Gitea, whose API it shares) as a migration source: the issues of its
//! repositories, through its REST API (`GET /api/v1/…` with an access token in
//! `Authorization: token …`), grouped by repository for the review.
//!
//! The address the person connects names what is reviewed: a repository
//! (`https://forge.example/acme/app`), everything of an owner (`https://forge.example/acme`) or
//! every repository the token's user reaches (`https://forge.example`). Pull requests stay where
//! they are: only issues come over.
//!
//! An issue keeps its title, description, open or closed state, labels, assignee, milestone,
//! due date and repository, who opened it and when (the original author when the forge itself
//! migrated it from elsewhere), its comments with theirs, the issues it blocks, depends on or
//! names as `#12`, and its page on the forge as the issue's source.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::http::{self, Key};
use super::landing::{
    IncomingAuthor, IncomingComment, IncomingGroup, IncomingIssue, IncomingRelation,
};
use super::{ReviewGroup, ReviewItem, ServiceError};

/// The source name issues keep.
pub const SOURCE: &str = "forgejo";
const PAGE: usize = 50;

/// An address the person typed, as the forge's base and what of it is reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The forge itself: `https://forge.example`.
    pub server: String,
    /// `acme/app`, `acme`, or empty for every repository the token reaches.
    pub path: String,
}

/// Split `address` into the forge and the owner or repository it names.
///
/// # Errors
/// It is not an http(s) address, or names more than an owner and a repository.
pub fn scope(address: &str) -> Result<Scope, ServiceError> {
    let base = http::base(address)?;
    let parsed = url::Url::parse(&base).map_err(|_| ServiceError::InvalidAddress)?;
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect())
        .unwrap_or_default();
    let segments: Vec<&str> = match segments.as_slice() {
        [owner, repo, "issues", ..] => vec![owner, repo],
        _ => segments,
    };
    if segments.len() > 2 {
        return Err(ServiceError::InvalidAddress);
    }
    let mut server = parsed.clone();
    server.set_path("");
    Ok(Scope {
        server: server.as_str().trim_end_matches('/').to_owned(),
        path: segments
            .iter()
            .map(|part| part.trim_end_matches(".git"))
            .collect::<Vec<_>>()
            .join("/"),
    })
}

fn get(server: &str, key: &str, path: &str) -> Result<Option<Value>, ServiceError> {
    http::get_json(&format!("{server}/api/v1/{path}"), key, Key::Token)
}

/// Every entry of a paged list at `path` (which already has a query), or `None` when the forge
/// has no such endpoint (its first page is a 404).
fn listed_optional(
    server: &str,
    key: &str,
    path: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Option<Vec<Value>>, ServiceError> {
    let mut all: Vec<Value> = Vec::new();
    let mut seen = BTreeSet::new();
    for page in 1.. {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err(ServiceError::Cancelled);
        }
        let Some(answer) = get(server, key, &format!("{path}&limit={PAGE}&page={page}"))? else {
            // A missing first page: the forge does not offer this endpoint at all.
            return Ok((page > 1).then_some(all));
        };
        let entries = answer.as_array().cloned().unwrap_or_default();
        let count = entries.len();
        let mut new = 0;
        for entry in entries {
            // A list the forge does not page answers the same entries again.
            if seen.insert(entry.get("id").map(Value::to_string).unwrap_or_default()) {
                all.push(entry);
                new += 1;
            }
        }
        if all.len() > super::MAX_REVIEW_ITEMS {
            return Err(ServiceError::TooLarge);
        }
        if count < PAGE || new == 0 {
            break;
        }
    }
    Ok(Some(all))
}

/// Every entry of a paged list at `path`, none when the forge has no such endpoint.
fn listed(
    server: &str,
    key: &str,
    path: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, ServiceError> {
    Ok(listed_optional(server, key, path, cancel)?.unwrap_or_default())
}

/// Who the token belongs to and what the address names, as the source's title.
///
/// # Errors
/// The forge refused the token, or has no such owner or repository.
pub fn check(scope: &Scope, key: &str) -> Result<String, ServiceError> {
    let user = get(&scope.server, key, "user")?
        .ok_or_else(|| ServiceError::Unavailable("the address is not a Forgejo".into()))?;
    if user.get("login").and_then(Value::as_str).is_none() {
        return Err(ServiceError::Unavailable(
            "the address is not a Forgejo".into(),
        ));
    }
    if !scope.path.is_empty() {
        repositories(scope, key)?;
    }
    let host = url::Url::parse(&scope.server)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(if scope.path.is_empty() {
        format!("Forgejo: {host}")
    } else {
        format!("Forgejo: {host}/{}", scope.path)
    })
}

/// The repositories the scope names that keep issues.
fn repositories(scope: &Scope, key: &str) -> Result<Vec<Value>, ServiceError> {
    let repositories = match scope.path.split_once('/') {
        Some(_) => vec![
            get(&scope.server, key, &format!("repos/{}", scope.path))?
                .ok_or(ServiceError::Missing)?,
        ],
        None if scope.path.is_empty() => listed(&scope.server, key, "user/repos?", None)?,
        None => {
            let owner = &scope.path;
            match get(&scope.server, key, &format!("orgs/{owner}"))? {
                Some(_) => listed(&scope.server, key, &format!("orgs/{owner}/repos?"), None)?,
                None => listed(&scope.server, key, &format!("users/{owner}/repos?"), None)?,
            }
        }
    };
    Ok(repositories
        .into_iter()
        .filter(|repository| {
            repository.get("has_issues").and_then(Value::as_bool) != Some(false)
                && !text(repository, "full_name").is_empty()
        })
        .collect())
}

fn issues_of(
    scope: &Scope,
    key: &str,
    repository: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<Value>, ServiceError> {
    listed(
        &scope.server,
        key,
        &format!("repos/{repository}/issues?state=all&type=issues"),
        cancel,
    )
}

/// The repositories as groups, each listing its issues.
///
/// # Errors
/// The forge refused the token or could not be read.
pub fn review(scope: &Scope, key: &str) -> Result<Vec<ReviewGroup>, ServiceError> {
    let mut groups = Vec::new();
    let mut total = 0;
    for repository in repositories(scope, key)? {
        let name = text(&repository, "full_name").to_owned();
        let items: Vec<ReviewItem> = issues_of(scope, key, &name, None)?
            .iter()
            .map(|issue| ReviewItem {
                key: id(issue),
                title: text(issue, "title").to_owned(),
                facts: vec![identifier(&name, issue), text(issue, "state").to_owned()],
            })
            .collect();
        total += items.len();
        if total > super::MAX_REVIEW_ITEMS {
            return Err(ServiceError::TooLarge);
        }
        if items.is_empty() {
            continue;
        }
        groups.push(ReviewGroup {
            key: id(&repository),
            title: name,
            items,
            chosen: true,
        });
    }
    Ok(groups)
}

pub struct Issues {
    pub issues: Vec<IncomingIssue>,
    pub not_reviewed: usize,
    pub gone: Vec<String>,
}

/// An issue's blocks and its dependencies, in that order.
type Relations = (Vec<Value>, Vec<Value>);
/// The blocks and dependencies of chosen issues, by repository and issue number.
type Related = BTreeMap<(String, u64), Relations>;

/// The reviewed, chosen issues as the forge holds them now.
///
/// # Errors
/// The forge refused the token, could not be read, or the run was cancelled.
pub fn issues(
    scope: &Scope,
    key: &str,
    chosen: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<Issues, ServiceError> {
    let (source, not_reviewed) = chosen_issues(scope, key, chosen, cancel)?;
    // The page of every chosen issue, by its repository and number.
    let pages: BTreeMap<(String, u64), String> = source
        .iter()
        .map(|(repository, issue)| {
            (
                (text(repository, "full_name").to_owned(), number(issue)),
                text(issue, "html_url").to_owned(),
            )
        })
        .collect();
    let comments = comments_of(scope, key, &source, cancel)?;
    let related = relations_of(scope, key, &source, cancel)?;
    let issues = assembled(&source, &pages, &comments, &related, cancel)?;
    let found: BTreeSet<&str> = issues.iter().map(|issue| issue.key.as_str()).collect();
    let gone = chosen
        .iter()
        .filter(|key| !found.contains(key.as_str()))
        .cloned()
        .collect();
    Ok(Issues {
        issues,
        not_reviewed,
        gone,
    })
}

/// The chosen issues, each with its repository, and how many reviewed ones were left out.
fn chosen_issues(
    scope: &Scope,
    key: &str,
    chosen: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<(Vec<(Value, Value)>, usize), ServiceError> {
    let mut source: Vec<(Value, Value)> = Vec::new();
    let mut not_reviewed = 0;
    for repository in repositories(scope, key)? {
        let name = text(&repository, "full_name").to_owned();
        let issues = issues_of(scope, key, &name, Some(cancel))?;
        let reviewed = issues.iter().any(|issue| chosen.contains(&id(issue)));
        for issue in issues {
            if chosen.contains(&id(&issue)) {
                source.push((repository.clone(), issue));
            } else if reviewed {
                not_reviewed += 1;
            }
        }
    }
    Ok((source, not_reviewed))
}

/// Every chosen issue's comments, by repository and issue number: one paged listing per
/// repository, keyed back to its issue, rather than a round trip per issue.
fn comments_of(
    scope: &Scope,
    key: &str,
    source: &[(Value, Value)],
    cancel: &AtomicBool,
) -> Result<BTreeMap<(String, u64), Vec<Value>>, ServiceError> {
    let commented: Vec<String> = source
        .iter()
        .filter(|(_, issue)| issue.get("comments").and_then(Value::as_u64).unwrap_or(0) > 0)
        .map(|(repository, _)| text(repository, "full_name").to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let listed = http::each(&commented, |name| {
        listed_optional(
            &scope.server,
            key,
            &format!("repos/{name}/issues/comments?"),
            Some(cancel),
        )
    });
    let mut comments: BTreeMap<(String, u64), Vec<Value>> = BTreeMap::new();
    for (name, answer) in commented.iter().zip(listed) {
        for comment in answer?.unwrap_or_default() {
            let issue = text(&comment, "issue_url")
                .rsplit_once('/')
                .and_then(|(_, number)| number.parse::<u64>().ok());
            if let Some(issue) = issue {
                comments
                    .entry((name.clone(), issue))
                    .or_default()
                    .push(comment);
            }
        }
    }
    Ok(comments)
}

/// Every chosen issue's blocks and dependencies, by repository and issue number, asked together.
///
/// Blocks and dependencies are a request per issue for each. Asking one issue of a repository
/// says whether the forge keeps them at all (it answers 404 when they are turned off), and those
/// answers are that issue's own, so nothing is asked twice.
fn relations_of(
    scope: &Scope,
    key: &str,
    source: &[(Value, Value)],
    cancel: &AtomicBool,
) -> Result<Related, ServiceError> {
    let first: Vec<(&str, u64)> = source
        .iter()
        .map(|(repository, issue)| (text(repository, "full_name"), number(issue)))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    let asked = http::each(&first, |(name, issue)| {
        let blocks = listed_optional(
            &scope.server,
            key,
            &format!("repos/{name}/issues/{issue}/blocks?"),
            Some(cancel),
        )?;
        let dependencies = listed_optional(
            &scope.server,
            key,
            &format!("repos/{name}/issues/{issue}/dependencies?"),
            Some(cancel),
        )?;
        Ok::<_, ServiceError>((blocks, dependencies))
    });
    let mut related: Related = BTreeMap::new();
    let mut keeps_relations: BTreeSet<&str> = BTreeSet::new();
    for ((name, issue), answer) in first.iter().zip(asked) {
        let (blocks, dependencies) = answer?;
        if blocks.is_some() || dependencies.is_some() {
            keeps_relations.insert(name);
            related.insert(
                ((*name).to_owned(), *issue),
                (blocks.unwrap_or_default(), dependencies.unwrap_or_default()),
            );
        }
    }
    let rest: Vec<(&str, u64, &str)> = source
        .iter()
        .filter(|(repository, issue)| {
            let name = text(repository, "full_name");
            keeps_relations.contains(name)
                && !related.contains_key(&(name.to_owned(), number(issue)))
        })
        .flat_map(|(repository, issue)| {
            [("blocks", "blocks"), ("dependencies", "blocked-by")]
                .map(|(path, _)| (text(repository, "full_name"), number(issue), path))
        })
        .collect();
    let listed = http::each(&rest, |(name, issue, path)| {
        listed_optional(
            &scope.server,
            key,
            &format!("repos/{name}/issues/{issue}/{path}?"),
            Some(cancel),
        )
    });
    for ((name, issue, path), answer) in rest.iter().zip(listed) {
        let found = answer?.unwrap_or_default();
        let (blocks, dependencies) = related.entry(((*name).to_owned(), *issue)).or_default();
        if *path == "blocks" {
            blocks.extend(found);
        } else {
            dependencies.extend(found);
        }
    }
    Ok(related)
}

/// The chosen issues as items, their relations by page and their comments, in the order gathered.
fn assembled(
    source: &[(Value, Value)],
    pages: &BTreeMap<(String, u64), String>,
    comments: &BTreeMap<(String, u64), Vec<Value>>,
    related: &Related,
    cancel: &AtomicBool,
) -> Result<Vec<IncomingIssue>, ServiceError> {
    let mut issues = Vec::with_capacity(source.len());
    for (repository, issue) in source {
        if cancel.load(Ordering::Relaxed) {
            return Err(ServiceError::Cancelled);
        }
        let name = text(repository, "full_name");
        let comments = comments
            .get(&(name.to_owned(), number(issue)))
            .map_or(&[][..], Vec::as_slice);
        let (blocks, dependencies) = related
            .get(&(name.to_owned(), number(issue)))
            .cloned()
            .unwrap_or_default();
        let mut relations = Vec::new();
        for (kind, found) in [("blocks", blocks), ("blocked-by", dependencies)] {
            for other in found {
                let repo = other
                    .pointer("/repository/full_name")
                    .and_then(Value::as_str)
                    .unwrap_or(name);
                if let Some(page) = pages.get(&(repo.to_owned(), number(&other))) {
                    relations.push(IncomingRelation {
                        kind: kind.into(),
                        page: page.clone(),
                    });
                }
            }
        }
        for mentioned in mentions(text(issue, "body")) {
            if mentioned == number(issue) {
                continue;
            }
            if let Some(page) = pages.get(&(name.to_owned(), mentioned))
                && !relations.iter().any(|known| known.page == *page)
            {
                relations.push(IncomingRelation {
                    kind: "related".into(),
                    page: page.clone(),
                });
            }
        }
        issues.push(incoming(repository, issue, comments, relations));
    }
    Ok(issues)
}

/// The issue numbers `body` names as `#12`.
fn mentions(body: &str) -> BTreeSet<u64> {
    let mut found = BTreeSet::new();
    let bytes = body.as_bytes();
    for (at, _) in body.match_indices('#') {
        let before = at.checked_sub(1).and_then(|before| bytes.get(before));
        if before.is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'&' || *byte == b'/')
        {
            continue;
        }
        let digits: String = body
            .get(at + 1..)
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(number) = digits.parse::<u64>() {
            found.insert(number);
        }
    }
    found
}

fn incoming(
    repository: &Value,
    issue: &Value,
    comments: &[Value],
    relations: Vec<IncomingRelation>,
) -> IncomingIssue {
    let name = text(repository, "full_name");
    let assignee = issue
        .get("assignees")
        .and_then(Value::as_array)
        .and_then(|assignees| assignees.first())
        .or_else(|| issue.get("assignee").filter(|assignee| !assignee.is_null()))
        .and_then(|user| author(Some(user), ""))
        // Someone named on the forge need not be in the workspace: named, not a member.
        .map(|user| format!("outside:{}", user.name));
    let milestone = issue
        .get("milestone")
        .filter(|milestone| !milestone.is_null())
        .map(|milestone| IncomingGroup {
            key: format!("milestone:{}", id(milestone)),
            name: text(milestone, "title").trim().to_owned(),
        });
    let due = text(issue, "due_date")
        .get(..10)
        .filter(|day| jiff::civil::Date::strptime("%Y-%m-%d", day).is_ok())
        .map(str::to_owned);
    IncomingIssue {
        source: SOURCE.into(),
        key: id(issue),
        identifier: identifier(name, issue),
        title: text(issue, "title").to_owned(),
        body: text(issue, "body").to_owned(),
        status: if text(issue, "state") == "closed" {
            "done"
        } else {
            "todo"
        }
        .to_owned(),
        labels: issue
            .get("labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .map(|label| text(label, "name").trim().to_owned())
                    .filter(|label| !label.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        assignee,
        comments: comments
            .iter()
            .filter(|comment| !text(comment, "body").trim().is_empty())
            .map(|comment| {
                let user = author(comment.get("user"), text(comment, "original_author"));
                IncomingComment {
                    author: user
                        .as_ref()
                        .map(|user| user.name.clone())
                        .unwrap_or_default(),
                    handle: user.and_then(|user| user.handle),
                    at: text(comment, "created_at").to_owned(),
                    updated_at: text(comment, "updated_at").to_owned(),
                    url: text(comment, "html_url").to_owned(),
                    text: text(comment, "body").to_owned(),
                }
            })
            .collect(),
        links: relations
            .iter()
            .map(|relation| relation.page.clone())
            .collect(),
        parent: None,
        url: text(issue, "html_url").to_owned(),
        author: author(issue.get("user"), text(issue, "original_author")),
        created_at: text(issue, "created_at").to_owned(),
        updated_at: text(issue, "updated_at").to_owned(),
        team: Some(IncomingGroup {
            key: id(repository),
            name: name.to_owned(),
        }),
        project: milestone,
        priority: None,
        estimate: None,
        due,
        relations,
    }
}

/// How the forge names an issue to people: `app#12` (the repository's own name).
fn identifier(repository: &str, issue: &Value) -> String {
    let short = repository.rsplit('/').next().unwrap_or(repository);
    format!("{short}#{}", number(issue))
}

/// A forge user as an author: their full name (else their login), and their login as the
/// handle. An issue the forge itself migrated names its first author in `original`.
fn author(user: Option<&Value>, original: &str) -> Option<IncomingAuthor> {
    if !original.trim().is_empty() {
        return Some(IncomingAuthor {
            name: original.trim().to_owned(),
            handle: None,
        });
    }
    let user = user?;
    let login = text(user, "login").trim();
    let full = text(user, "full_name").trim();
    let name = if full.is_empty() { login } else { full };
    if name.is_empty() {
        return None;
    }
    Some(IncomingAuthor {
        name: name.to_owned(),
        handle: (!login.is_empty()).then(|| login.to_owned()),
    })
}

fn id(value: &Value) -> String {
    value.get("id").map(Value::to_string).unwrap_or_default()
}

fn number(issue: &Value) -> u64 {
    issue.get("number").and_then(Value::as_u64).unwrap_or(0)
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}
