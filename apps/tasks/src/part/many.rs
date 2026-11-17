//! A list of new issues made in one step: what `create-many` checks and
//! fills in where it is invoked, and the part the list goes to.
//!
//! The verb applies `create`'s own code to each issue of the list, so the two
//! never differ in what they refuse or record. What an issue of the list can
//! be refused for is checked here first, where every part is held, and
//! refused naming the entry: a list is made whole or not at all. The issues
//! go into one part together, so they are one step of its history: the newest
//! part while it has room for all of them, else the next ([`part_for_new`]).

use std::collections::HashMap;
use std::rc::Rc;

use ui::tilo_resource::items::Ref;
use ui::tilo_resource::{self, Origin, Refusal};

use super::{Held, IssuePart, PART_ISSUES, PART_TEXT_BYTES};
use crate::model::{ListedIssue, MAX_MANY, MAX_TITLE_CHARS, Tracker, checked_assignee};

/// Refuse a list that is empty, or longer than one call takes.
pub(super) fn check_count(count: usize) -> Result<(), Refusal> {
    if count == 0 {
        return Err(Refusal::new("Give the issues: the list is empty"));
    }
    if count > MAX_MANY {
        return Err(Refusal::new(format!(
            "A call takes at most {MAX_MANY} issues: give them in several calls"
        )));
    }
    Ok(())
}

/// The refusal of the entry at `at` (counted from 0) that reads `name`, for
/// `why`: a list is refused whole, and says which entry it is refused for.
pub(super) fn entry(at: usize, name: &str, why: impl std::fmt::Display) -> Refusal {
    Refusal::new(format!(
        "issue {} in the list ({name}): {why}",
        at.saturating_add(1)
    ))
}

/// For each issue of a list of new ones, the place (counted from 0) of the
/// issue of the same list it stands under: the one its `under` names by key,
/// or by its place counted from 1.
///
/// # Errors
/// Refuses a key that is given twice or reads as a number, an issue that
/// names both a `parent` and an `under`, and an `under` naming no issue
/// before it in the list.
pub(super) fn nesting(issues: &[ListedIssue]) -> Result<Vec<Option<usize>>, Refusal> {
    let mut keyed: HashMap<&str, usize> = HashMap::new();
    let mut under = Vec::with_capacity(issues.len());
    for (at, issue) in issues.iter().enumerate() {
        let title = issue.title.as_str();
        let parent = match issue.under.as_deref().map(str::trim) {
            None => None,
            Some(_) if issue.parent.is_some() => {
                return Err(entry(
                    at,
                    title,
                    "it stands under one issue: name it in `parent` when the tracker has it already, or in `under` when this list makes it",
                ));
            }
            Some(named) => {
                let place = keyed.get(named).copied().or_else(|| {
                    named
                        .parse::<usize>()
                        .ok()
                        .and_then(|place| place.checked_sub(1))
                });
                match place {
                    Some(place) if place < at => Some(place),
                    Some(_) => return Err(not_before(at, title, named)),
                    None if keys_later(issues, at, named) => {
                        return Err(not_before(at, title, named));
                    }
                    None => {
                        return Err(entry(
                            at,
                            title,
                            format!(
                                "`under` names {named}, which no issue of the list has as its `key`"
                            ),
                        ));
                    }
                }
            }
        };
        under.push(parent);
        if let Some(key) = issue.key.as_deref().map(str::trim) {
            if key.is_empty() || key.parse::<usize>().is_ok() {
                return Err(entry(
                    at,
                    title,
                    "its `key` is a name, not a number: a number names a place in the list",
                ));
            }
            if keyed.insert(key, at).is_some() {
                return Err(entry(
                    at,
                    title,
                    format!("its `key` {key} names an issue earlier in the list already"),
                ));
            }
        }
    }
    Ok(under)
}

/// Whether the issue at `at` or one after it has the key `named`.
fn keys_later(issues: &[ListedIssue], at: usize, named: &str) -> bool {
    issues
        .iter()
        .skip(at)
        .any(|issue| issue.key.as_deref().map(str::trim) == Some(named))
}

fn not_before(at: usize, title: &str, named: &str) -> Refusal {
    entry(
        at,
        title,
        format!(
            "`under` names {named}, which is not before it in the list: put the issue it stands under first"
        ),
    )
}

/// What `create` refuses of one issue, checked before any issue of the list
/// is made.
pub(super) fn check_listed(issue: &ListedIssue) -> Result<(), Refusal> {
    if issue.title.as_str().trim().chars().count() > MAX_TITLE_CHARS {
        return Err(Refusal::new("An issue's title fits on a line"));
    }
    super::check_estimate(issue.estimate)?;
    Origin::checked_option(issue.origin.clone())?;
    checked_assignee(issue.assignee.clone())?;
    Ok(())
}

/// The text a list of new issues holds, in bytes: titles and descriptions.
fn list_text(issues: &[ListedIssue]) -> usize {
    issues
        .iter()
        .map(|issue| {
            issue
                .title
                .as_str()
                .len()
                .saturating_add(issue.body.as_deref().map_or(0, str::len))
        })
        .fold(0, usize::saturating_add)
}

/// The part a list of new issues goes to, whole: the newest part while it
/// has room for all of them, else the next. A part that holds nothing yet
/// takes any list.
pub(super) fn part_for_new(issues: &[ListedIssue], parts: &[(u64, Rc<IssuePart>)]) -> u64 {
    let Some((index, head)) = parts.last() else {
        return 0;
    };
    let fits = head.issues.len().saturating_add(issues.len()) <= PART_ISSUES
        && head.text_bytes().saturating_add(list_text(issues)) <= PART_TEXT_BYTES;
    if fits || head.issues.is_empty() {
        *index
    } else {
        index.saturating_add(1)
    }
}

/// Check a list of new issues where it is invoked and fill in what each
/// reads of the tracker: the issue it stands under when the tracker has it
/// already, the project it works towards, and its place in the tracker's
/// order, each after the one before it in the list.
pub(super) fn prepare_create(
    issues: &mut [ListedIssue],
    tracker: &Tracker,
    held: &Held<'_>,
) -> Result<(), String> {
    check_count(issues.len()).map_err(|refusal| refusal.to_string())?;
    nesting(issues).map_err(|refusal| refusal.to_string())?;
    if list_text(issues) > PART_TEXT_BYTES {
        return Err(
            "This is more text than one step takes: give the issues in several calls".into(),
        );
    }
    let mut rank = held.last_rank();
    for (at, issue) in issues.iter_mut().enumerate() {
        let title = issue.title.as_str().to_owned();
        let refuse = |why: String| entry(at, &title, why).to_string();
        check_listed(issue).map_err(|refusal| refuse(refusal.to_string()))?;
        if let Some(parent) = issue.parent.as_mut() {
            *parent = Ref::from(held.named(parent.as_str()).map_err(&refuse)?);
        }
        issue.goal = issue
            .project
            .as_ref()
            .map(|project| super::goal_named(tracker, project))
            .transpose()
            .map_err(&refuse)?;
        let next = tilo_resource::order::between(Some(&rank), None);
        issue.rank = Some(std::mem::replace(&mut rank, next));
    }
    Ok(())
}
