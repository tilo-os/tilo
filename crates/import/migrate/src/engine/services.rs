//! Outline, Linear and Forgejo as migration sources, through the installer's own service sources
//! ([`crate::service`]): the same connection check, review, journal and resume.
//!
//! Their keys are typed by the person and kept in this computer's account vault; the review,
//! the journal and every request to the service happen here. A mail account is connected
//! straight to the workspace instead: its password goes to the server that keeps the person's
//! workspace, which signs in with it before it keeps the account and seals the password in the
//! workspace's vault, and Mail syncs its mail from its server from then on.
//!
//! What comes over lands as the agent's invocation, through the destination app's own typed
//! actions: an Outline document becomes a Notes page (`import_page`, under the id its source
//! decides, then `move-note` under its parent), and an issue from Linear or Forgejo one in Tasks (`new_project`, `rename` and
//! `set_kind` for the project or area its project or team, milestone or repository there
//! became; `add_origin` and `import_issue` for the issue; then `edit-text`, `file`, `move_to`,
//! `set_status`, `label`, `estimate`, `schedule` and a priority verb for what it carries, and a
//! comment in Discussion for each of its thread and for how it relates to another issue). Each
//! keeps its origin: who wrote it on the service, when, its page there and the name the service
//! gave it (`ENG-12`), so a person sees the original author and date while History says who
//! moved it.

use std::collections::BTreeMap;
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
/// The app that keeps the workspace's tasks, as its path starts: its root resource is where a
/// project and an issue are made, and an item's own address (`tl:so.tilo.tasks:<id>`) is where
/// it is changed.
const TASKS_APP: &str = "so.tilo.tasks";
/// The app's own resource, the root of its document, as its actions name it in arguments.
const TASKS: &str = "tl:so.tilo.tasks";
/// The app that keeps the comments a task's thread holds, as its path starts.
const DISCUSSION: &str = "so.tilo.lang.discussion";
/// The app that keeps the workspace's address book, where someone given a task before they
/// joined is kept as a contact.
const CONTACTS: &str = "so.tilo.contacts";
/// What Contacts finds or adds, by name, as a task's contact assignee names them.
const NAMED_OR_NEW: &str = "named_or_new";
/// Tasks' own actions: a project, its kind and its name.
const NEW_PROJECT: &str = "new_project";
const RENAME: &str = "rename";
const SET_KIND: &str = "set_kind";
/// An issue's origin, and the issue itself, made once each.
const ADD_ORIGIN: &str = "add_origin";
const IMPORT_ISSUE: &str = "import_issue";
/// What an issue carries, set after it is made.
const EDIT_TEXT: &str = "edit-text";
const FILE: &str = "file";
const MOVE_TO: &str = "move_to";
const ESTIMATE: &str = "estimate";
const SCHEDULE: &str = "schedule";
const LABEL: &str = "label";
const RELATE: &str = "relate";
const ASSIGN: &str = "assign";
/// What a task's thread takes, in the Discussion app.
const COMMENT: &str = "comment";
/// How many items an import hands over at once.
const LANDING_BATCH: usize = 200;
/// The most members and bytes of arguments one grouped call carries: a group is one step of
/// history, and one message of the workspace's session.
const GROUP_MEMBERS: usize = 500;
const GROUP_BYTES: usize = 256 * 1024;

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

/// The account a connected `source` is reached with among the workspace's `record`.
fn account(source: &str, record: &AccountList) -> Result<String> {
    let kind = kind(source)?;
    service::connected(record)
        .into_iter()
        .find(|(_, connected, _)| *connected == kind)
        .map(|(id, _, _)| id.as_str().to_owned())
        .ok_or_else(|| {
            anyhow!("{source} is not connected: `tilo migrate connect {source}`, which asks the person for its key")
        })
}

