//! Outline, Linear and Forgejo as migration sources, through the installer's own service sources
//! ([`crate::service`]): the same connection check, review, journal and resume.
//!
//! Their keys are typed by the person and kept in this computer's account vault; the review,
//! the journal and every request to the service happen here. What comes over lands as the
//! agent's invocation: an Outline document becomes a Notes page (`import-markdown`, under the id its
//! source decides, then `move-note` under its parent), an issue from Linear or Forgejo one in
//! Tasks (made by `create-many` in the project or area its project or team, milestone or
//! repository there became, then `comment`, `move-under` and `link` for the issues it relates
//! to). Items go over 200 at a time, and the actions after the first as groups of many, so a
//! tracker of thousands of issues is a few dozen calls. Each keeps its origin: who wrote it on
//! the service, when, its page there and the name the service gave it (`ENG-12`), so a person
//! sees the original author and date while History says who moved it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::service::{
    self, Delivered, IncomingAuthor, IncomingGroup, IncomingIssue, IncomingPage, RefusedItem,
    ServiceAddress, ServiceJournal, ServiceKind, ServiceReview,
};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use accounts::person::AccountList;

use super::Lander;
use super::state::{Item, Place, Review};

pub const OUTLINE: &str = "outline";
pub const LINEAR: &str = "linear";
pub const FORGEJO: &str = "forgejo";
/// A mail account on its server (IMAP), which joins the person's accounts with its password;
/// Mail then fetches its mail itself.
pub const MAIL: &str = "mail";

/// The tilolang Notes notebook: a page is imported as a Markdown file, its place in the tree
/// said by the file's path.
const NOTES: &str = "notes";
const IMPORT_PAGE: &str = "import_page";
/// Tasks' issues, where an issue is made, commented on, nested and linked.
const ISSUES: &str = "tasks/tasks.issues";
const CREATE_MANY: &str = "tasks/tasks.issues#create-many";
const CREATE_AREA: &str = "tasks/tasks.tracker#create-area";
const COMMENT: &str = "comment";
const MOVE_UNDER: &str = "move-under";
const LINK: &str = "link";
/// How many items an import hands over at once: what one `create-many` call makes.
const LANDING_BATCH: usize = 200;
/// The most issues one `create-many` call makes (Tasks' own bound).
const ISSUES_PER_CALL: usize = 200;
/// The most bytes of issues one `create-many` call carries: well within the text Tasks keeps in
/// one part of its tracker.
const ISSUES_BYTES: usize = 384 * 1024;
/// The most members and bytes of arguments one grouped call carries: a group is one step of
/// history, and one message of the workspace's session.
const GROUP_MEMBERS: usize = 500;
const GROUP_BYTES: usize = 256 * 1024;
/// The most characters an issue's title has in Tasks (its own bound): a longer one is cut to it,
/// and said whole at the top of the issue's description.
const TITLE_CHARS: usize = 240;
/// How Tasks names the kind of an issue a link points at.
const ISSUE_KIND: &str = "tasks.tracker.issues";
use tilo_app_id::apps::TASKS as TASKS_APP;

/// A service reached with a key or a password: Outline, Linear, Forgejo, a mail account.
pub struct Services;

impl super::Source for Services {
    fn here(&self, cx: &super::Cx<'_>, row: &'static super::registry::Row) -> super::Here {
        let connected = sources(cx);
        let service = connected
            .iter()
            .find(|service| service.get("source").and_then(Value::as_str) == Some(row.key));
        match service
            .and_then(|service| service.get("next"))
            .and_then(Value::as_str)
        {
            Some(next) if next.contains("review") => super::Here {
                here: service.and_then(|service| service.get("title")).cloned(),
                next: Some(next.to_owned()),
                unavailable: None,
            },
            _ => super::Here {
                next: row.connect_command(),
                ..super::Here::default()
            },
        }
    }

    fn connect(
        &self,
        cx: &super::Cx<'_>,
        row: &'static super::registry::Row,
        how: super::Connecting<'_>,
    ) -> Result<Value> {
        connect(cx, row.key, how)
    }

    fn review(
        &self,
        cx: &super::Cx<'_>,
        row: &'static super::registry::Row,
        _: Option<&str>,
    ) -> Result<Review> {
        review(cx, row.key)
    }

    fn chose(&self, cx: &super::Cx<'_>, review: &Review) -> Result<()> {
        choose(cx.place, review)
    }

    fn summary(&self, _: &'static super::registry::Row, review: &Review) -> String {
        summary(review)
    }

    fn land(
        &self,
        _: &super::Cx<'_>,
        _: &'static super::registry::Row,
        review: &Review,
        _: Option<&super::files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        land(review, lander)
    }

    fn landed(
        &self,
        cx: &super::Cx<'_>,
        row: &'static super::registry::Row,
        _: &super::state::Journal,
    ) -> Result<usize> {
        landed(cx.place, row.key)
    }

