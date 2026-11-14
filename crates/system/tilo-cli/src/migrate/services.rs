//! Outline, Linear and Forgejo as migration sources, through the installer's own service sources
//! ([`migration_sources::service`]): the same connection check, review, journal and resume.
//!
//! Their keys are typed by the person and kept in this computer's account vault; the review,
//! the journal and every request to the service happen here. What comes over lands as the
//! agent's run: an Outline document becomes a Notes page (`import-markdown`, under the id its
//! source decides, then `move-note` under its parent), an issue from Linear or Forgejo one in
//! Tasks (`create` in the project or area its project or team, milestone or repository there
//! became, then `comment`, `move-under` and `link` for the issues it relates to). Each keeps its origin: who wrote it on the service, when,
//! its page there and the name the service gave it (`ENG-12`), so a person sees the original
//! author and date while History says who moved it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, anyhow, bail};
use migration_sources::service::{
    self, IncomingAuthor, IncomingGroup, IncomingIssue, IncomingPage, ServiceAddress,
    ServiceJournal, ServiceKind, ServiceReview,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Lander;
use super::state::{Item, Place, Review};

pub const OUTLINE: &str = "outline";
pub const LINEAR: &str = "linear";
pub const FORGEJO: &str = "forgejo";
/// A mail account on its server (IMAP), which joins the person's accounts with its password;
/// Mail then fetches its mail itself.
pub const MAIL: &str = "mail";

/// Where an email account joins the person's accounts (Mail's, which the person allows).
const ADD_ACCOUNT: &str = "mail/mail.mailbox#add-account";

const IMPORT_MARKDOWN: &str = "notes/notes.notebook#import-markdown";
const MOVE_NOTE: &str = "notes/notes.notebook#move-note";
const CREATE_ISSUE: &str = "tasks/tasks.tracker#create";
const CREATE_AREA: &str = "tasks/tasks.tracker#create-area";
const SET_PROJECT: &str = "tasks/tasks.tracker#set-project";
const COMMENT: &str = "tasks/tasks.tracker#comment";
const MOVE_UNDER: &str = "tasks/tasks.tracker#move-under";
const LINK: &str = "tasks/tasks.tracker#link";
/// How Tasks names the kind of an issue a link points at.
const ISSUE_KIND: &str = "tasks.tracker.issues";
const TASKS_APP: &str = "so.tilo.tasks";

fn kind(source: &str) -> Result<ServiceKind> {
    ServiceKind::from_key(source)
        .ok_or_else(|| anyhow!("{source:?} is not a service `tilo migrate` connects"))
}

fn config_root() -> Result<std::path::PathBuf> {
    service::config_root().ok_or_else(|| anyhow!("this account has no Tilo config directory"))
}

/// The account a connected `source` is reached with: for mail, the account connected last.
fn account(place: &Place, source: &str) -> Result<String> {
    let kind = kind(source)?;
    let connected = service::connected(&config_root()?);
    let last: Option<String> = if kind == ServiceKind::Mail {
        place.read(&place.path(source, "account.json"))?
    } else {
        None
    };
    connected
        .into_iter()
        .filter(|(_, connected, _)| *connected == kind)
        .map(|(id, _, _)| id.as_str().to_owned())
        .find(|id| last.as_ref().is_none_or(|last| last == id))
        .ok_or_else(|| {
            let how = if kind == ServiceKind::Mail {
                "its password"
            } else {
                "its key"
            };
            anyhow!("{source} is not connected: `tilo migrate connect {source}`, which asks the person for {how}")
        })
}

/// `tilo migrate connect`: check the key or password the person types and keep it in the vault.
pub fn connect(
    place: &Place,
    source: &str,
    url: Option<&str>,
    login: Option<&str>,
    key_file: Option<&Path>,
) -> Result<Value> {
    let kind = kind(source)?;
    let server = url.unwrap_or_default().to_owned();
    if kind == ServiceKind::Outline && server.is_empty() {
        bail!("Outline is reached at its own address: --url https://your-team.getoutline.com");
    }
    if kind == ServiceKind::Forgejo && server.is_empty() {
        bail!(
            "Forgejo is reached at the repository (or owner) whose issues come over: --url https://forge.example/owner/repo"
        );
    }
    let login = login.unwrap_or_default().trim().to_owned();
    if kind == ServiceKind::Mail && login.is_empty() {
        bail!("a mail account is named by its address: --login you@example.com");
    }
    let what = match kind {
        ServiceKind::Outline => "Outline API key".to_owned(),
        ServiceKind::Linear => "Linear API key".to_owned(),
        ServiceKind::Forgejo => "Forgejo access token".to_owned(),
        ServiceKind::Mail => format!("Password of {login}"),
    };
    let secret = super::secret(&format!("{what} (it is not shown): "), key_file)?;
    if secret.is_empty() {
        bail!("nothing was typed");
    }
    let account = service::connect(
        &config_root()?,
        ServiceAddress {
            kind,
            server,
            login,
            secret,
        },
    )
    .map_err(|error| anyhow!("{source}: {error}"))?;
    if kind == ServiceKind::Mail {
        place.keep(&place.path(source, "account.json"), &account.as_str())?;
    }
    let kept = if kind == ServiceKind::Mail {
        "its password"
    } else {
        "its key"
    };
    Ok(json!({
        "source": source,
        "said": format!("{source} is connected ({kept} is in this computer's vault); next: tilo migrate review {source}"),
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
        .map(|(_, kind, title)| {
            json!({"source": kind.key(), "title": title, "next": format!("tilo migrate review {}", kind.key())})
        })
        .collect();
    for (kind, url) in [
        (OUTLINE, " --url https://…"),
        (LINEAR, ""),
        (FORGEJO, " --url https://…/owner/repo"),
        (MAIL, " --login you@example.com"),
    ] {
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

/// How many of `source`'s items the workspace has, as its journal says.
///
/// # Errors
/// The journal could not be read.
pub fn landed(place: &Place, source: &str) -> Result<usize> {
    Ok(service_journal(place, source)?.map_or(0, |journal| journal.landed.len()))
}

/// Forget what the run `run` brought (`keys`, as [`Lander::note`] kept them), after the
/// workspace took it back: a later run brings it again.
///
/// # Errors
/// The journals could not be read or kept.
pub fn forget(
    place: &Place,
    source: &str,
    run: &str,
    keys: &std::collections::BTreeSet<String>,
) -> Result<()> {
    if let Some(mut journal) = service_journal(place, source)? {
        for key in keys {
            if let Some(item) = key.strip_prefix(NOTED) {
                journal.landed.remove(item);
            }
        }
        keep_service(place, source, &journal)?;
    }
    let kept_path = place.path(source, "ids.json");
    if let Some(mut kept) = place.read::<Kept>(&kept_path)? {
        kept.forget_run(run);
        place.keep(&kept_path, &kept)?;
    }
    Ok(())
}

/// How a run notes an item of a service source's own journal in its own.
const NOTED: &str = "service:";

/// Review the connected `source` now, keeping what was chosen and what already came over.
pub fn review(place: &Place, source: &str) -> Result<Review> {
    let account = account(place, source)?;
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
        ServiceKind::Linear => ("team", "issues"),
        ServiceKind::Forgejo => ("repository", "issues"),
        ServiceKind::Mail => ("mail-account", "accounts"),
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
    let source = match review.source.as_str() {
        OUTLINE => "Outline",
        MAIL => "mail",
        FORGEJO => "Forgejo",
        _ => "Linear",
    };
    let titles = chosen
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // A mail account is its own title: "1 account" after it says nothing more.
    if review.source == MAIL {
        return format!("From {source}: {titles}");
    }
    format!("From {source}: {titles} ({})", counted(count, what))
}

/// `count` of what `plural` names, in the singular for one: `1 issue`, `3 issues`.
pub(super) fn counted(count: usize, plural: &str) -> String {
    let what = match count {
        1 => plural.strip_suffix('s').unwrap_or(plural),
        _ => plural,
    };
    format!("{count} {what}")
}

/// Bring the chosen groups of `review` over.
pub fn land(review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    if review.source == MAIL {
        return land_mail(review, lander);
    }
    let place = lander.place.clone();
    let source = review.source.clone();
    let mut journal =
        service_journal(&place, &source)?.ok_or_else(|| super::not_reviewed(&source))?;
    journal.recovered();
    let kept_path = place.path(&source, "ids.json");
    let kept: Kept = place.read(&kept_path)?.unwrap_or_default();
    let cancel = AtomicBool::new(false);
    lander.open(if source == OUTLINE { "notes" } else { "tasks" });
    let mut landing = Landing {
        lander,
        kept,
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
    place.keep(&kept_path, &landing.kept)?;
    if let Some(error) = landing.failure.take() {
        return Err(error);
    }
    let report = result.map_err(|error| anyhow!("{source}: {error}"))?;
    landing.lander.count("", 0, report.already_there)?;
    Ok(())
}

/// The chosen mail account into the person's accounts: its settings, through Mail's
/// `add-account`, which the person allows in their tab. Its password stays where `connect` put
/// it, this computer's account vault, where Tilo's Mail here signs in with it. Mail fetches its
/// mail itself from then on.
fn land_mail(review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    if !review.chosen().iter().any(|item| item.key == review.handle) {
        return Ok(());
    }
    let (config, _password) = service::mail_account(&config_root()?, &review.handle)
        .map_err(|error| anyhow!("mail: {error}"))?;
    lander.open("mail");
    lander.once(
        &format!("account:{}", config.email),
        "accounts",
        |workspace, run| {
            let arguments = json!({
                "display_name": config.label(),
                "email": config.email,
                "username": config.username,
                "imap_host": config.imap_host,
                "imap_port": config.imap_port,
                "smtp_host": config.smtp_host,
                "smtp_port": config.smtp_port,
            });
            match workspace.act(run, ADD_ACCOUNT, arguments) {
                Ok(_) => Ok(super::Landed::One),
                Err(error) if format!("{error:#}").contains("already") => {
                    Ok(super::Landed::Already)
                }
                Err(error) => Err(super::thunderbird::mail_not_open(error, &config.email)),
            }
        },
    )
}

/// What a source's runs made in the workspace, kept beside its journal so a later run finds it:
/// the Tasks issue each issue became and the project or area each team and project became, by
/// their pages and keys, the links made between issues, and the links waiting for the issue
/// they point at.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Kept {
    /// The Tasks issue each issue became, by its page on the service.
    #[serde(default)]
    pub issues: BTreeMap<String, String>,
    /// The run that brought each issue, by its page.
    #[serde(default)]
    pub issue_runs: BTreeMap<String, String>,
    /// The project or area each project and team (or milestone and repository) became, by its
    /// key.
    #[serde(default)]
    pub areas: BTreeMap<String, KeptArea>,
    /// Links made, as `from-page\u{0}to-page`.
    #[serde(default)]
    pub links: std::collections::BTreeSet<String>,
    /// Relations whose other issue had not come over yet, by that issue's page: the page of the
    /// issue that relates to it, and how.
    #[serde(default)]
    pub waiting: BTreeMap<String, Vec<(String, String)>>,
}

/// A project or area a run made for a team, project, repository or milestone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeptArea {
    pub id: String,
    pub run: String,
}

impl Kept {
    /// Forget what the run `run` made, after it was taken back.
    pub fn forget_run(&mut self, run: &str) {
        let pages: Vec<String> = self
            .issue_runs
            .iter()
            .filter(|(_, by)| *by == run)
            .map(|(page, _)| page.clone())
            .collect();
        for page in &pages {
            self.issues.remove(page);
            self.issue_runs.remove(page);
            self.waiting.remove(page);
        }
        self.links.retain(|link| {
            !link
                .split('\u{0}')
                .any(|page| pages.iter().any(|gone| gone == page))
        });
        for waiting in self.waiting.values_mut() {
            waiting.retain(|(from, _)| !pages.contains(from));
        }
        self.areas.retain(|_, area| area.run != run);
    }
}

/// The workspace's apps, as a service source's run lands in them.
struct Landing<'a, 'b> {
    lander: &'a mut Lander<'b>,
    kept: Kept,
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

    /// The project or area `group` became, made now if no run made it yet: a project in Linear
    /// (a milestone in Forgejo) becomes a project, a team (a repository) an area, each named as
    /// the service names it.
    fn area(&mut self, group: &IncomingGroup, kind: &str, service: &str) -> Result<String> {
        let key = format!("{kind}:{}", group.key);
        if let Some(area) = self.kept.areas.get(&key) {
            return Ok(area.id.clone());
        }
        let name = if group.name.trim().is_empty() {
            service
        } else {
            group.name.trim()
        };
        let run = self.lander.run.id.clone();
        let (workspace, run_of) = self.lander.workspace()?;
        let made = workspace.act(run_of, CREATE_AREA, json!({"name": name, "kind": kind}))?;
        let id = created_id(&made).ok_or_else(|| {
            anyhow!("Tasks made the {kind} {name} but did not say its id: {made}")
        })?;
        self.kept.areas.insert(
            key,
            KeptArea {
                id: id.clone(),
                run,
            },
        );
        self.lander.count("projects", 1, 0)?;
        Ok(id)
    }

    /// Link `from` (an issue's page) to `to`, as `kind` says it relates, once both are here.
    fn link(&mut self, from: &str, to: &str, kind: &str) -> Result<()> {
        let pair = format!("{from}\u{0}{to}");
        if self.kept.links.contains(&pair) {
            return Ok(());
        }
        let (Some(issue), Some(related)) = (
            self.kept.issues.get(from).cloned(),
            self.kept.issues.get(to).cloned(),
        ) else {
            return Ok(());
        };
        let label = format!("{} {}", relation_words(kind), reference_of(to));
        let (workspace, run) = self.lander.workspace()?;
        workspace.act(
            run,
            LINK,
            json!({
                "issue": issue,
                "app": TASKS_APP,
                "kind": ISSUE_KIND,
                "resource": format!("tasks.tracker/issues/{related}"),
                "label": label.trim(),
                "record": null,
            }),
        )?;
        self.kept.links.insert(pair);
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
            let origin = origin_of(
                page.author.as_ref(),
                "Outline",
                &page.url,
                None,
                &page.created_at,
                &page.updated_at,
            );
            match workspace.act(
                run,
                IMPORT_MARKDOWN,
                json!({"title": page.title, "markdown": markdown, "images": images, "note": note,
                       "origin": origin}),
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
            .note(&format!("{NOTED}{}", page.key))
            .and_then(|()| self.lander.count("pages", 1, 0))
            .map_err(|error| self.fail(error))
    }

    fn issue(&mut self, issue: &IncomingIssue) -> std::io::Result<()> {
        self.stop_check()?;
        let result = self.land_issue(issue);
        result.map_err(|error| self.fail(error))?;
        self.lander
            .note(&format!("{NOTED}{}", issue.key))
            .and_then(|()| self.lander.count("issues", 1, 0))
            .map_err(|error| self.fail(error))
    }
}

impl Landing<'_, '_> {
    /// Each comment of `issue` on the Tasks issue `id`, by its author there and on its date.
    fn comment_all(
        workspace: &mut dyn super::workspace::Workspace,
        run: &super::workspace::Run,
        id: &str,
        issue: &IncomingIssue,
    ) -> Result<()> {
        for comment in &issue.comments {
            let author = (!comment.author.trim().is_empty()).then(|| IncomingAuthor {
                name: comment.author.clone(),
                handle: comment.handle.clone(),
            });
            let url = if comment.url.is_empty() {
                issue.url.as_str()
            } else {
                comment.url.as_str()
            };
            let origin = origin_of(
                author.as_ref(),
                service_name(&issue.source),
                url,
                None,
                &comment.at,
                &comment.updated_at,
            );
            workspace.act(
                run,
                COMMENT,
                json!({"issue": id, "text": comment.text, "origin": origin}),
            )?;
        }
        Ok(())
    }

    fn land_issue(&mut self, issue: &IncomingIssue) -> Result<()> {
        // A project in Linear stays a project; an issue in none works towards its team's area.
        let service = service_name(&issue.source);
        let project = match (&issue.project, &issue.team) {
            (Some(project), _) => Some(self.area(project, "project", service)?),
            (None, Some(team)) => Some(self.area(team, "area", service)?),
            (None, None) => None,
        };
        if let Some(id) = self.kept.issues.get(&issue.url).cloned() {
            // An earlier run brought it before it had a project: it joins its project now.
            if let Some(project) = &project {
                let (workspace, run) = self.lander.workspace()?;
                workspace.act(run, SET_PROJECT, json!({"issue": id, "project": project}))?;
            }
            return Ok(());
        }
        let origin = origin_of(
            issue.author.as_ref(),
            service,
            &issue.url,
            Some(&issue.identifier),
            &issue.created_at,
            &issue.updated_at,
        );
        let due_ms = issue
            .due
            .as_deref()
            .and_then(|due| chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d").ok())
            .and_then(|day| day.and_hms_opt(12, 0, 0))
            .map(|noon| noon.and_utc().timestamp_millis());
        let title = if issue.title.trim().is_empty() {
            issue.identifier.as_str()
        } else {
            issue.title.as_str()
        };
        let (workspace, run) = self.lander.workspace()?;
        let created = workspace.act(
            run,
            CREATE_ISSUE,
            json!({
                "title": title.trim(),
                "body": issue.body,
                "status": issue.status,
                "assignee": issue.assignee,
                "url": issue.url,
                "parent": null,
                "project": project,
                "priority": issue.priority,
                "estimate": issue.estimate,
                "due_ms": due_ms,
                "labels": issue.labels,
                "origin": origin,
            }),
        )?;
        let id = created_id(&created).ok_or_else(|| {
            anyhow!(
                "Tasks made {} but did not say its id: {created}",
                issue.identifier
            )
        })?;
        Self::comment_all(workspace, run, &id, issue)?;
        self.kept.issues.insert(issue.url.clone(), id.clone());
        self.kept
            .issue_runs
            .insert(issue.url.clone(), self.lander.run.id.clone());
        if let Some(parent) = issue
            .parent
            .as_ref()
            .and_then(|parent| self.kept.issues.get(parent))
            .cloned()
        {
            let (workspace, run) = self.lander.workspace()?;
            workspace.act(run, MOVE_UNDER, json!({"issue": id, "parent": parent}))?;
        }
        // How it relates to the issues here, and theirs to it that waited for it.
        for relation in &issue.relations {
            if self.kept.issues.contains_key(&relation.page) {
                self.link(&issue.url, &relation.page, &relation.kind)?;
            } else {
                self.kept
                    .waiting
                    .entry(relation.page.clone())
                    .or_default()
                    .push((issue.url.clone(), relation.kind.clone()));
            }
        }
        for (from, kind) in self.kept.waiting.remove(&issue.url).unwrap_or_default() {
            self.link(&from, &issue.url, &kind)?;
        }
        Ok(())
    }
}

/// How the service an issue came from names itself to people.
fn service_name(source: &str) -> &'static str {
    match source {
        FORGEJO => "Forgejo",
        _ => "Linear",
    }
}

/// How a relation reads before the other issue's name.
fn relation_words(kind: &str) -> &'static str {
    match kind {
        "blocks" => "Blocks",
        "blocked-by" => "Blocked by",
        "duplicate-of" => "Duplicate of",
        "duplicated-by" => "Duplicated by",
        _ => "Related to",
    }
}

/// The name an issue's page gives it: `ENG-12` of Linear's `…/issue/ENG-12/slug`, `app#12` of
/// Forgejo's `…/acme/app/issues/12`.
fn reference_of(page: &str) -> String {
    let mut parts = page.trim_end_matches('/').rsplit('/');
    let last = parts.next().unwrap_or_default();
    if !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) {
        let mut before = page.trim_end_matches('/').rsplit('/').skip(1);
        if let (Some("issues"), Some(repository)) = (before.next(), before.next()) {
            return format!("{repository}#{last}");
        }
    }
    let named = |part: &str| {
        part.split_once('-').is_some_and(|(team, number)| {
            !team.is_empty()
                && team.chars().all(|c| c.is_ascii_alphanumeric())
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
        })
    };
    if named(last) {
        return last.to_owned();
    }
    parts
        .next()
        .filter(|part| named(part))
        .map_or_else(|| last.to_owned(), str::to_owned)
}

/// Where an item was first written, as an action's `origin` takes it: who wrote it on `source`,
/// its page there, the name the source gave it and when it was written and changed there.
/// `None` when the source does not say who wrote it: the item is then the mover's, as written.
fn origin_of(
    author: Option<&IncomingAuthor>,
    source: &str,
    url: &str,
    reference: Option<&str>,
    created: &str,
    updated: &str,
) -> Option<Value> {
    let author = author.filter(|author| !author.name.trim().is_empty())?;
    let created_ms = millis(created);
    // A last change the source dates before the item was written is not kept.
    let updated_ms =
        millis(updated).filter(|updated| created_ms.is_none_or(|made| *updated >= made));
    Some(json!({
        "author": author.name.trim(),
        "handle": author.handle,
        "source": source,
        "url": (!url.is_empty()).then_some(url),
        "reference": reference.filter(|reference| !reference.is_empty()),
        "created_ms": created_ms,
        "updated_ms": updated_ms,
    }))
}

/// An RFC 3339 time as milliseconds since the epoch.
fn millis(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at.trim())
        .ok()
        .map(|at| at.timestamp_millis())
}

/// The id Tasks answered for the issue it made: `-> tasks.tracker/issues/<id>`.
fn created_id(answer: &str) -> Option<String> {
    if let Some((_, after)) = answer
        .split_once("/issues/")
        .or_else(|| answer.split_once("/areas/"))
    {
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
    fn an_issue_keeps_who_opened_it_where_and_when() {
        let author = IncomingAuthor {
            name: "Grace Hopper".into(),
            handle: Some("grace@example.com".into()),
        };
        let origin = origin_of(
            Some(&author),
            "Linear",
            "https://linear.example/acme/issue/ENG-1/set-up-ci",
            Some("ENG-1"),
            "2025-09-01T10:00:00.000Z",
            "2025-08-01T10:00:00.000Z",
        )
        .unwrap();
        assert_eq!(origin["author"], "Grace Hopper");
        assert_eq!(origin["handle"], "grace@example.com");
        assert_eq!(origin["reference"], "ENG-1");
        assert_eq!(origin["created_ms"], 1_756_720_800_000_i64);
        assert!(
            origin["updated_ms"].is_null(),
            "a change dated before it was written is not kept"
        );
        assert!(origin_of(None, "Linear", "", None, "", "").is_none());
        assert_eq!(
            reference_of("https://linear.example/acme/issue/ENG-12/a-title"),
            "ENG-12"
        );
        assert_eq!(
            reference_of("https://linear.example/acme/issue/ENG-12"),
            "ENG-12"
        );
        assert_eq!(
            reference_of("https://forge.example/acme/app/issues/12"),
            "app#12"
        );
        assert_eq!(service_name(FORGEJO), "Forgejo");
    }

    #[test]
    fn a_page_is_kept_under_the_same_note_every_run() {
        assert_eq!(note_for("outline", "doc-1"), note_for("outline", "doc-1"));
        assert_ne!(note_for("outline", "doc-1"), note_for("outline", "doc-2"));
        assert!(uuid::Uuid::parse_str(&note_for("outline", "doc-1")).is_ok());
    }
}