/// `tilo migrate connect`: check the key the person types and keep it in this computer's vault,
/// or connect the mail account whose password they type ([`connect_mail`]).
pub fn connect(cx: &super::Cx<'_>, source: &str, how: super::Connecting<'_>) -> Result<Value> {
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
    let address = ServiceAddress {
        kind,
        server,
        login,
        secret,
    };
    let mut workspace = cx.open()?;
    if kind == ServiceKind::Mail {
        return connect_mail(&mut *workspace, &address);
    }
    let record = super::writer::accounts(&mut *workspace)?;
    let (account, saving) = service::connect(&config_root()?, &record, address)
        .map_err(|error| anyhow!("{source}: {error}"))?;
    super::writer::change_accounts(&mut *workspace, (&saving.address(), &saving.arguments))?;
    Ok(json!({
        "source": source,
        "said": format!("{source} is connected (its key is in this computer's vault); next: tilo migrate review {source}"),
        "account": account.as_str(),
    }))
}

/// Connect the mail account `address` names: its settings and the password the person typed go
/// to the server that keeps their workspace, as their own request (`workspace/mail-accounts#add`),
/// which signs in with the password before it keeps the account and seals the password in the
/// workspace's vault. Nothing of it is kept on this computer.
///
/// # Errors
/// The address names no account Mail signs in to with a password, or the server refused, in its
/// words.
pub(super) fn connect_mail(
    workspace: &mut dyn super::writer::Workspace,
    address: &ServiceAddress,
) -> Result<Value> {
    let config = service::mail_settings(address).map_err(|error| anyhow!("{MAIL}: {error}"))?;
    let mut adding = json!({
        "address": config.email,
        "displayName": config.label(),
        "incomingHost": config.imap_host,
        "incomingPort": config.imap_port,
        "outgoingHost": config.smtp_host,
        "outgoingPort": config.smtp_port,
        "password": address.secret.as_str(),
    });
    if !config.username.is_empty()
        && !config.username.eq_ignore_ascii_case(&config.email)
        && let Some(fields) = adding.as_object_mut()
    {
        fields.insert("username".into(), json!(config.username));
    }
    let added = workspace
        .person(super::thunderbird::MAIL_ACCOUNTS, "add", adding)
        .map_err(|error| error.context(format!("{} was not connected", config.email)))?;
    // What the server answers is the account it keeps: anything else is not one.
    if !added
        .get("address")
        .and_then(Value::as_str)
        .is_some_and(|kept| kept.eq_ignore_ascii_case(&config.email))
    {
        bail!(
            "{} was not connected: the server answered no account for it",
            config.email
        );
    }
    Ok(json!({
        "source": MAIL,
        "said": format!("{} is connected: Mail syncs it from its server now", config.email),
        "account": config.email,
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

/// A service's own journal of what it brought.
const SERVICE: &str = "service";
/// The ids a service's items were made under in the workspace.
const IDS: &str = "ids";

fn keep_service(place: &Place, source: &str, journal: &ServiceJournal) -> Result<()> {
    place.keep_field(source, SERVICE, journal)
}

fn service_journal(place: &Place, source: &str) -> Result<Option<ServiceJournal>> {
    place.field(source, SERVICE)
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
    if let Some(mut kept) = place.field::<Kept>(source, IDS)? {
        kept.forget_invocation(invocation);
        place.keep_field(source, IDS, &kept)?;
    }
    Ok(())
}

/// How an import notes an item of a service source's own journal in its own.
const NOTED: &str = "service:";

/// Review the connected `source` now, keeping what was chosen and what already came over.
pub fn review(cx: &super::Cx<'_>, source: &str) -> Result<Review> {
    if source == MAIL {
        bail!(
            "a mail account comes over as it is connected: `tilo migrate connect mail --login you@example.com`, and Mail syncs it from its server"
        );
    }
    let place = cx.place;
    let record = record(cx)?;
    let account = account(source, &record)?;
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
        FORGEJO => "Forgejo",
        _ => "Linear",
    };
    let titles = chosen
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>()
        .join(", ");
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
    let root = config_root()?;
    let record = super::writer::accounts(lander.workspace)?;
    // A mail account is connected, never landed: nothing publishes one.
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
    let kept: Kept = place.field(source, IDS)?.unwrap_or_default();
    let cancel = AtomicBool::new(false);
    lander.open(if source == OUTLINE { "notes" } else { "tasks" });
    let mut landing = Landing {
        lander,
        kept,
        failure: None,
    };
    let mut save = |journal: &ServiceJournal| -> Result<(), String> {
        keep_service(&place, source, journal).map_err(|error| error.to_string())
    };
    let result = hand_over(&mut journal, &mut landing, &mut save, &cancel);
    place.keep_field(source, IDS, &landing.kept)?;
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

/// What a source's imports made in the workspace, kept beside its journal so a later import finds it:
/// the Tasks task each issue became and the project or area each team and project became, by
/// their pages and keys, the relations linked between tasks, and the relations waiting for the
/// issue they point at.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Kept {
    /// The Tasks task each issue became, by its page on the service, at its address there
    /// (`tl:so.tilo.tasks:<id>`).
    #[serde(default)]
    pub issues: BTreeMap<String, String>,
    /// The import that brought each issue, by its page.
    #[serde(default)]
    pub issue_runs: BTreeMap<String, String>,
    /// The project or area each project and team (or milestone and repository) became, by its
    /// key.
    #[serde(default)]
    pub areas: BTreeMap<String, KeptArea>,
    /// Relations linked between tasks, as `from-page\u{0}to-page`.
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
    /// The issues whose title's body, project, parent, priority, estimate, day and labels are
    /// all set, by their page: an import stopped part-way finishes them, and sets none twice.
    #[serde(default)]
    pub applied: std::collections::BTreeSet<String>,
}

/// A project or area an import made for a team, project, repository or milestone, at its address
/// (`tl:so.tilo.tasks:<id>`).
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
            self.applied.remove(page);
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

/// Apply `members` on `resource` (the notebook's `notes`) as few grouped calls as a group's limits
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

/// Whether the workspace refused what it was asked, rather than not answering it: one that was
/// still not ready after the call asked again stops the import, which the next one resumes, and
/// is never taken as a refusal of the item asked.
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
            return Err(self.fail(super::Stopped.into()));
        }
        Ok(())
    }

    fn keep(&self) -> Result<()> {
        self.lander
            .place
            .keep_field(self.lander.source, IDS, &self.kept)
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

    /// Do `verb` of the resource at `path` — an app, or an app and one of its resources — with
    /// `args`, and answer what the app said.
    fn act(&mut self, path: &str, verb: &str, args: Value) -> Result<super::writer::Answer> {
        let (workspace, invocation) = self.lander.workspace()?;
        workspace.act(invocation, &format!("{path}#{verb}"), args)
    }

    /// The address of the resource `id` of Tasks, as an argument names it.
    fn task_address(id: &str) -> String {
        format!("{TASKS}:{id}")
    }

    /// Where Tasks' resource at `address` stands in the workspace's tree, as a call's path
    /// names it: the app, then the resource by its own address.
    fn task_path(address: &str) -> String {
        format!("{TASKS_APP}/{address}")
    }

    /// The address of the contact `id` of Contacts, as a task's assignee names it.
    fn contact_address(id: &str) -> String {
        format!("tl:{CONTACTS}:{id}")
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
        let made = self.act(TASKS_APP, NEW_PROJECT, json!({}))?;
        let id = created_id(&made).ok_or_else(|| {
            anyhow!("Tasks made the {kind} {name} but did not say its id: {made:?}")
        })?;
        let address = Self::task_address(&id);
        let path = Self::task_path(&address);
        // A new project is called what the app calls one until it is renamed here, and is a
        // project until it is said to be an area.
        self.act(&path, RENAME, json!({"name": name}))?;
        if kind == "area" {
            self.act(&path, SET_KIND, json!({"kind": kind}))?;
        }
        self.kept.areas.insert(
            key,
            KeptArea {
                id: address.clone(),
                run: made_by,
            },
        );
        self.keep()?;
        self.lander.count("projects", 1, 0)?;
        Ok(address)
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

    /// Make those of `issues` no import made yet, keeping each one's address by its page the
    /// moment Tasks answers it. A sub-issue whose parent is already here is moved under it; one
    /// whose parent has not come over yet waits for it.
    fn make_all(&mut self, issues: &[IncomingIssue], refused: &mut Vec<RefusedItem>) -> Result<()> {
        for issue in issues {
            if self.kept.issues.contains_key(&issue.url) {
                continue;
            }
            match self.make_one(issue) {
                Ok(()) => {}
                Err(error) if refused_for_good(&error) => {
                    refused.push(refusal(&issue.key, &issue.identifier, &error));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Make `issue` with its origin, keeping its address by its page as soon as it is made: an
    /// import stopped after that finds it and makes it no second time.
    fn make_one(&mut self, issue: &IncomingIssue) -> Result<()> {
        let service = service_name(&issue.source);
        let made = self.act(TASKS_APP, ADD_ORIGIN, origin_args(issue, service))?;
        let origin = created_id(&made).ok_or_else(|| {
            anyhow!(
                "Tasks made an origin for {} but did not say its id: {made:?}",
                issue.key
            )
        })?;
        let made = self.act(
            TASKS_APP,
            IMPORT_ISSUE,
            json!({
                "origin": Self::task_address(&origin),
                "title": issue_title(issue),
                "key": issue.identifier,
                "status": task_status(&issue.status),
            }),
        )?;
        let id = created_id(&made).ok_or_else(|| {
            anyhow!(
                "Tasks made {} but did not say its id: {made:?}",
                issue.identifier
            )
        })?;
        // The address the issue is changed at, kept before anything else is asked of it.
        let address = Self::task_address(&id);
        self.kept.issues.insert(issue.url.clone(), address);
        self.kept
            .issue_runs
            .insert(issue.url.clone(), self.lander.invocation.id.clone());
        self.keep()
    }

    /// Set on those of `issues` what they carry, unless an earlier import set it already: a body,
    /// the project, the issue it sits under, how urgent it is, its estimate, its day and its
    /// labels. An issue finished here is kept as applied, so an import interrupted after some of
    /// its issues is finished by the next one.
    fn apply_all(
        &mut self,
        issues: &[&IncomingIssue],
        projects: &BTreeMap<String, String>,
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        for issue in issues {
            if self.kept.applied.contains(&issue.url) {
                continue;
            }
            let Some(address) = self.kept.issues.get(&issue.url).cloned() else {
                continue;
            };
            match self.apply_one(issue, &address, projects.get(&issue.url)) {
                Ok(()) => {
                    self.kept.applied.insert(issue.url.clone());
                }
                Err(error) if refused_for_good(&error) => {
                    refused.push(refusal(&issue.key, &reference_of(&issue.url), &error));
                }
                Err(error) => return Err(error),
            }
        }
        self.keep()
    }

    /// Set on the task at `address` what `issue` carries.
    fn apply_one(
        &mut self,
        issue: &IncomingIssue,
        address: &str,
        project: Option<&String>,
    ) -> Result<()> {
        let path = Self::task_path(address);
        if !issue.body.trim().is_empty() {
            self.act(
                &path,
                EDIT_TEXT,
                json!({"field": "notes", "value": issue.body}),
            )?;
        }
        if let Some(project) = project {
            self.act(&path, FILE, json!({"p": project}))?;
        }
        if let Some(parent) = &issue.parent {
            if let Some(parent) = self.kept.issues.get(parent).cloned() {
                self.act(&path, MOVE_TO, json!({"parent": parent, "before": null}))?;
            } else {
                let children = self.kept.parents.entry(parent.clone()).or_default();
                if !children.contains(&issue.url) {
                    children.push(issue.url.clone());
                }
            }
        }
        if let Some(priority) = issue.priority.as_deref().and_then(priority_action) {
            self.act(&path, priority, json!({}))?;
        }
        if let Some(estimate) = issue.estimate {
            self.act(&path, ESTIMATE, json!({"points": estimate}))?;
        }
        if let Some(due) = &issue.due {
            self.act(&path, SCHEDULE, json!({"due": due}))?;
        }
        for label in &issue.labels {
            if !label.trim().is_empty() {
                self.act(&path, LABEL, json!({"name": label}))?;
            }
        }
        // Someone given the issue before they joined the workspace is a contact of its book:
        // found by the name the service had for them, or added, and the task is theirs.
        if let Some(name) = issue
            .assignee
            .as_deref()
            .and_then(|who| who.strip_prefix("outside:"))
        {
            match self.assign_contact(&path, name) {
                // The workspace keeps no address book here: the issue still lands, unassigned.
                Err(error) if refused_for_good(&error) => {}
                other => other?,
            }
        }
        Ok(())
    }

    /// Give the task at `path` to the contact called `name`: the one the address book keeps, or
    /// a card added for them.
    fn assign_contact(&mut self, path: &str, name: &str) -> Result<()> {
        let made = self.act(CONTACTS, NAMED_OR_NEW, json!({"name": name}))?;
        let contact = created_id(&made)
            .ok_or_else(|| anyhow!("Contacts made no card for {name}: {made:?}"))?;
        self.act(
            path,
            ASSIGN,
            json!({"who": null, "contact": Self::contact_address(&contact)}),
        )
        .map(drop)
    }

    /// Land `issues`: the issues first, then their comments, the sub-issues waiting for them and
    /// how they relate.
    fn land_issues(&mut self, issues: &[IncomingIssue]) -> Result<Delivered> {
        let mut refused = Vec::new();
        let mut projects = BTreeMap::new();
        for issue in issues {
            if let Some(project) = self.project_of(issue)? {
                projects.insert(issue.url.clone(), project);
            }
        }
        self.make_all(issues, &mut refused)?;
        let made: Vec<&IncomingIssue> = issues
            .iter()
            .filter(|issue| self.kept.issues.contains_key(&issue.url))
            .collect();
        self.apply_all(&made, &projects, &mut refused)?;

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

    /// The comments of `issues` no import brought yet, said in the Discussion app about the task
    /// each comment's issue became: each is a comment, which is all a thread is.
    fn comment_all(
        &mut self,
        issues: &[&IncomingIssue],
        refused: &mut Vec<RefusedItem>,
    ) -> Result<()> {
        for issue in issues {
            let Some(address) = self.kept.issues.get(&issue.url).cloned() else {
                continue;
            };
            for (at, comment) in issue.comments.iter().enumerate() {
                let key = comment_key(&issue.url, at);
                if comment.text.trim().is_empty() || self.kept.comments.contains(&key) {
                    continue;
                }
                let mut args = serde_json::Map::new();
                args.insert("text".into(), json!(comment.text));
                args.insert(
                    tilo_actions::params::FROM_ARGUMENT.into(),
                    made_from(&address),
                );
                match self.act(DISCUSSION, COMMENT, Value::Object(args)) {
                    Ok(_) => {
                        self.kept.comments.insert(key);
                    }
                    Err(error) if refused_for_good(&error) => refused.push(refusal(
                        &issue.key,
                        &format!("a comment on {}", issue.identifier),
                        &error,
                    )),
                    Err(error) => return Err(error),
                }
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
        for issue in issues {
            let Some(parent) = self.kept.issues.get(&issue.url).cloned() else {
                continue;
            };
            for child in self.kept.parents.remove(&issue.url).unwrap_or_default() {
                let Some(address) = self.kept.issues.get(&child).cloned() else {
                    continue;
                };
                match self.act(
                    &Self::task_path(&address),
                    MOVE_TO,
                    json!({"parent": parent, "before": null}),
                ) {
                    Ok(_) => {}
                    Err(error) if refused_for_good(&error) => {
                        refused.push(refusal(&child, &reference_of(&child), &error));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Link each of `issues` to the issues here it relates to, and those that waited for it, with
    /// how they relate: a typed link between the two tasks, which both their pages draw.
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
        let mut asked = std::collections::BTreeSet::new();
        for (from, to, kind) in pairs {
            let pair = format!("{from}\u{0}{to}");
            if self.kept.links.contains(&pair) || !asked.insert(pair.clone()) {
                continue;
            }
            let Some(address) = self.kept.issues.get(&from).cloned() else {
                continue;
            };
            let Some(other) = self.kept.issues.get(&to).cloned() else {
                continue;
            };
            match self.act(
                &Self::task_path(&address),
                RELATE,
                json!({"other": other, "how": relation_words(&kind)}),
            ) {
                Ok(_) => {
                    self.kept.links.insert(pair);
                }
                Err(error) if refused_for_good(&error) => {
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
                Err(error) => return Err(error),
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

/// What Tasks calls the issue: its title, or the name the service gave it when it has none.
fn issue_title(issue: &IncomingIssue) -> String {
    let title = issue.title.trim();
    if title.is_empty() {
        issue.identifier.trim().to_owned()
    } else {
        title.to_owned()
    }
}

/// The service an issue came from, as Tasks' `add_origin` takes it: who wrote it there, its page,
/// the name the service gave it, and when it was written and changed there. An issue the service
/// does not say who wrote is the service's own, so its origin is still named.
fn origin_args(issue: &IncomingIssue, service: &str) -> Value {
    let author = issue
        .author
        .as_ref()
        .map(|author| author.name.trim())
        .filter(|name| !name.is_empty());
    json!({
        "author": author.unwrap_or(service),
        "source": service,
        "handle": issue
            .author
            .as_ref()
            .and_then(|author| author.handle.clone())
            .unwrap_or_default(),
        "url": issue.url,
        "reference": issue.identifier,
        "created": time_arg(&issue.created_at),
        "updated": time_arg(&issue.updated_at),
    })
}

/// A moment a source writes as RFC 3339, as Tasks takes it; `null` where the source says none.
fn time_arg(at: &str) -> Value {
    if at.trim().is_empty() {
        Value::Null
    } else {
        json!(at.trim())
    }
}

/// The status Tasks calls the issue's, from the service's word.
fn task_status(status: &str) -> &'static str {
    match status {
        "todo" => "todo",
        "in-progress" => "in_progress",
        "in-review" => "in_review",
        "done" => "done",
        "cancelled" => "cancelled",
        _ => "backlog",
    }
}

/// The action that sets `priority`, when the service names one.
fn priority_action(priority: &str) -> Option<&'static str> {
    Some(match priority {
        "urgent" => "set_priority_urgent",
        "high" => "set_priority_high",
        "medium" => "set_priority_medium",
        "low" => "set_priority_low",
        _ => return None,
    })
}

/// What a comment is made from: the task it is said about, as a reference argument.
fn made_from(address: &str) -> Value {
    tilo_actions::params::MadeFrom {
        resource: address.to_owned(),
        kind: String::new(),
    }
    .to_json()
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

/// The id Tasks answered for the resource it made: the id a resource the action returned names
/// (`{"app", "kind", "id", "label"}`), or the id in what it said it made.
pub(crate) fn created_id(answer: &super::writer::Answer) -> Option<String> {
    let value = answer.result()?;
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        return Some(id.to_owned());
    }
    // What an action said it made, per item: its id and the address it is published under.
    if let Some(id) = value
        .get("made")
        .and_then(Value::as_array)
        .and_then(|made| made.first())
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
    {
        return Some(id.to_owned());
    }
    for key in ["issue"] {
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