    fn forget(
        &self,
        cx: &super::Cx<'_>,
        row: &'static super::registry::Row,
        run: &str,
        keys: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        forget(cx.place, row.key, run, keys)
    }
}

fn kind(source: &str) -> Result<ServiceKind> {
    ServiceKind::from_key(source)
        .ok_or_else(|| anyhow!("{source:?} is not a service `tilo migrate` connects"))
}

fn config_root() -> Result<std::path::PathBuf> {
    service::config_root().ok_or_else(|| anyhow!("this account has no Tilo config directory"))
}

/// The workspace's accounts, as this computer's held session reads them.
fn record(cx: &super::Cx<'_>) -> Result<AccountList> {
    super::writer::accounts(&mut *cx.open()?)
}

/// The account a connected `source` is reached with among the workspace's `record`: for mail,
/// the account connected last.
fn account(place: &Place, source: &str, record: &AccountList) -> Result<String> {
    let kind = kind(source)?;
    let connected = service::connected(record);
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
pub fn connect(cx: &super::Cx<'_>, source: &str, how: super::Connecting<'_>) -> Result<Value> {
    let place = cx.place;
    let super::Connecting {
        url,
        login,
        secret_file,
    } = how;
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
    let secret = cx.secret(&format!("{what} (it is not shown): "), secret_file)?;
    if secret.is_empty() {
        bail!("nothing was typed");
    }
    let mut workspace = cx.open()?;
    let record = super::writer::accounts(&mut *workspace)?;
    let (account, saving) = service::connect(
        &config_root()?,
        &record,
        ServiceAddress {
            kind,
            server,
            login,
            secret,
        },
    )
    .map_err(|error| anyhow!("{source}: {error}"))?;
    super::writer::change_accounts(&mut *workspace, (&saving.address(), &saving.arguments))?;
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
pub fn sources(cx: &super::Cx<'_>) -> Vec<Value> {
    // Without the workspace's accounts nothing reads as connected: each source says how to.
    let record = record(cx).unwrap_or_default();
    let mut found: Vec<Value> = service::connected(&record)
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

/// Forget what the import `invocation` brought (`keys`, as [`Lander::note`] kept them), after the
/// workspace took it back: a later import brings it again.
///
/// # Errors
/// The journals could not be read or kept.
pub fn forget(
    place: &Place,
    source: &str,
    invocation: &str,
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
        kept.forget_invocation(invocation);
        place.keep(&kept_path, &kept)?;
    }
    Ok(())
}

/// How an import notes an item of a service source's own journal in its own.
const NOTED: &str = "service:";

/// Review the connected `source` now, keeping what was chosen and what already came over.
pub fn review(cx: &super::Cx<'_>, source: &str) -> Result<Review> {
    let place = cx.place;
    let record = record(cx)?;
    let account = account(place, source, &record)?;
    let mut fresh = service::review(&config_root()?, &record, &account)
        .map_err(|error| anyhow!("{source}: {error}"))?;
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
        unreadable: Vec::new(),
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
                note: None,
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

/// What an import brings, for its History entry.
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
    let root = config_root()?;
    let record = super::writer::accounts(lander.workspace)?;
    // A mail account lands as `land_mail` lands it: nothing else publishes one.
    let mut publish = |account: &str| Err(format!("{account} is no source to publish"));
    land_with(&review.source, lander, |journal, landing, save, cancel| {
        service::run(&root, &record, journal, landing, &mut publish, save, cancel)
    })
}

/// Bring `source`'s chosen items over as `hand_over` hands them to the workspace, keeping what each
/// call made by its source's page beside the journal.
pub(super) fn land_with(
    source: &str,
    lander: &mut Lander<'_>,
    hand_over: impl FnOnce(
        &mut ServiceJournal,
        &mut dyn service::Landing,
        &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
        &AtomicBool,
    ) -> Result<service::ServiceReport, service::ServiceError>,
) -> Result<()> {
    let place = lander.place.clone();
    let mut journal =
        service_journal(&place, source)?.ok_or_else(|| super::not_reviewed(source))?;
    journal.recovered();
    let kept_path = place.path(source, "ids.json");
    let kept: Kept = place.read(&kept_path)?.unwrap_or_default();
    let cancel = AtomicBool::new(false);
    lander.open(if source == OUTLINE { "notes" } else { "tasks" });
    let mut landing = Landing {
        lander,
        kept,
        kept_path: kept_path.clone(),
        failure: None,
    };
    let mut save = |journal: &ServiceJournal| -> Result<(), String> {
        place
            .keep(&journal_path(&place, source), journal)
            .map_err(|error| error.to_string())
    };
    let result = hand_over(&mut journal, &mut landing, &mut save, &cancel);
    place.keep(&kept_path, &landing.kept)?;
    if let Some(error) = landing.failure.take() {
        return Err(error);
    }
    let report = result.map_err(|error| anyhow!("{source}: {error}"))?;
    landing.lander.count("", 0, report.already_there)?;
    refused(&report)
}

/// The import's end as the person reads it: what the workspace refused did not come over, each with
/// its reason, and the next import tries it again.
fn refused(report: &service::ServiceReport) -> Result<()> {
    if report.refused.is_empty() {
        return Ok(());
    }
    let each = report
        .refused
        .iter()
        .map(|item| format!("{}: {}", item.name, item.why))
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "{} brought over, {} refused by the workspace (`tilo migrate start` tries them again):\n{each}",
        report.landed,
        report.refused.len()
    )
}

/// The chosen mail account into the person's accounts: its settings and its password, which
/// `connect` kept in this computer's account vault, go to the server that keeps the person's
/// workspace, which signs in with it and keeps both. Mail syncs its mail from its server from
/// then on.
fn land_mail(review: &Review, lander: &mut Lander<'_>) -> Result<()> {
    if !review.chosen().iter().any(|item| item.key == review.handle) {
        return Ok(());
    }
    let record = super::writer::accounts(lander.workspace)?;
    let (config, password) = service::mail_account(&config_root()?, &record, &review.handle)
        .map_err(|error| anyhow!("mail: {error}"))?;
    let mut adding = json!({
        "address": config.email,
        "displayName": config.label(),
        "incomingHost": config.imap_host,
        "incomingPort": config.imap_port,
        "outgoingHost": config.smtp_host,
        "outgoingPort": config.smtp_port,
    });
    if let Some(fields) = adding.as_object_mut() {
        if !config.username.is_empty() && !config.username.eq_ignore_ascii_case(&config.email) {
            fields.insert("username".into(), json!(config.username));
        }
        fields.insert("password".into(), json!(password.as_str()));
    }
    super::thunderbird::connect_mail(adding, lander)
}

/// What a source's imports made in the workspace, kept beside its journal so a later import finds it:
/// the Tasks issue each issue became and the project or area each team and project became, by
/// their pages and keys, the links made between issues, and the links waiting for the issue
/// they point at.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Kept {
    /// The Tasks issue each issue became, by its page on the service.
    #[serde(default)]
    pub issues: BTreeMap<String, String>,
    /// The import that brought each issue, by its page.
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
    /// Sub-issues made before their parent came over, by the parent's page: their pages.
    #[serde(default)]
    pub parents: BTreeMap<String, Vec<String>>,
    /// The comments brought, as the issue's page and the comment's place in its thread
    /// ([`comment_key`]).
    #[serde(default)]
    pub comments: std::collections::BTreeSet<String>,
}

