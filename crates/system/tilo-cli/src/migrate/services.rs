//! Outline and Linear as migration sources, through the installer's own service sources
//! ([`migration_sources::service`]): the same connection check, review, journal and resume.
//!
//! Their keys are typed by the person and kept in this computer's account vault; the review,
//! the journal and every request to the service happen here. What comes over lands as the
//! agent's run: an Outline document becomes a Notes page (`import-markdown`, under the id its
//! source decides, then `move-note` under its parent), an issue from Linear one in Tasks (`create`,
//! then `add-label`, `comment` and `move-under`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, anyhow, bail};
use migration_sources::service::{
    self, IncomingIssue, IncomingPage, ServiceAddress, ServiceJournal, ServiceKind, ServiceReview,
};
use serde_json::{Value, json};

use super::Lander;
use super::state::{Item, Place, Review};

pub const OUTLINE: &str = "outline";
pub const LINEAR: &str = "linear";

const IMPORT_MARKDOWN: &str = "notes/notes.notebook#import-markdown";
const MOVE_NOTE: &str = "notes/notes.notebook#move-note";
const CREATE_ISSUE: &str = "tasks/tasks.tracker#create";
const ADD_LABEL: &str = "tasks/tasks.tracker#add-label";
const COMMENT: &str = "tasks/tasks.tracker#comment";
const MOVE_UNDER: &str = "tasks/tasks.tracker#move-under";

fn kind(source: &str) -> Result<ServiceKind> {
    ServiceKind::from_key(source)
        .filter(|kind| *kind != ServiceKind::Mail)
        .ok_or_else(|| anyhow!("{source:?} is not a service `tilo migrate` connects"))
}

fn config_root() -> Result<std::path::PathBuf> {
    service::config_root().ok_or_else(|| anyhow!("this account has no Tilo config directory"))
}

/// The account a connected `source` is reached with.
fn account(source: &str) -> Result<String> {
    let kind = kind(source)?;
    service::connected(&config_root()?)
        .into_iter()
        .find(|(_, connected, _)| *connected == kind)
        .map(|(id, _, _)| id.as_str().to_owned())
        .ok_or_else(|| anyhow!("{source} is not connected: `tilo migrate connect {source}`, which asks the person for its key"))
}

/// `tilo migrate connect`: check the key the person types and keep it in the vault.
pub fn connect(source: &str, url: Option<&str>, key_file: Option<&Path>) -> Result<Value> {
    let kind = kind(source)?;
    let server = url.unwrap_or_default().to_owned();
    if kind == ServiceKind::Outline && server.is_empty() {
        bail!("Outline is reached at its own address: --url https://your-team.getoutline.com");
    }
    let what = match kind {
        ServiceKind::Outline => "Outline API key",
        _ => "Linear API key",
    };
    let secret = super::secret(&format!("{what} (it is not shown): "), key_file)?;
    if secret.is_empty() {
        bail!("no key was typed");
    }
    let account = service::connect(
        &config_root()?,
        ServiceAddress {
            kind,
            server,
            login: String::new(),
            secret,
        },
    )
    .map_err(|error| anyhow!("{source}: {error}"))?;
    Ok(json!({
        "source": source,
        "said": format!("{source} is connected (its key is in this computer's vault); next: tilo migrate review {source}"),
        "account": account.as_str(),
    }))
}

/// The connected services, as `tilo migrate sources` lists them.
pub fn sources() -> Vec<Value> {
    let Some(root) = service::config_root() else {
        return Vec::new();
    };
    let mut found: Vec<Value> = service::connected(&root)
        .into_iter()
        .filter(|(_, kind, _)| *kind != ServiceKind::Mail)
        .map(|(_, kind, title)| {
            json!({"source": kind.key(), "title": title, "next": format!("tilo migrate review {}", kind.key())})
        })
        .collect();
    for (kind, url) in [(OUTLINE, " --url https://…"), (LINEAR, "")] {
        if !found.iter().any(|source| source["source"] == kind) {
            found.push(json!({
                "source": kind,
                "title": "not connected",
                "next": format!("tilo migrate connect {kind}{url}"),
            }));
        }
    }
    found
}

fn journal_path(place: &Place, source: &str) -> std::path::PathBuf {
    place.path(source, "service.json")
}

fn keep_service(place: &Place, source: &str, journal: &ServiceJournal) -> Result<()> {
    place.keep(&journal_path(place, source), journal)
}

fn service_journal(place: &Place, source: &str) -> Result<Option<ServiceJournal>> {
    place.read(&journal_path(place, source))
}