/// A project or area an import made for a team, project, repository or milestone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeptArea {
    pub id: String,
    pub run: String,
}

impl Kept {
    /// Forget what the import `invocation` made, after it was taken back.
    pub fn forget_invocation(&mut self, invocation: &str) {
        let pages: Vec<String> = self
            .issue_runs
            .iter()
            .filter(|(_, by)| *by == invocation)
            .map(|(page, _)| page.clone())
            .collect();
        for page in &pages {
            self.issues.remove(page);
            self.issue_runs.remove(page);
            self.waiting.remove(page);
            self.parents.remove(page);
        }
        self.comments.retain(|comment| {
            !comment
                .split_once('\u{0}')
                .is_some_and(|(page, _)| pages.iter().any(|gone| gone == page))
        });
        for children in self.parents.values_mut() {
            children.retain(|child| !pages.contains(child));
        }
        self.links.retain(|link| {
            !link
                .split('\u{0}')
                .any(|page| pages.iter().any(|gone| gone == page))
        });
        for waiting in self.waiting.values_mut() {
            waiting.retain(|(from, _)| !pages.contains(from));
        }
        self.areas.retain(|_, area| area.run != invocation);
    }
}

/// The workspace's apps, as a service source's run lands in them.
struct Landing<'a, 'b> {
    lander: &'a mut Lander<'b>,
    kept: Kept,
    /// Where `kept` is saved, after every call that made something.
    kept_path: std::path::PathBuf,
    /// Why the import stopped, when the workspace did (rather than the service).
    failure: Option<anyhow::Error>,
}

/// One action of a grouped call: its verb on the resource the group acts on, and its arguments.
struct Member {
    verb: &'static str,
    args: Value,
}

/// How `members` (each `size` bytes) are cut into calls of at most `count` members and `bytes`
/// bytes, in order; a member larger than `bytes` is a call of its own.
fn calls<T>(members: &[T], count: usize, bytes: usize, size: impl Fn(&T) -> usize) -> Vec<&[T]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut held = 0_usize;
    for (at, member) in members.iter().enumerate() {
        let weight = size(member);
        let full = at > start
            && (at.saturating_sub(start) >= count || held.saturating_add(weight) > bytes);
        if full {
            out.extend(members.get(start..at));
            start = at;
            held = 0;
        }
        held = held.saturating_add(weight);
    }
    if start < members.len() {
        out.extend(members.get(start..));
    }
    out
}

/// Apply `members` on `resource` (`tasks/tasks.issues`) as few grouped calls as a group's limits
/// allow, in order. A group lands whole or not at all: one the workspace refuses is split in
/// halves until the member it refused is alone, so every other member still lands. Answers per
/// member: `None` when it landed, or the workspace's refusal of it.
///
/// # Errors
/// The workspace could not be reached: what was answered before is landed.
fn grouped(
    workspace: &mut dyn super::writer::Workspace,
    invocation: &super::writer::Invocation,
    resource: &str,
    members: &[Member],
) -> Result<Vec<Option<anyhow::Error>>> {
    let mut outcomes = Vec::with_capacity(members.len());
    for call in calls(members, GROUP_MEMBERS, GROUP_BYTES, |member| {
        member.args.to_string().len()
    }) {
        settle(workspace, invocation, resource, call, &mut outcomes)?;
    }
    Ok(outcomes)
}

fn settle(
    workspace: &mut dyn super::writer::Workspace,
    invocation: &super::writer::Invocation,
    resource: &str,
    members: &[Member],
    outcomes: &mut Vec<Option<anyhow::Error>>,
) -> Result<()> {
    let answered = match members {
        [] => return Ok(()),
        [one] => workspace.act(
            invocation,
            &format!("{resource}#{}", one.verb),
            one.args.clone(),
        ),
        many => {
            let actions: Vec<Value> = many
                .iter()
                .map(|member| json!({"action": member.verb, "args": member.args}))
                .collect();
            workspace.act(
                invocation,
                &format!("{resource}#group"),
                json!({"actions": actions}),
            )
        }
    };
    match answered {
        Ok(_) => {
            outcomes.extend(members.iter().map(|_| None));
            Ok(())
        }
        Err(error) if !refused_for_good(&error) => Err(error),
        Err(error) if members.len() == 1 => {
            outcomes.push(Some(error));
            Ok(())
        }
        Err(_) => {
            let (first, rest) = members.split_at(members.len() / 2);
            settle(workspace, invocation, resource, first, outcomes)?;
            settle(workspace, invocation, resource, rest, outcomes)
        }
    }
}