/// Review the connected `source` now, keeping what was chosen and what already came over.
pub fn review(place: &Place, source: &str) -> Result<Review> {
    let account = account(source)?;
    let mut fresh =
        service::review(&config_root()?, &account).map_err(|error| anyhow!("{source}: {error}"))?;
    let journal = match service_journal(place, source)? {
        Some(mut journal) => {
            // A group the person left out stays left out.
            for group in &mut fresh.groups {
                if let Some(old) = journal
                    .review
                    .groups
                    .iter()
                    .find(|old| old.key == group.key)
                {
                    group.chosen = old.chosen;
                }
            }
            journal.reviewed_again(fresh);
            journal
        }
        None => ServiceJournal::new(fresh),
    };
    keep_service(place, source, &journal)?;
    Ok(of(source, &journal.review))
}

/// The review as `tilo migrate` shows it: each group with its count.
fn of(source: &str, review: &ServiceReview) -> Review {
    let (kind, counts) = match review.kind {
        ServiceKind::Outline => ("collection", "documents"),
        _ => ("team", "issues"),
    };
    Review {
        source: source.to_owned(),
        title: review.title.clone(),
        handle: review.account.clone(),
        items: review
            .groups
            .iter()
            .map(|group| Item {
                key: group.title.clone(),
                kind: kind.into(),
                title: group.title.clone(),
                count: group.items.len(),
                counts: counts.into(),
                within: None,
                chosen: group.chosen,
            })
            .collect(),
    }
}

/// Carry the person's choices in `review` into the service's own journal.
pub fn choose(place: &Place, review: &Review) -> Result<()> {
    let mut journal = service_journal(place, &review.source)?
        .ok_or_else(|| super::not_reviewed(&review.source))?;
    for item in &review.items {
        if let Some(group) = journal
            .review
            .groups
            .iter_mut()
            .find(|group| group.title == item.key)
        {
            group.chosen = item.chosen;
        }
    }
    keep_service(place, &review.source, &journal)
}