/// The ids a call that made items answered, in the order it made them.
fn made_ids(answer: &super::writer::Answer) -> Vec<String> {
    answer
        .result()
        .and_then(|value| value.get("made"))
        .and_then(Value::as_array)
        .map(|made| {
            made.iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the workspace refused what it was asked, rather than not answering it: one that was
/// still not ready after the call asked again stops the import, which the next one resumes, and
/// is never taken as a refusal of the items asked (a list halved down to single issues, each
/// made without its parent).
fn refused_for_good(error: &anyhow::Error) -> bool {
    super::writer::Refused::of(error).is_some_and(|refused| !refused.asks_again())
}

fn refusal(key: &str, name: &str, error: &anyhow::Error) -> RefusedItem {
    RefusedItem {
        key: key.to_owned(),
        name: name.to_owned(),
        why: format!("{error:#}"),
    }
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

    fn keep(&self) -> Result<()> {
        self.lander.place.keep(&self.kept_path, &self.kept)
    }

    /// Note what landed in the import's own journal, so taking the import back forgets it, and count it.
    fn noted(&mut self, delivered: &Delivered, kind: &str) -> std::io::Result<()> {
        let keys: Vec<String> = delivered
            .landed
            .iter()
            .map(|key| format!("{NOTED}{key}"))
            .collect();
        let count = keys.len();
        self.lander
            .note(keys)
            .and_then(|()| self.lander.count(kind, count, 0))
            .map_err(|error| self.fail(error))
    }

    /// The project or area `group` became, made now if no import made it yet: a project in Linear
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
        let made_by = self.lander.invocation.id.clone();
        let (workspace, invocation) = self.lander.workspace()?;
        let made = workspace.act(invocation, CREATE_AREA, json!({"name": name, "kind": kind}))?;
        let id = created_id(&made).ok_or_else(|| {
            anyhow!("Tasks made the {kind} {name} but did not say its id: {made:?}")
        })?;
        self.kept.areas.insert(
            key,
            KeptArea {
                id: id.clone(),
                run: made_by,
            },
        );
        self.keep()?;
        self.lander.count("projects", 1, 0)?;
        Ok(id)
    }

    /// The project or area an issue works towards: its project in Linear, else its team's area.
    fn project_of(&mut self, issue: &IncomingIssue) -> Result<Option<String>> {
        let service = service_name(&issue.source);
        match (&issue.project, &issue.team) {
            (Some(project), _) => self.area(project, "project", service).map(Some),
            (None, Some(team)) => self.area(team, "area", service).map(Some),
            (None, None) => Ok(None),
        }
    }

    /// Make those of `issues` no import made yet, `create-many` at a time, keeping each one's id by
    /// its page the moment the call answers. A sub-issue is made under its parent: in the same
    /// call when the parent comes before it there, by the parent's id when an earlier call made
    /// it, and otherwise moved under it once it comes.
    fn make_all(
        &mut self,
        issues: &[IncomingIssue],
        projects: &BTreeMap<String, String>,
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        let missing: Vec<(&IncomingIssue, Value)> = issues
            .iter()
            .filter(|issue| !self.kept.issues.contains_key(&issue.url))
            .map(|issue| (issue, listed(issue, projects.get(&issue.url))))
            .collect();
        for call in calls(&missing, ISSUES_PER_CALL, ISSUES_BYTES, |(_, listed)| {
            listed.to_string().len()
        }) {
            self.make(call, true, refused)?;
        }
        Ok(())
    }

    /// Make `issues` in one call, each under its parent when `nest`; a list Tasks refuses is
    /// halved until the issue it refuses is alone.
    fn make(
        &mut self,
        issues: &[(&IncomingIssue, Value)],
        nest: bool,
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }
        let mut list = Vec::with_capacity(issues.len());
        for (at, (issue, listed)) in issues.iter().enumerate() {
            let mut listed = listed.clone();
            if let Some(parent) = issue.parent.as_ref().filter(|_| nest) {
                let earlier = issues
                    .get(..at)
                    .and_then(|before| before.iter().position(|(other, _)| other.url == *parent));
                let nested = match earlier {
                    Some(place) => Some(("under", json!(place.saturating_add(1).to_string()))),
                    None => self.kept.issues.get(parent).map(|id| ("parent", json!(id))),
                };
                if let (Some((field, value)), Some(fields)) = (nested, listed.as_object_mut()) {
                    fields.insert(field.to_owned(), value);
                }
            }
            list.push(listed);
        }
        let (workspace, invocation) = self.lander.workspace()?;
        match workspace.act(invocation, CREATE_MANY, json!({"issues": list})) {
            Ok(answer) => {
                let ids = made_ids(&answer);
                if ids.len() != issues.len() {
                    bail!(
                        "Tasks made {} issues but answered {} ids: {answer:?}",
                        issues.len(),
                        ids.len()
                    );
                }
                let invocation = self.lander.invocation.id.clone();
                for ((issue, _), id) in issues.iter().zip(ids) {
                    self.kept.issues.insert(issue.url.clone(), id);
                    self.kept
                        .issue_runs
                        .insert(issue.url.clone(), invocation.clone());
                }
                // What a call made is kept before anything else is asked: an import stopped after
                // it finds every issue it made, and makes none twice.
                self.keep()
            }
            Err(error) if !refused_for_good(&error) => Err(error),
            Err(error) if issues.len() == 1 => {
                let Some((issue, listed)) = issues.first() else {
                    return Ok(());
                };
                // The issue an earlier import made its parent may be gone from Tasks (deleted
                // since): the issue comes over all the same, at the top level.
                if nest
                    && issue
                        .parent
                        .as_ref()
                        .is_some_and(|parent| self.kept.issues.contains_key(parent))
                {
                    return self.make(&[(*issue, listed.clone())], false, refused);
                }
                refused.push(refusal(&issue.key, &issue.identifier, &error));
                Ok(())
            }
            Err(_) => {
                let (first, rest) = issues.split_at(issues.len() / 2);
                self.make(first, nest, refused)?;
                self.make(rest, nest, refused)
            }
        }
    }

    /// Land `issues`: the issues first, then their comments, the sub-issues waiting for them and
    /// how they relate, each as grouped calls.
    fn land_issues(&mut self, issues: &[IncomingIssue]) -> Result<Delivered> {
        let mut refused = Vec::new();
        let mut projects = BTreeMap::new();
        for issue in issues {
            if let Some(project) = self.project_of(issue)? {
                projects.insert(issue.url.clone(), project);
            }
        }
        self.make_all(issues, &projects, &mut refused)?;
        let made: Vec<&IncomingIssue> = issues
            .iter()
            .filter(|issue| self.kept.issues.contains_key(&issue.url))
            .collect();

        // A sub-issue whose parent was not here when it was made waits for it.
        for issue in &made {
            if let Some(parent) = &issue.parent
                && !self.kept.issues.contains_key(parent)
            {
                let children = self.kept.parents.entry(parent.clone()).or_default();
                if !children.contains(&issue.url) {
                    children.push(issue.url.clone());
                }
            }
        }
        self.comment_all(&made, &mut refused)?;
        self.move_waiting(&made, &mut refused)?;
        self.link_all(&made, &mut refused)?;
        self.keep()?;

        let failed: std::collections::BTreeSet<&str> =
            refused.iter().map(|item| item.key.as_str()).collect();
        let landed = made
            .iter()
            .filter(|issue| !failed.contains(issue.key.as_str()))
            .map(|issue| issue.key.clone())
            .collect();
        Ok(Delivered { landed, refused })
    }

    /// The comments of `issues` no import brought yet, by their authors there and on their dates.
    fn comment_all(
        &mut self,
        issues: &[&IncomingIssue],
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        let mut members = Vec::new();
        let mut of = Vec::new();
        for issue in issues {
            let Some(id) = self.kept.issues.get(&issue.url) else {
                continue;
            };
            for (at, comment) in issue.comments.iter().enumerate() {
                let key = comment_key(&issue.url, at);
                if comment.text.trim().is_empty() || self.kept.comments.contains(&key) {
                    continue;
                }
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
                members.push(Member {
                    verb: COMMENT,
                    args: json!({"issue": id, "text": comment.text, "origin": origin}),
                });
                of.push((*issue, key));
            }
        }
        let (workspace, invocation) = self.lander.workspace()?;
        let outcomes = grouped(workspace, invocation, ISSUES, &members)?;
        for ((issue, key), outcome) in of.into_iter().zip(outcomes) {
            match outcome {
                None => {
                    self.kept.comments.insert(key);
                }
                Some(error) => refused.push(refusal(
                    &issue.key,
                    &format!("a comment on {}", issue.identifier),
                    &error,
                )),
            }
        }
        Ok(())
    }

    /// Move the sub-issues that waited for one of `issues` under it.
    fn move_waiting(
        &mut self,
        issues: &[&IncomingIssue],
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        let mut members = Vec::new();
        let mut of = Vec::new();
        for issue in issues {
            let Some(parent) = self.kept.issues.get(&issue.url).cloned() else {
                continue;
            };
            for child in self.kept.parents.remove(&issue.url).unwrap_or_default() {
                if let Some(id) = self.kept.issues.get(&child) {
                    members.push(Member {
                        verb: MOVE_UNDER,
                        args: json!({"issue": id, "parent": parent}),
                    });
                    of.push(child);
                }
            }
        }
        let (workspace, invocation) = self.lander.workspace()?;
        let outcomes = grouped(workspace, invocation, ISSUES, &members)?;
        for (child, outcome) in of.into_iter().zip(outcomes) {
            if let Some(error) = outcome {
                refused.push(refusal(&child, &reference_of(&child), &error));
            }
        }
        Ok(())
    }

    /// Link each of `issues` to the issues here it relates to, and those that waited for it.
    fn link_all(
        &mut self,
        issues: &[&IncomingIssue],
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        let mut pairs: Vec<(String, String, String)> = Vec::new();
        for issue in issues {
            for relation in &issue.relations {
                if self.kept.issues.contains_key(&relation.page) {
                    pairs.push((
                        issue.url.clone(),
                        relation.page.clone(),
                        relation.kind.clone(),
                    ));
                } else {
                    let waiting = self.kept.waiting.entry(relation.page.clone()).or_default();
                    let entry = (issue.url.clone(), relation.kind.clone());
                    if !waiting.contains(&entry) {
                        waiting.push(entry);
                    }
                }
            }
            for (from, kind) in self.kept.waiting.remove(&issue.url).unwrap_or_default() {
                pairs.push((from, issue.url.clone(), kind));
            }
        }
        let mut members = Vec::new();
        let mut of = Vec::new();
        let mut asked = std::collections::BTreeSet::new();
        for (from, to, kind) in pairs {
            let pair = format!("{from}\u{0}{to}");
            if self.kept.links.contains(&pair) || !asked.insert(pair.clone()) {
                continue;
            }
            let (Some(issue), Some(related)) =
                (self.kept.issues.get(&from), self.kept.issues.get(&to))
            else {
                continue;
            };
            let label = format!("{} {}", relation_words(&kind), reference_of(&to));
            members.push(Member {
                verb: LINK,
                args: json!({
                    "issue": issue,
                    "app": TASKS_APP,
                    "kind": ISSUE_KIND,
                    "resource": format!("tasks.issues/issues/{related}"),
                    "label": label.trim(),
                }),
            });
            of.push((from, pair));
        }
        let (workspace, invocation) = self.lander.workspace()?;
        let outcomes = grouped(workspace, invocation, ISSUES, &members)?;
        for ((from, pair), outcome) in of.into_iter().zip(outcomes) {
            match outcome {
                None => {
                    self.kept.links.insert(pair);
                }
                Some(error) => {
                    let key = issues
                        .iter()
                        .find(|issue| issue.url == from)
                        .map_or(from.as_str(), |issue| issue.key.as_str());
                    refused.push(refusal(
                        key,
                        &format!("a link of {}", reference_of(&from)),
                        &error,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Land `pages` in Notes: each page as a Markdown file imported by the notebook's
    /// `import_page`, named by the path its place in the tree gives it, so the note stands under
    /// the note of the page above it. A page the notebook already holds opens the note it made,
    /// whatever the page says now.
    ///
    /// A page whose pictures came with it is refused, not imported without them: the notebook's
    /// import carries a page's words and no pictures beside them yet, and a page that lost its
    /// pictures is worse than one left for a later import.
    fn land_pages(&mut self, pages: &[IncomingPage]) -> Result<Delivered> {
        let ordered = pages_in_tree_order(pages);
        let mut members = Vec::with_capacity(pages.len());
        let mut carried = Vec::with_capacity(pages.len());
        {
            let (workspace, _) = self.lander.workspace()?;
            for page in ordered.iter().copied() {
                if !page.pictures.is_empty() {
                    continue;
                }
                let content = workspace.put(page.markdown.as_bytes())?;
                let origin = origin_of(
                    page.author.as_ref(),
                    "Outline",
                    &page.url,
                    None,
                    &page.created_at,
                    &page.updated_at,
                );
                let by = origin
                    .as_ref()
                    .and_then(|origin| origin.get("author"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                members.push(Member {
                    verb: IMPORT_PAGE,
                    args: json!({
                        "file": {
                            "content": content,
                            "name": format!("{}.md", page_path(pages, page)),
                            "media": "text/markdown",
                        },
                        "by": by,
                        "service": "Outline",
                        "page": note_for(&page.source, &page.key),
                        "created": page.created_at,
                        "updated": page.updated_at,
                    }),
                });
                carried.push(page);
            }
        }
        let (workspace, invocation) = self.lander.workspace()?;
        let imported = grouped(workspace, invocation, NOTES, &members)?;
        let mut refused: Vec<RefusedItem> = pages
            .iter()
            .filter(|page| !page.pictures.is_empty())
            .map(|page| RefusedItem {
                key: page.key.clone(),
                name: page.title.clone(),
                why: "its pictures cannot be carried into the notebook yet: its import takes the \
                      page's words alone"
                    .to_owned(),
            })
            .collect();
        let mut landed = Vec::new();
        for (page, outcome) in carried.into_iter().zip(imported) {
            match outcome {
                // A page an earlier import brought is the person's now, as they left it.
                Some(error) if already_imported(&error) => landed.push(page.key.clone()),
                None => landed.push(page.key.clone()),
                Some(error) => refused.push(refusal(&page.key, &page.title, &error)),
            }
        }
        Ok(Delivered { landed, refused })
    }
}

impl service::Landing for Landing<'_, '_> {
    fn batch(&self) -> usize {
        LANDING_BATCH
    }

    fn pages(&mut self, pages: &[IncomingPage]) -> std::io::Result<Delivered> {
        self.stop_check()?;
        let delivered = self.land_pages(pages).map_err(|error| self.fail(error))?;
        self.noted(&delivered, "pages")?;
        Ok(delivered)
    }

    fn issues(&mut self, issues: &[IncomingIssue]) -> std::io::Result<Delivered> {
        self.stop_check()?;
        let delivered = self.land_issues(issues).map_err(|error| self.fail(error))?;
        self.noted(&delivered, "issues")?;
        Ok(delivered)
    }
}

/// How a comment is named among what an import brought: its issue's page and its place there.
fn comment_key(page: &str, at: usize) -> String {
    format!("{page}\u{0}{at}")
}

/// `issue` as `create-many` takes it, working towards `project`.
fn listed(issue: &IncomingIssue, project: Option<&String>) -> Value {
    let service = service_name(&issue.source);
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
        .and_then(|due| jiff::civil::Date::strptime("%Y-%m-%d", due).ok())
        .and_then(|day| day.at(12, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|noon| noon.timestamp().as_millisecond());
    let title = if issue.title.trim().is_empty() {
        issue.identifier.trim()
    } else {
        issue.title.trim()
    };
    let (title, body) = if title.chars().count() > TITLE_CHARS {
        let cut: String = title.chars().take(TITLE_CHARS.saturating_sub(1)).collect();
        let body = if issue.body.trim().is_empty() {
            title.to_owned()
        } else {
            format!("{title}\n\n{}", issue.body)
        };
        (format!("{}…", cut.trim_end()), body)
    } else {
        (title.to_owned(), issue.body.clone())
    };
    json!({
        "title": title,
        "body": body,
        "status": issue.status,
        "assignee": issue.assignee,
        "url": issue.url,
        "project": project,
        "priority": issue.priority,
        "estimate": issue.estimate,
        "due_ms": due_ms,
        "labels": issue.labels,
        "origin": origin,
    })
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
    at.trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

/// Whether `error` is the refusal that says a note from this source is in the notebook already.
fn already_imported(error: &anyhow::Error) -> bool {
    super::writer::Refused::of(error)
        .is_some_and(|refused| refused.code() == tilo_actions::codes::NOTE_ALREADY_IMPORTED)
}

/// The id Tasks answered for the issue it made, as the record its answer names
/// (`tasks.issues/issues/<id>`) or the id in its value.
fn created_id(answer: &super::writer::Answer) -> Option<String> {
    let value = answer.result()?;
    // What the write made, per item: its id and the address it is published
    // under. Nothing reads an id out of what the answer says it touched.
    if let Some(id) = value
        .get("made")
        .and_then(Value::as_array)
        .and_then(|made| made.first())
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
    {
        return Some(id.to_owned());
    }
    for key in ["id", "issue"] {
        if let Some(id) = value.get(key).and_then(Value::as_str) {
            return Some(id.to_owned());
        }
    }
    match value {
        Value::String(id) => Some(id.clone()),
        _ => None,
    }
}

/// Where a source's page is read, as the notebook keeps it: a page imported by the same address
/// again opens the note it made, so the installer and this command bring the same page to the
/// same note.
#[must_use]
pub fn note_for(source: &str, key: &str) -> String {
    let digest = blake3::hash(format!("{source}\u{0}{key}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(digest.as_bytes().get(..16).unwrap_or(&[0; 16]));
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// `pages` with every page after the pages it stands under: the notebook's import finds the note
/// of the page above a page only once that note is there.
fn pages_in_tree_order(pages: &[IncomingPage]) -> Vec<&IncomingPage> {
    let mut ordered: Vec<&IncomingPage> = Vec::with_capacity(pages.len());
    let mut remaining: Vec<&IncomingPage> = pages.iter().collect();
    let mut placed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    while !remaining.is_empty() {
        let before = remaining.len();
        remaining.retain(|page| {
            let ready = page.parent.as_deref().is_none_or(|parent| {
                placed.contains(parent) || !pages.iter().any(|p| p.key == parent)
            });
            if ready {
                placed.insert(page.key.as_str());
                ordered.push(page);
            }
            !ready
        });
        if remaining.len() == before {
            // Pages whose parents form a circle: no order stands their parents first, so they are
            // imported where the notebook can hold them, at the top of the tree.
            ordered.append(&mut remaining);
        }
    }
    ordered
}

/// The path `page` stands at in the notebook, built from the titles of the pages above it: the
/// import names each note under the note of its file's folder. A circle of pages above it, or a
/// title that cannot be a file's name, ends the path there rather than naming no page at all.
fn page_path(pages: &[IncomingPage], page: &IncomingPage) -> String {
    let name = |page: &IncomingPage| {
        let title = page.title.trim();
        let title = if title.is_empty() { "Untitled" } else { title };
        title.replace(['/', '\\'], "-")
    };
    let mut path = vec![name(page)];
    let mut parent = page.parent.as_deref();
    let mut seen: std::collections::HashSet<&str> =
        std::collections::HashSet::from([page.key.as_str()]);
    while let Some(key) = parent {
        let Some(above) = pages.iter().find(|above| above.key == key) else {
            break;
        };
        if !seen.insert(above.key.as_str()) {
            break;
        }
        path.push(name(above));
        parent = above.parent.as_deref();
    }
    path.reverse();
    path.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::workspace::{Answer, Refused};

    #[test]
    fn a_note_an_earlier_run_imported_is_told_by_its_code() {
        let imported: anyhow::Error = Refused::coded(
            tilo_actions::codes::NOTE_ALREADY_IMPORTED,
            "that note is imported already",
        )
        .into();
        assert!(already_imported(&imported));
        let other: anyhow::Error =
            Refused::coded("something_else", "that note is imported already").into();
        assert!(
            !already_imported(&other),
            "the app's words alone decide nothing"
        );
    }

    #[test]
    fn tasks_answers_name_the_issue_they_made() {
        let id = "0f0e0d0c-0b0a-4908-8706-050403020100";
        let answered = |value: Value| Answer::of(Some(value));
        // The typed answer every write gives: per item, its id and the
        // address it is published under.
        assert_eq!(
            created_id(&answered(json!({
                "made": [{"id": id, "address": format!("tasks.issues/issues/{id}")}]
            })))
            .as_deref(),
            Some(id)
        );
        assert_eq!(
            created_id(&answered(json!({"id": id}))).as_deref(),
            Some(id)
        );
        assert_eq!(created_id(&Answer::of(None)), None);
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