/// What a run brings, for its History entry.
#[must_use]
pub fn summary(review: &Review) -> String {
    let chosen = review.chosen();
    let count: usize = chosen.iter().map(|item| item.count).sum();
    let what = chosen.first().map_or("items", |item| item.counts.as_str());
    let source = if review.source == OUTLINE {
        "Outline"
    } else {
        "Linear"
    };
    format!(
        "From {source}: {} ({count} {what})",
        chosen
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Bring the chosen groups of `review` over.
pub fn land(review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    let place = lander.place.clone();
    let source = review.source.clone();
    let mut journal =
        service_journal(&place, &source)?.ok_or_else(|| super::not_reviewed(&source))?;
    journal.recovered();
    let ids_path = place.path(&source, "ids.json");
    let ids: BTreeMap<String, String> = place.read(&ids_path)?.unwrap_or_default();
    let cancel = AtomicBool::new(false);
    lander.open(if source == OUTLINE { "notes" } else { "tasks" });
    let mut landing = Landing {
        lander,
        ids,
        failure: None,
    };
    let mut save = |journal: &ServiceJournal| -> Result<(), String> {
        place
            .keep(&journal_path(&place, &source), journal)
            .map_err(|error| error.to_string())
    };
    let result = service::run(
        &config_root()?,
        &mut journal,
        &mut landing,
        &mut save,
        &cancel,
    );
    place.keep(&ids_path, &landing.ids)?;
    if let Some(error) = landing.failure.take() {
        return Err(error);
    }
    let report = result.map_err(|error| anyhow!("{source}: {error}"))?;
    landing.lander.count("", 0, report.already_there)?;
    Ok(())
}

/// The workspace's apps, as a service source's run lands in them.
struct Landing<'a, 'b> {
    lander: &'a mut Lander<'b>,
    /// The Tasks issue each issue from Linear became, by the issue's page.
    ids: BTreeMap<String, String>,
    /// Why the run stopped, when the workspace did (rather than the service).
    failure: Option<anyhow::Error>,
}

impl Landing<'_, '_> {
    fn fail(&mut self, error: anyhow::Error) -> std::io::Error {
        let said = format!("{error:#}");
        self.failure = Some(error);
        std::io::Error::other(said)
    }

    fn stop_check(&mut self) -> std::io::Result<()> {
        if self.lander.stopping() {
            let _ = self.lander.place.stop_asked(self.lander.source, true);
            return Err(self.fail(super::Stopped.into()));
        }
        Ok(())
    }
}

impl service::Landing for Landing<'_, '_> {
    fn page(&mut self, page: &IncomingPage) -> std::io::Result<()> {
        self.stop_check()?;
        let result = (|| -> Result<()> {
            let (workspace, run) = self.lander.workspace()?;
            let mut images = Vec::new();
            for picture in &page.pictures {
                images.push(workspace.put(&picture.bytes)?);
            }
            let note = note_for(&page.source, &page.key);
            let markdown = if page.url.is_empty() {
                page.markdown.clone()
            } else {
                format!("{}\n\n[{}]({})", page.markdown, "Open in Outline", page.url)
            };
            match workspace.act(
                run,
                IMPORT_MARKDOWN,
                json!({"title": page.title, "markdown": markdown, "images": images, "note": note}),
            ) {
                Ok(_) => {}
                // A page an earlier run brought is the person's now, as they left it.
                Err(error) if format!("{error:#}").contains("imported already") => return Ok(()),
                Err(error) => return Err(error),
            }
            if let Some(parent) = &page.parent {
                let parent = note_for(&page.source, parent);
                workspace.act(
                    run,
                    MOVE_NOTE,
                    json!({"page": note, "parent": parent, "before": null}),
                )?;
            }
            Ok(())
        })();
        result.map_err(|error| self.fail(error))?;
        self.lander
            .count("pages", 1, 0)
            .map_err(|error| self.fail(error))
    }

    fn issue(&mut self, issue: &IncomingIssue) -> std::io::Result<()> {
        self.stop_check()?;
        let result = (|| -> Result<String> {
            let (workspace, run) = self.lander.workspace()?;
            let created = workspace.act(
                run,
                CREATE_ISSUE,
                json!({
                    "title": format!("{} {}", issue.identifier, issue.title).trim(),
                    "body": issue.body,
                    "status": issue.status,
                    "assignee": issue.assignee,
                    "url": issue.url,
                    "parent": null,
                }),
            )?;
            let id = created_id(&created).ok_or_else(|| {
                anyhow!(
                    "Tasks made {} but did not say its id: {created}",
                    issue.identifier
                )
            })?;
            for label in &issue.labels {
                workspace.act(run, ADD_LABEL, json!({"issue": id, "label": label}))?;
            }
            for comment in &issue.comments {
                let text = format!("{} ({}): {}", comment.author, comment.at, comment.text);
                workspace.act(run, COMMENT, json!({"issue": id, "text": text}))?;
            }
            Ok(id)
        })();
        let id = result.map_err(|error| self.fail(error))?;
        self.ids.insert(issue.url.clone(), id.clone());
        if let Some(parent) = issue
            .parent
            .as_ref()
            .and_then(|parent| self.ids.get(parent))
            .cloned()
        {
            let moved = self.lander.workspace().and_then(|(workspace, run)| {
                workspace.act(run, MOVE_UNDER, json!({"issue": id, "parent": parent}))
            });
            moved.map_err(|error| self.fail(error))?;
        }
        self.lander
            .count("issues", 1, 0)
            .map_err(|error| self.fail(error))
    }
}

/// The id Tasks answered for the issue it made: `-> tasks.tracker/issues/<id>`.
fn created_id(answer: &str) -> Option<String> {
    if let Some((_, after)) = answer.split_once("/issues/") {
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    let body = answer.trim().strip_prefix("ok").unwrap_or(answer).trim();
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        for key in ["id", "issue"] {
            if let Some(id) = value.get(key).and_then(Value::as_str) {
                return Some(id.to_owned());
            }
        }
        if let Value::String(id) = value {
            return Some(id);
        }
    }
    body.split(|c: char| c.is_whitespace() || matches!(c, ',' | '"' | '(' | ')'))
        .find(|word| uuid::Uuid::parse_str(word).is_ok())
        .map(str::to_owned)
}

/// The note a source's page is kept under: Notes' own rule for a migration's pages
/// (`note_for` in `apps/notes/src/import_inbox.rs`), so the installer and this command bring the
/// same page to the same note.
#[must_use]
pub fn note_for(source: &str, key: &str) -> String {
    let digest = blake3::hash(format!("{source}\u{0}{key}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(digest.as_bytes().get(..16).unwrap_or(&[0; 16]));
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_answers_name_the_issue_they_made() {
        let id = "0f0e0d0c-0b0a-4908-8706-050403020100";
        assert_eq!(
            created_id(&format!("ok {{\"id\":\"{id}\"}}")).as_deref(),
            Some(id)
        );
        assert_eq!(created_id(&format!("ok created {id}")).as_deref(), Some(id));
        assert_eq!(created_id("ok"), None);
        assert_eq!(
            created_id("ok\n-> tasks.tracker/issues/f4055b9d9830a78c21b8\nlink: http://x")
                .as_deref(),
            Some("f4055b9d9830a78c21b8")
        );
    }

    #[test]
    fn a_page_is_kept_under_the_same_note_every_run() {
        assert_eq!(note_for("outline", "doc-1"), note_for("outline", "doc-1"));
        assert_ne!(note_for("outline", "doc-1"), note_for("outline", "doc-2"));
        assert!(uuid::Uuid::parse_str(&note_for("outline", "doc-1")).is_ok());
    }
}
