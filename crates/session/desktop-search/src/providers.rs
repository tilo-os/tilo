//! Portable sources: everything a desktop can search without a compositor.
//! Each takes a snapshot the host collects when search opens, of what is
//! published (resources, actions, the desktop's model) or what a service
//! answers; none reads a host's own chrome state.

#[cfg(feature = "catalogue")]
pub mod catalogue;
#[cfg(feature = "indexes")]
pub mod files;
pub mod host_apps;
pub mod jobs;
pub mod people;
pub mod places;
pub mod records;
#[cfg(feature = "settings")]
pub mod settings;
pub mod waiting;

use std::cell::RefCell;
use std::collections::BTreeMap;

use tilo_actions::{ActionAddress, Availability, ResourceSpec};
use ui::search::{PagePoll, PendingToken, SearchQuery};

use crate::model::{ActionTarget, Hit, Source, Target};
use crate::rank;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, rank_fields};

/// An application the desktop can open. A desktop lists only the apps it
/// can run ([`crate::availability::runs_on`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub title: String,
    /// What kind of app it is ("Office"); the registry's when not given.
    pub category: Option<String>,
    /// Where it runs, when that is not this desktop ("Runs on the node").
    pub description: Option<String>,
}

/// Applications, each offered under its own name. A row says what the app
/// is (its description, else its category), never its package id: the id
/// tells builds apart for a developer, a person recognises the name. The id
/// stays searchable.
pub struct Applications {
    entries: Vec<AppEntry>,
    /// Each app's searchable text, built once with the list.
    fields: Vec<Vec<(String, ui::search::FieldWeight)>>,
    /// The apps' searchable text, indexed while it stays the same.
    ranker: ui::search::ListIndex,
}

impl Applications {
    #[must_use]
    pub fn new(mut entries: Vec<AppEntry>) -> Self {
        for app in &mut entries {
            if app.category.is_none() {
                app.category = crate::availability::category_for(&app.id).map(str::to_owned);
            }
        }
        let fields = entries
            .iter()
            .map(|app| {
                rank::application_fields(
                    &app.title,
                    &app.id,
                    app.category.as_deref(),
                    app.description.as_deref(),
                )
            })
            .collect();
        Self {
            entries,
            fields,
            ranker: ui::search::ListIndex::default()
                .with_ranks(ui::search::system::application_ranks),
        }
    }
}

impl Source for Applications {
    fn id(&self) -> &'static str {
        "applications"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let browsing = needle.trim().is_empty();
        let ranks = rank::rank_rows(&self.ranker, needle, &self.fields);
        self.entries
            .iter()
            .zip(ranks)
            .filter_map(|(app, matched)| {
                let matched = matched?;
                Some(Hit {
                    kind: ResourceKind::Application,
                    provider: self.id().into(),
                    id: app.id.clone(),
                    title: app.title.clone(),
                    subtitle: app.description.clone().or_else(|| app.category.clone()),
                    facts: Vec::new(),
                    score: if browsing {
                        ui::search::system::starting_point_rank(&app.id)
                    } else {
                        matched
                    },
                    icon: None,
                    target: Target::Launch {
                        app: app.id.clone(),
                    },
                })
            })
            .collect()
    }
}

/// A window open on the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowEntry {
    pub tab: u64,
    pub title: String,
    /// The app showing it: its name, and its package id for its glyph.
    pub app_title: String,
    pub app: Option<String>,
    /// Facts about it: who holds it, where it is, and what tells it from a
    /// window titled the same.
    pub place: Vec<String>,
}

pub const WINDOWS_KIND: &str = "Windows";

/// Open windows; choosing one focuses it.
pub struct Windows(pub Vec<WindowEntry>);

impl Source for Windows {
    fn id(&self) -> &'static str {
        "desktop-windows"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        self.0
            .iter()
            .filter_map(|window| {
                let score = if needle.trim().is_empty() {
                    60
                } else {
                    rank::window_match_score(needle, &[&window.title, &window.app_title])?
                };
                Some(Hit {
                    kind: ResourceKind::Custom(WINDOWS_KIND.into()),
                    provider: self.id().into(),
                    id: window.tab.to_string(),
                    title: window.title.clone(),
                    subtitle: None,
                    facts: window.place.clone(),
                    score,
                    icon: None,
                    target: Target::Window {
                        tab: window.tab,
                        app: window.app.clone(),
                    },
                })
            })
            .collect()
    }
}

/// A host command the palette offers (close the focused window, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
}

pub struct Commands(pub Vec<CommandSpec>);

impl Source for Commands {
    fn id(&self) -> &'static str {
        "desktop-commands"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        self.0
            .iter()
            .filter_map(|command| {
                let score = if needle.trim().is_empty() {
                    20
                } else {
                    rank_fields(
                        needle,
                        &[
                            (&command.title, ui::search::FieldWeight::TITLE),
                            (
                                command.subtitle.as_deref().unwrap_or_default(),
                                ui::search::FieldWeight::SUBTITLE,
                            ),
                        ],
                    )?
                };
                Some(Hit {
                    kind: ResourceKind::Action,
                    provider: self.id().into(),
                    id: command.id.clone(),
                    title: command.title.clone(),
                    subtitle: command.subtitle.clone(),
                    facts: Vec::new(),
                    score,
                    icon: None,
                    target: Target::Command {
                        id: command.id.clone(),
                    },
                })
            })
            .collect()
    }
}

/// Every resource of `resources`, nested ones included, by id.
fn index(resources: &[ResourceSpec]) -> BTreeMap<String, &ResourceSpec> {
    fn visit<'a>(resource: &'a ResourceSpec, out: &mut BTreeMap<String, &'a ResourceSpec>) {
        out.entry(resource.id.as_str().to_owned())
            .or_insert(resource);
        for child in &resource.children {
            visit(child, out);
        }
    }
    let mut out = BTreeMap::new();
    for resource in resources {
        visit(resource, &mut out);
    }
    out
}

/// Whether a person may pick this action from a palette now.
fn offered(spec: &tilo_actions::ActionSpec) -> bool {
    spec.reaches_people() && matches!(spec.contract.availability, Availability::Available)
}

/// Whether `needle` asks for this action whatever its availability: an
/// action with nothing to act on now (closing a sheet that is not open)
/// takes no row from one that would do something, but naming it exactly
/// still offers it, and running it says why not.
fn offered_for(spec: &tilo_actions::ActionSpec, needle: &str) -> bool {
    offered(spec)
        || (spec.reaches_people()
            && ui::search::fold_search_text(needle.trim())
                == ui::search::fold_search_text(&spec.label))
}

fn action_target(resource: &ResourceSpec, action: &str) -> Option<ActionTarget> {
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .filter(|spec| offered(spec))?;
    Some(ActionTarget {
        resource: resource.id.clone(),
        resource_kind: resource.kind.clone(),
        resource_label: resource.label.clone(),
        spec: spec.clone(),
        preset: None,
    })
}

/// The actions open applications publish (`tilo_actions`), the same
/// catalogue their own controls, agents and remote peers invoke.
pub struct Actions(pub Vec<ResourceSpec>);

impl Source for Actions {
    fn id(&self) -> &'static str {
        "app-actions"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        // Every open window's actions would bury everything else unasked.
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let records: std::collections::HashSet<&str> =
            self.0.iter().map(|record| record.id.as_str()).collect();
        tilo_actions::flatten(&self.0)
            .into_iter()
            .filter(|flat| offered_for(flat.action, needle))
            .filter_map(|flat| {
                // An open record's actions answer to what they do. An item's
                // (one message of a chat, one card of a board) answer only to
                // the item named: "edit message" must not list the same edit
                // once for every message there is.
                let score = if records.contains(flat.resource_id.as_str()) {
                    rank::action_match_score(
                        needle,
                        &flat.action.label,
                        &flat.action.keywords,
                        &flat.resource_label,
                    )?
                } else {
                    rank::item_action_match_score(needle, &flat.resource_label)?
                };
                Some(Hit {
                    kind: ResourceKind::Action,
                    provider: self.id().into(),
                    id: flat.key.clone(),
                    title: flat.action.label.clone(),
                    subtitle: Some(flat.resource_label.clone()),
                    facts: Vec::new(),
                    score,
                    icon: crate::window::action_icon(flat.action),
                    target: Target::Action(Box::new(ActionTarget {
                        resource: flat.resource_id.clone(),
                        resource_kind: flat.resource_kind.clone(),
                        resource_label: flat.resource_label.clone(),
                        spec: flat.action.clone(),
                        preset: None,
                    })),
                })
            })
            .collect()
    }
}

/// What one app publishes in one room, as the session's action host lists
/// it: inert descriptors.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishedActions {
    pub app: String,
    /// The room (workspace) it publishes them in.
    pub room: String,
    pub actions: Vec<tilo_actions::ActionDescriptor>,
}

/// Whether a person may pick `action` for `needle`: it reaches people, and
/// it has something to act on now or `needle` names it exactly (where
/// running it says why not).
fn descriptor_offered(action: &tilo_actions::ActionDescriptor, needle: &str) -> bool {
    action.contract.reaches_people()
        && (action.contract.availability.is_available()
            || ui::search::fold_search_text(needle.trim())
                == ui::search::fold_search_text(&action.action_label))
}

/// The actions providers publish through the session's action host, the
/// same surface their own controls, agents and other devices invoke:
/// ranked by label, other words and what they act on, run by address.
pub struct Published(pub Vec<PublishedActions>);

impl Published {
    /// The id of one action's row.
    fn row_id(published: &PublishedActions, address: &ActionAddress) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            published.app, published.room, address.resource, address.action
        )
    }
}

impl Source for Published {
    fn id(&self) -> &'static str {
        "published-actions"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        // Every provider's actions would bury everything else unasked.
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for published in &self.0 {
            for action in &published.actions {
                if !descriptor_offered(action, needle) {
                    continue;
                }
                let Some(score) = rank::action_match_score(
                    needle,
                    &action.action_label,
                    &action.keywords,
                    &action.resource_label,
                ) else {
                    continue;
                };
                hits.push(Hit {
                    kind: ResourceKind::Action,
                    provider: self.id().into(),
                    id: Self::row_id(published, &action.address),
                    title: action.action_label.clone(),
                    subtitle: action.description.clone(),
                    facts: Vec::new(),
                    score,
                    icon: action.icon.as_deref().and_then(ui::icons::Icon::from_name),
                    target: Target::Invoke(Box::new(crate::model::InvokeTarget {
                        app: published.app.clone(),
                        room: published.room.clone(),
                        address: action.address.clone(),
                        arguments: serde_json::Map::new(),
                        descriptor: Some(action.clone()),
                    })),
                });
            }
        }
        // Equal matches: the action that does it in one step before the
        // one asking for input, and a switch before a view of the same
        // thing.
        hits.sort_by_key(|hit| {
            let descriptor = match &hit.target {
                Target::Invoke(target) => target.descriptor.as_ref(),
                _ => None,
            };
            let one_step = descriptor.is_some_and(|action| action.params.is_empty());
            let toggles = descriptor.is_some_and(|action| action.state.toggles);
            (
                std::cmp::Reverse(hit.score),
                2_i32
                    .saturating_sub(i32::from(one_step))
                    .saturating_sub(i32::from(toggles)),
            )
        });
        hits
    }
}

/// The palette kind for an application provider's group label.
fn content_kind(label: &str) -> ResourceKind {
    match label {
        "Notes" => ResourceKind::Note,
        "Files" => ResourceKind::File,
        "Folders" => ResourceKind::Folder,
        "Videos" => ResourceKind::Video,
        "Playlists" => ResourceKind::Playlist,
        "Creators" => ResourceKind::Creator,
        "Records" | "Resources" | "Open resources" => ResourceKind::Record,
        other => ResourceKind::Custom(other.to_owned()),
    }
}

/// A provider matched this hit by something other than its visible
/// names (a body, a tag): still a match, below any named one.
const CONTENT_MATCH_SCORE: i32 = 10;

/// What open applications find through their own search providers (a
/// notebook's pages, a mailbox's messages). Only hits addressing an action
/// the application currently exports are offered: a provider discovers,
/// it never grants.
///
/// An application may answer a query later than it is asked (a video
/// service's results, a row offering to search the service for what was
/// typed): its provider hands back a pending token, and [`Self::poll`] tells
/// the palette once the answer is in, so the rows are built again.
pub struct Content {
    providers: Vec<Box<dyn ui::search::SearchProvider>>,
    resources: Vec<ResourceSpec>,
    /// The part of the application a search it opened is narrowed to (a
    /// [`ui::search::ScopeDecl`] anchor).
    scope: Option<String>,
    limit: usize,
    /// Whether nothing typed asks the providers for what they list unasked
    /// (a file manager's places, a notebook's notes).
    browse: bool,
    /// The answers still coming, by provider.
    pending: RefCell<Vec<(usize, PendingToken)>>,
}

impl Content {
    pub fn new(
        providers: Vec<Box<dyn ui::search::SearchProvider>>,
        resources: Vec<ResourceSpec>,
    ) -> Self {
        Self {
            providers,
            resources,
            scope: None,
            limit: 16,
            browse: false,
            pending: RefCell::default(),
        }
    }

    /// With nothing typed, list what the providers list unasked: a search
    /// of one window shows what it holds before anything is typed, as the
    /// app shows it.
    #[must_use]
    pub fn browsing(mut self) -> Self {
        self.browse = true;
        self
    }

    /// Narrow every query to `scope`, one part of the application.
    #[must_use]
    pub fn within(mut self, scope: Option<String>) -> Self {
        self.scope = scope;
        self
    }

    /// Ask the providers whose answers were still coming. Returns whether
    /// one arrived, so the query should run again.
    pub fn poll(&self) -> bool {
        let waiting = std::mem::take(&mut *self.pending.borrow_mut());
        let mut arrived = false;
        let mut still = Vec::new();
        for (at, token) in waiting {
            let Some(provider) = self.providers.get(at) else {
                continue;
            };
            match provider.poll_page(&token) {
                PagePoll::Pending => still.push((at, token)),
                PagePoll::Ready(_) | PagePoll::Answered(_) => arrived = true,
            }
        }
        *self.pending.borrow_mut() = still;
        arrived
    }

    /// Whether an answer is still coming.
    pub fn waiting(&self) -> bool {
        !self.pending.borrow().is_empty()
    }
}

impl Source for Content {
    fn id(&self) -> &'static str {
        "app-content"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() && !self.browse {
            return Vec::new();
        }
        let resources = index(&self.resources);
        let mut query = SearchQuery::new(needle);
        if let Some(scope) = &self.scope {
            query = query.within(scope.clone());
        }
        let mut pending = Vec::new();
        let mut hits = Vec::new();
        for (at, provider) in self.providers.iter().enumerate() {
            let page = provider.search_page(&query, None, self.limit, self.limit);
            if let Some(token) = page.pending {
                pending.push((at, token));
            }
            for result in &page.results {
                // A hit naming no action of its own (a video, a search of
                // the app's service) opens through the app's `open`, at the
                // hit's anchor, as picking it inside the app does.
                let (address, anchor) = match result.action_address() {
                    Some(address) => (address, None),
                    None => match provider.open_action() {
                        Some(open) => (open, Some(result.action_id.clone())),
                        None => continue,
                    },
                };
                let resolved;
                let resource = if let Some(resource) = resources.get(&address.resource) {
                    *resource
                } else {
                    let Some(found) = provider.resolve_action_resource(&address) else {
                        continue;
                    };
                    resolved = found;
                    &resolved
                };
                let Some(mut target) = action_target(resource, &address.action) else {
                    continue;
                };
                let mut id = key(&address);
                if let Some(anchor) = anchor {
                    let Some(param) = target.spec.params().first() else {
                        continue;
                    };
                    id = format!("{id}\u{1f}{anchor}");
                    target.preset = Some((param.name.to_string(), anchor));
                }
                let score = rank::record_match_score(needle, &result.title, &address.resource)
                    .unwrap_or(CONTENT_MATCH_SCORE)
                    .min(MAX_PROVIDER_SCORE);
                hits.push(Hit {
                    kind: content_kind(&result.kind),
                    provider: self.id().into(),
                    id,
                    title: result.title.clone(),
                    subtitle: result.subtitle.clone(),
                    facts: Vec::new(),
                    score,
                    icon: result.icon,
                    target: Target::Action(Box::new(target)),
                });
            }
        }
        *self.pending.borrow_mut() = pending;
        hits
    }
}

fn key(address: &ActionAddress) -> String {
    format!("{}\u{1f}{}", address.resource, address.action)
}

/// The kind answers are grouped under.
/// The kind worked-out answers are grouped under, in the desktop's
/// language.
#[must_use]
pub fn answers_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-answers"))
}

/// How long fetched exchange rates answer without saying how old they are.
const RATES_STALE_SECS: u64 = 24 * 60 * 60;

/// What was typed, worked out ([`crate::answers`]): a sum, a conversion or
/// an amount of money, which choosing copies, and an address, which
/// choosing opens. Rows are addressed by what they answer (`sum:12*7`), so
/// a row resolves without the query that found it.
#[derive(Default)]
pub struct Answers {
    /// The exchange rates the host last fetched; money is answered only
    /// with them.
    pub rates: Option<crate::answers::currency::Rates>,
    /// Now, in Unix seconds: how old the rates are.
    pub now: u64,
}

impl Answers {
    /// The rows answering `typed`.
    fn answers(&self, typed: &str) -> Vec<Hit> {
        use crate::answers::{address, evaluate, format_number, units};
        use crate::i18n::t_args;
        let typed = typed.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        let kind = answers_kind();
        let answer = |id: &str, title: String, subtitle: Option<String>, target: Target| Hit {
            kind: kind.clone(),
            provider: self.id().into(),
            id: format!("{id}:{typed}"),
            title,
            subtitle,
            facts: Vec::new(),
            score: MAX_PROVIDER_SCORE,
            icon: None,
            target,
        };
        let sum =
            |expression: String| t_args("search-answer-sum", &[("expression", expression.into())]);
        let mut out = Vec::new();
        if let Some(value) = evaluate(typed) {
            let text = format_number(value);
            out.push(answer(
                "sum",
                text.clone(),
                Some(sum(typed.to_owned())),
                Target::Copy { text },
            ));
        }
        if let Some(conversion) = units::convert(typed) {
            out.push(answer(
                "unit",
                conversion.to.clone(),
                Some(sum(conversion.from)),
                Target::Copy {
                    text: conversion.to,
                },
            ));
        }
        if let Some(rates) = &self.rates
            && let Some((from, to)) = crate::answers::currency::convert(typed, rates)
        {
            let age = self.now.saturating_sub(rates.fetched_at);
            let subtitle = if age > RATES_STALE_SECS {
                t_args(
                    "search-answer-money-stale",
                    &[
                        ("amount", from.into()),
                        (
                            "age",
                            ui::format::relative(i64::try_from(age).unwrap_or(i64::MAX)).into(),
                        ),
                    ],
                )
            } else {
                sum(from)
            };
            out.push(answer(
                "money",
                to.clone(),
                Some(subtitle),
                Target::Copy { text: to },
            ));
        }
        if let Some((uri, shown)) = address(typed) {
            out.push(answer(
                "address",
                t_args("search-answer-open-address", &[("address", shown.into())]),
                None,
                Target::Open {
                    uri,
                    fallback: None,
                },
            ));
        }
        out
    }

    /// The row `id` names (`sum:12*7`).
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<Hit> {
        let (_, typed) = id.split_once(':')?;
        self.answers(typed).into_iter().find(|hit| hit.id == id)
    }
}

impl Source for Answers {
    fn id(&self) -> &'static str {
        "quick-answers"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        self.answers(needle)
    }
}

/// The kind "Search the web for …" is grouped under, in the desktop's
/// language.
#[must_use]
pub fn web_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-web"))
}

/// The kind the web services named in a query are grouped under, in the
/// desktop's language.
#[must_use]
pub fn websites_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-websites"))
}

/// A service row's score when the query names the service alone: below an
/// exact name's, so an app named the same way still leads.
const SERVICE_SCORE: i32 = 60;

/// What a service row gives way at the merge when an app serves the same
/// service (Tube for `YouTube`): enough that the app's row leads the service
/// named alone.
const SERVED_SERVICE_YIELD: f32 = -1.5;

/// What a row every query gets gives way: more than any direct match's
/// strength (a whole title, the provider's top score and an app's launch
/// intent together), so it is always last.
const FALLBACK_INTENT: f32 = -10.0;

/// The web as a source ([`crate::answers::web`]): "Search the web for …"
/// for every query, last, and the web services a query names ("youtube
/// cats").
///
/// A web search is handed to the browser as `web+search:`, which runs it on
/// the engine the person chose, and opens on
/// [`crate::answers::web::FALLBACK_ENGINE`] where nothing takes that scheme
/// (a desktop inside a web browser). A service a Tilo app serves (Tube for
/// `YouTube`) is searched in that app while it is installed
/// ([`Self::app_for_scheme`]), on the web otherwise.
pub struct WebSearch {
    /// The installed app that claims a URI scheme, by name; `None` when
    /// none does.
    pub app_for_scheme: fn(&str) -> Option<String>,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self {
            app_for_scheme: |_| None,
        }
    }
}

impl WebSearch {
    fn web_search_row(&self, query: &str) -> Hit {
        use crate::answers::web::{fallback_search_url, web_search_uri};
        Hit {
            kind: web_kind(),
            provider: self.id().into(),
            id: format!("search:{query}"),
            title: crate::i18n::t_args("search-web-for", &[("query", query.into())]),
            subtitle: Some(crate::i18n::t("search-web-subtitle")),
            facts: Vec::new(),
            score: 1,
            icon: Some(ui::icons::Icon::Globe),
            target: Target::Open {
                uri: web_search_uri(query),
                fallback: Some(fallback_search_url(query)),
            },
        }
    }

    /// The service named alone: opens its site.
    fn service_row(&self, service: &'static crate::answers::web::Service) -> Hit {
        Hit {
            kind: websites_kind(),
            provider: self.id().into(),
            id: format!("service:{}", service.id),
            title: service.name.into(),
            subtitle: Some(service.domain.into()),
            facts: Vec::new(),
            score: SERVICE_SCORE,
            icon: Some(service.icon),
            target: Target::Open {
                uri: service.home.into(),
                fallback: None,
            },
        }
    }

    /// "Search `service` for `query`": in the app that serves it when one
    /// is installed, on the web otherwise.
    fn service_search_row(
        &self,
        service: &'static crate::answers::web::Service,
        query: &str,
    ) -> Hit {
        let web = service.web_search(query);
        let in_app = service
            .app_search(query)
            .and_then(|(uri, scheme)| Some((uri, (self.app_for_scheme)(scheme)?)));
        let (subtitle, target) = match in_app {
            Some((uri, app)) => (
                app,
                // Removed since the row was built: the web serves.
                Target::Open {
                    uri,
                    fallback: Some(web),
                },
            ),
            None => (
                service.domain.to_owned(),
                Target::Open {
                    uri: web,
                    fallback: None,
                },
            ),
        };
        Hit {
            kind: websites_kind(),
            provider: self.id().into(),
            id: format!("query:{}:{query}", service.id),
            title: crate::i18n::t_args(
                "search-web-service-for",
                &[("service", service.name.into()), ("query", query.into())],
            ),
            subtitle: Some(subtitle),
            facts: Vec::new(),
            score: MAX_PROVIDER_SCORE,
            icon: Some(service.icon),
            target,
        }
    }

    /// The row `id` names: what it answers is in its address
    /// (`search:cats`, `query:youtube:cats`).
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<Hit> {
        use crate::answers::web::Service;
        let (row, rest) = id.split_once(':')?;
        let found = match row {
            "search" if !rest.trim().is_empty() => self.web_search_row(rest),
            "service" => self.service_row(Service::by_id(rest)?),
            "query" => {
                let (service, query) = rest.split_once(':')?;
                self.service_search_row(Service::by_id(service)?, query)
            }
            _ => return None,
        };
        (found.id == id).then_some(found)
    }

    /// Whether `hit` is the row every query gets.
    #[must_use]
    pub fn is_fallback(hit: &Hit) -> bool {
        hit.id.starts_with("search:")
    }
}

impl Source for WebSearch {
    fn id(&self) -> &'static str {
        "web"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        use crate::answers::web::{Named, named};
        let typed = needle.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        match named(typed) {
            Some(Named::Search(service, query)) => {
                out.push(self.service_search_row(service, &query));
            }
            Some(Named::Service(service)) => out.push(self.service_row(service)),
            None => {}
        }
        out.push(self.web_search_row(typed));
        out
    }

    /// "Search the web for …" answers every query, so it comes after every
    /// row that answers it directly (its title holds what was typed and
    /// would otherwise lead). A query naming a service is worth what the
    /// row's title typed whole is; a service named alone yields to the app
    /// that serves it.
    fn intent(&self, hit: &Hit) -> f32 {
        let Some((row, rest)) = hit.id.split_once(':') else {
            return 0.0;
        };
        match row {
            "search" => FALLBACK_INTENT,
            "query" => {
                ui::search::system::lexical_strength(&hit.title, &hit.kind, &hit.title, None)
            }
            "service"
                if crate::answers::web::Service::by_id(rest)
                    .is_some_and(|service| service.app.is_some()) =>
            {
                SERVED_SERVICE_YIELD
            }
            _ => 0.0,
        }
    }
}

pub const RECENT_KIND: &str = "Recent";

/// What this palette opened lately, newest first, offered while the query
/// is empty.
#[derive(Default)]
pub struct Recents(pub Vec<Hit>);

/// How many recent choices a palette remembers.
pub const MAX_RECENTS: usize = 6;

impl Recents {
    /// Remember `hit` as the newest choice. An answer or a web search is
    /// what one query typed came to, nothing to go back to.
    pub fn record(&mut self, hit: &Hit) {
        if matches!(
            hit.target,
            Target::Copy { .. } | Target::Open { .. } | Target::Cancel { .. }
        ) {
            return;
        }
        self.0
            .retain(|recent| !(recent.provider == hit.provider && recent.id == hit.id));
        self.0.insert(0, hit.clone());
        self.0.truncate(MAX_RECENTS);
    }
}

impl Source for Recents {
    fn id(&self) -> &'static str {
        "recent"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if !needle.trim().is_empty() {
            return Vec::new();
        }
        self.0
            .iter()
            .enumerate()
            .map(|(position, hit)| Hit {
                kind: ResourceKind::Custom(RECENT_KIND.into()),
                provider: self.id().into(),
                id: format!("{}\u{1f}{}", hit.provider, hit.id),
                score: MAX_PROVIDER_SCORE
                    .saturating_sub(i32::try_from(position).unwrap_or(MAX_PROVIDER_SCORE)),
                ..hit.clone()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::search;

    fn apps() -> Applications {
        Applications::new(vec![
            AppEntry {
                id: "so.tilo.sheets".into(),
                title: "Sheets".into(),
                category: Some("Office".into()),
                description: None,
            },
            AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notes".into(),
                category: Some("Office".into()),
                description: None,
            },
        ])
    }

    #[test]
    fn an_app_row_says_what_the_app_is_never_its_package_id() {
        let apps = Applications::new(vec![
            AppEntry {
                id: "so.tilo.calendar".into(),
                title: "Calendar".into(),
                category: None,
                description: None,
            },
            AppEntry {
                id: "node-app:3".into(),
                title: "Terminal".into(),
                category: None,
                description: Some("Runs on the node".into()),
            },
            AppEntry {
                id: "com.example.unknown".into(),
                title: "Unknown".into(),
                category: None,
                description: None,
            },
        ]);
        for needle in ["", "calendar", "term", "unknown"] {
            for hit in apps.query(needle) {
                let subtitle = hit.subtitle.as_deref().unwrap_or_default();
                assert!(
                    !subtitle.contains(&hit.id) && !subtitle.contains("so.tilo"),
                    "{needle:?}: {subtitle:?}"
                );
                assert!(!hit.title.contains(&hit.id), "{needle:?}: {}", hit.title);
            }
        }
        let subtitle = |needle: &str, id: &str| {
            apps.query(needle)
                .into_iter()
                .find(|hit| hit.id == id)
                .and_then(|hit| hit.subtitle)
        };
        assert_eq!(subtitle("", "so.tilo.calendar").as_deref(), Some("Office"));
        assert_eq!(
            subtitle("cal", "so.tilo.calendar").as_deref(),
            Some("Office")
        );
        assert_eq!(
            subtitle("", "node-app:3").as_deref(),
            Some("Runs on the node")
        );
        assert_eq!(subtitle("", "com.example.unknown"), None);
        assert!(!apps.query("so.tilo.calendar").is_empty());
    }

    /// The apps of the registry that run on `host`, as a desktop there
    /// lists them, plus `extra`.
    fn apps_on(host: &[&str], extra: Vec<AppEntry>) -> Applications {
        Applications::new(
            crate::availability::catalog()
                .iter()
                .filter(|app| crate::availability::runs_on(app.id, host))
                .map(|app| AppEntry {
                    id: app.id.into(),
                    title: app.name.into(),
                    category: None,
                    description: None,
                })
                .chain(extra)
                .collect(),
        )
    }

    fn ranked_ids(apps: &Applications, query: &str) -> Vec<String> {
        search(query, &[apps], 0)
            .into_iter()
            .flat_map(|group| group.results)
            .map(|hit| hit.id)
            .collect()
    }

    #[test]
    fn a_name_begun_or_a_word_of_it_leads_a_keyword_or_a_fragment() {
        let everywhere = apps_on(&["linux", "macos", "web", "tilo-os"], Vec::new());
        // "term" begins Terminal's name; it only begins Calendar's German
        // keyword ("Termin").
        let term = ranked_ids(&everywhere, "term");
        assert_eq!(term.first().map(String::as_str), Some("so.tilo.terminal"));
        let calendar = term.iter().position(|id| id == "so.tilo.calendar");
        assert!(calendar.is_none_or(|at| at > 0), "{term:?}");
        // A word of the name ("Editor" of "Hex Editor") leads an app whose
        // name only contains the letters.
        let editor = ranked_ids(&everywhere, "editor");
        let first = everywhere
            .entries
            .iter()
            .find(|app| Some(&app.id) == editor.first())
            .expect("something is an editor");
        assert!(
            first
                .title
                .to_lowercase()
                .split_whitespace()
                .any(|word| word == "editor"),
            "{editor:?}"
        );

        // In a browser beside a node that runs Terminal, the node's
        // Terminal is what "term" opens.
        let browser = apps_on(
            &["web"],
            vec![AppEntry {
                id: "node-app:3".into(),
                title: "Terminal".into(),
                category: None,
                description: Some("Runs on the node".into()),
            }],
        );
        let term = ranked_ids(&browser, "term");
        assert_eq!(term.first().map(String::as_str), Some("node-app:3"));
        assert!(!term.iter().any(|id| id == "so.tilo.terminal"));
    }

    fn resource_with_action(
        id: &str,
        label: &str,
        action: &str,
        action_label: &str,
    ) -> ResourceSpec {
        let mut resource = ResourceSpec::new(id, "notes.page", label);
        resource
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                action,
                action_label,
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.action".into(),
                    payload: Vec::new(),
                },
            ));
        resource
    }

    #[test]
    fn typing_an_app_name_puts_its_launch_first() {
        let windows = Windows(vec![WindowEntry {
            tab: 7,
            title: "Budget sheets".into(),
            app_title: "Notes".into(),
            app: None,
            place: vec!["Workspace".into(), "Main".into()],
        }]);
        let groups = search("sheets", &[&apps(), &windows], 10);
        let first = &groups[0].results[0];
        assert_eq!(first.title, "Sheets");
        assert_eq!(
            first.target,
            Target::Launch {
                app: "so.tilo.sheets".into()
            }
        );
        assert!(groups.iter().any(|g| g.kind.label() == WINDOWS_KIND));
    }

    #[test]
    fn empty_query_browses_applications_and_windows_but_not_actions() {
        let actions = Actions(vec![resource_with_action(
            "p1",
            "Groceries",
            "open",
            "Open page",
        )]);
        let windows = Windows(vec![WindowEntry {
            tab: 1,
            title: "Notes".into(),
            app_title: "Notes".into(),
            app: None,
            place: vec!["Workspace".into()],
        }]);
        let groups = search("", &[&apps(), &windows, &actions], 0);
        let kinds: Vec<&str> = groups.iter().map(|g| g.kind.label()).collect();
        assert!(kinds.contains(&"Apps"));
        assert!(kinds.contains(&WINDOWS_KIND));
        assert!(!kinds.contains(&"Actions"));
    }

    #[test]
    fn app_actions_are_found_by_label_and_resource() {
        let actions = Actions(vec![resource_with_action(
            "page-1",
            "Groceries",
            "archive",
            "Archive page",
        )]);
        let hits = actions.query("archive");
        assert_eq!(hits.len(), 1);
        let Target::Action(target) = &hits[0].target else {
            panic!("an action hit runs an action");
        };
        assert_eq!(target.address(), ActionAddress::new("page-1", "archive"));
        assert_eq!(hits[0].subtitle.as_deref(), Some("Groceries"));
        assert_eq!(actions.query("groceries").len(), 1);
        assert!(actions.query("zebra").is_empty());
    }

    /// An item's kind is its collection's field and an action's id is a
    /// name for code, so neither finds an action: "messages" must not offer
    /// every action on every message of a chat. An item's actions come with
    /// the item named, never once per item for the action's own label.
    #[test]
    fn an_action_is_found_by_what_its_row_says_and_an_items_by_the_item() {
        let topics = ["Lunch plans", "Quarterly budget", "Holiday photos"];
        let messages = topics.iter().enumerate().map(|(n, topic)| {
            let mut message = resource_with_action(
                &format!("so.tilo.chat/messages/{n}"),
                topic,
                "pin-message",
                "Pin message",
            );
            message.kind = "so.tilo.chat.messages".into();
            message
        });
        let chat = resource_with_action("so.tilo.chat", "Chat", "report", "Report a problem")
            .with_children(messages);
        let actions = Actions(vec![chat]);
        assert!(actions.query("messages").is_empty());
        assert!(actions.query("pin-message").is_empty());
        assert!(actions.query("pin").is_empty(), "not once per message");
        let budget = actions.query("quarterly budget");
        assert_eq!(budget.len(), 1);
        assert_eq!(
            budget.first().map(|hit| hit.title.as_str()),
            Some("Pin message")
        );
        assert_eq!(actions.query("report").len(), 1, "the record's own action");
    }

    #[test]
    fn an_unavailable_action_answers_only_its_name() {
        let mut resource = resource_with_action("r", "Doc", "print", "Print");
        resource.actions[0].type_mut().contract.availability = Availability::Unavailable {
            code: "offline".into(),
            reason: "Offline".into(),
        };
        let actions = Actions(vec![resource]);
        assert!(actions.query("prin").is_empty());
        assert_eq!(
            actions.query("print").len(),
            1,
            "named exactly, it says why not"
        );
    }

    struct Pages;
    impl ui::search::SearchProvider for Pages {
        fn name(&self) -> &'static str {
            "Notes"
        }
        fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
            ["Groceries", "Trip plan", "Hidden"]
                .into_iter()
                .filter(|title| title.to_lowercase().contains(&query.to_lowercase()))
                .map(|title| {
                    let resource = if title == "Hidden" { "gone" } else { title };
                    ui::search::SearchResult::for_action(
                        "Notes",
                        title,
                        ActionAddress::new(resource, "open"),
                    )
                })
                .collect()
        }
    }

    #[test]
    fn application_content_is_found_by_title_and_opens_through_its_action() {
        let resources = vec![
            resource_with_action("Groceries", "Groceries", "open", "Open"),
            resource_with_action("Trip plan", "Trip plan", "open", "Open"),
        ];
        let content = Content::new(vec![Box::new(Pages)], resources);
        let hits = content.query("trip");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, ResourceKind::Note);
        assert_eq!(hits[0].title, "Trip plan");
        assert!(
            matches!(&hits[0].target, Target::Action(t) if t.address() == ActionAddress::new("Trip plan", "open"))
        );
        // A hit whose address the application no longer exports is dropped.
        assert!(content.query("hidden").is_empty());
    }

    /// An app whose rows for a query arrive after it is asked, as a
    /// framework app's do: it takes the query in on its next frame, then
    /// offers to search its service for it ("Search `YouTube` for …").
    /// Each query the app was asked, with the part of it it was narrowed to.
    type Asked = std::rc::Rc<RefCell<Vec<(String, Option<String>)>>>;

    struct Service {
        arrived: std::rc::Rc<std::cell::Cell<bool>>,
        asked: Asked,
    }
    impl ui::search::SearchProvider for Service {
        fn name(&self) -> &'static str {
            "Videos"
        }
        fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
            Vec::new()
        }
        fn search_page(
            &self,
            query: &SearchQuery,
            _: Option<&ui::search::SearchCursor>,
            _: usize,
            _: usize,
        ) -> ui::search::SearchPage {
            self.asked
                .borrow_mut()
                .push((query.text().to_owned(), query.scope().map(str::to_owned)));
            if !self.arrived.get() {
                let mut page = ui::search::SearchPage::empty();
                page.pending = Some(PendingToken::new(1));
                return page;
            }
            let row = ui::search::SearchResult::for_action(
                "Videos",
                format!("Search the service for “{}”", query.text()),
                ActionAddress::new("videos", "search"),
            );
            ui::search::SearchPage::of(vec![row], None)
        }
        fn poll_page(&self, _: &PendingToken) -> PagePoll {
            if self.arrived.get() {
                PagePoll::Answered(ui::search::SearchPage::empty())
            } else {
                PagePoll::Pending
            }
        }
    }

    #[test]
    fn an_apps_answer_that_comes_later_is_shown_once_it_arrives() {
        let arrived = std::rc::Rc::new(std::cell::Cell::new(false));
        let asked = std::rc::Rc::default();
        let service = Service {
            arrived: std::rc::Rc::clone(&arrived),
            asked: std::rc::Rc::clone(&asked),
        };
        let resources = vec![resource_with_action("videos", "Videos", "search", "Search")];
        let content =
            Content::new(vec![Box::new(service)], resources).within(Some("library:videos".into()));
        assert!(content.query("lofi").is_empty(), "nothing has arrived yet");
        assert!(content.waiting());
        assert!(!content.poll(), "still coming");
        arrived.set(true);
        assert!(content.poll(), "the answer is in");
        let hits = content.query("lofi");
        assert_eq!(
            hits.first().map(|hit| hit.title.as_str()),
            Some("Search the service for “lofi”")
        );
        assert!(!content.waiting());
        assert!(
            asked
                .borrow()
                .iter()
                .all(|(text, scope)| text == "lofi" && scope.as_deref() == Some("library:videos")),
            "the app is asked within the part of it the search was opened for: {asked:?}"
        );
    }

    /// An app whose hits name no action: it opens them at their anchor
    /// through its `open`.
    struct Library;
    impl ui::search::SearchProvider for Library {
        fn name(&self) -> &'static str {
            "Videos"
        }
        fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
            vec![ui::search::SearchResult::new(
                "Videos",
                format!("Search the service for “{query}”"),
                format!("videos.search:{query}"),
            )]
        }
        fn open_action(&self) -> Option<ActionAddress> {
            Some(ActionAddress::new("videos", "open"))
        }
    }

    #[test]
    fn a_hit_naming_no_action_opens_through_the_apps_open_at_its_anchor() {
        struct Closed;
        impl ui::search::SearchProvider for Closed {
            fn name(&self) -> &'static str {
                "Closed"
            }
            fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
                Library.search(query)
            }
        }
        let mut library = ResourceSpec::new("videos", "videos.library", "Videos");
        // As the framework publishes it: a view taking the anchor.
        library.actions = vec![
            tilo_actions::ActionSpec::typed_operation(
                "open",
                "Open",
                "Item to open",
                tilo_actions::OperationKind::View,
                "test.open",
                vec![tilo_actions::ActionParam::required(
                    "anchor",
                    tilo_actions::ParamType::Text,
                    "Item to open",
                )],
                |_| tilo_actions::EncodedAction {
                    type_id: "test.open".into(),
                    payload: Vec::new(),
                },
            )
            .of_kind(tilo_actions::ActionKind::Read),
        ];
        let content = Content::new(vec![Box::new(Library)], vec![library]);
        let hits = content.query("lofi");
        let Some(Target::Action(target)) = hits.first().map(|hit| &hit.target) else {
            panic!("the hit opens through an action: {hits:?}");
        };
        assert_eq!(hits[0].title, "Search the service for “lofi”");
        assert_eq!(target.address(), ActionAddress::new("videos", "open"));
        assert_eq!(
            target.preset,
            Some(("anchor".to_owned(), "videos.search:lofi".to_owned()))
        );
        // Without an `open`, such a hit is not offered: nothing could open it.
        assert!(
            Content::new(vec![Box::new(Closed)], Vec::new())
                .query("lofi")
                .is_empty()
        );
    }

    #[test]
    fn recents_lead_an_empty_query_newest_first() {
        let mut recents = Recents::default();
        let launch = apps().query("sheets").remove(0);
        let notes = apps().query("notes").remove(0);
        recents.record(&launch);
        recents.record(&notes);
        recents.record(&launch);
        let groups = search("", &[&apps(), &recents], 0);
        assert_eq!(groups[0].kind.label(), RECENT_KIND);
        let titles: Vec<&str> = groups[0].results.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, ["Sheets", "Notes"]);
        assert!(recents.query("sheets").is_empty());
    }

    #[test]
    fn commands_match_their_title() {
        let commands = Commands(vec![CommandSpec {
            id: "desktop.close-window".into(),
            title: "Close window".into(),
            subtitle: Some("The focused window".into()),
        }]);
        assert_eq!(commands.query("close").len(), 1);
        assert!(commands.query("launch").is_empty());
    }

    /// Every app of the manifest registry, as the portable desktop lists them.
    fn registry_apps() -> Applications {
        Applications::new(
            crate::availability::catalog()
                .iter()
                .map(|app| AppEntry {
                    id: app.id.into(),
                    title: app.name.into(),
                    category: (!app.category.is_empty()).then(|| app.category.into()),
                    description: None,
                })
                .collect(),
        )
    }

    fn first_app(apps: &Applications, query: &str) -> Option<String> {
        let groups = search(query, &[apps], 0);
        let hit = groups.first()?.results.first()?;
        Some(hit.id.clone())
    }

    #[test]
    fn registry_apps_are_found_by_the_words_people_use() {
        let apps = registry_apps();
        for (query, id) in [
            ("tube", "so.tilo.tube"),
            ("youtube", "so.tilo.tube"),
            ("Youtube", "so.tilo.tube"),
            ("yotube", "so.tilo.tube"),
            ("mail", "so.tilo.mail"),
            ("email", "so.tilo.mail"),
            ("e-mail", "so.tilo.mail"),
            ("posteingang", "so.tilo.mail"),
            ("preferences", "so.tilo.settings"),
            ("excel", "so.tilo.sheets"),
            ("taschenrechner", "so.tilo.calculator"),
            ("wp", "so.tilo.word-processor"),
        ] {
            assert_eq!(
                first_app(&apps, query).as_deref(),
                Some(id),
                "{query:?} should lead with {id}"
            );
        }
    }

    #[test]
    fn an_app_named_by_a_keyword_leads_a_note_that_merely_has_the_word() {
        struct Notes;
        impl Source for Notes {
            fn id(&self) -> &'static str {
                "notes"
            }
            fn query(&self, needle: &str) -> Vec<Hit> {
                vec![Hit {
                    kind: ResourceKind::Note,
                    provider: "notes".into(),
                    id: "n".into(),
                    title: needle.into(),
                    subtitle: None,
                    facts: Vec::new(),
                    score: MAX_PROVIDER_SCORE,
                    icon: None,
                    target: Target::Window { tab: 1, app: None },
                }]
            }
        }
        let apps = registry_apps();
        let groups = search("youtube", &[&Notes, &apps], 0);
        assert_eq!(groups[0].results[0].id, "so.tilo.tube");
    }

    #[test]
    fn an_empty_query_gives_applications_no_launch_intent() {
        // Launch intent is a reading of what was typed: with nothing typed,
        // the recent rows the person chose lead the app list.
        let apps = apps();
        let mut recents = Recents::default();
        recents.record(&apps.query("notes").remove(0));
        let groups = search("", &[&apps, &recents], 0);
        assert_eq!(groups[0].kind.label(), RECENT_KIND);
    }

    #[test]
    fn a_query_against_every_registry_app_builds_no_searchable_text() {
        let apps = registry_apps();
        let built = crate::rank::FIELD_BUILDS.with(std::cell::Cell::get);
        assert_eq!(built, apps.entries.len(), "each app's text is built once");
        for query in [
            "",
            "youtube",
            "email",
            "einstellungen",
            "setings",
            "wp",
            "rechner neu",
        ] {
            let _ = search(query, &[&apps], 12);
        }
        assert_eq!(
            crate::rank::FIELD_BUILDS.with(std::cell::Cell::get),
            built,
            "a keystroke only searches the text built with the list"
        );
    }

    #[test]
    #[ignore = "measurement; run with --ignored --nocapture"]
    fn a_query_against_every_registry_app_stays_under_two_milliseconds() {
        let apps = registry_apps();
        let _ = apps.query("warm");
        for query in ["youtube", "email", "einstellungen", "setings", "wp"] {
            // The least of the tries: the rest is the machine's other work.
            let best = (0..25)
                .map(|_| {
                    let started = std::time::Instant::now();
                    let _ = search(query, &[&apps], 12);
                    started.elapsed()
                })
                .min()
                .unwrap_or_default();
            println!("{query:?}: best of 25 took {best:?}");
            // 2 ms optimised; the unoptimised test profile runs this code
            // several times slower.
            let budget =
                std::time::Duration::from_millis(if cfg!(debug_assertions) { 10 } else { 2 });
            assert!(
                best < budget,
                "{query:?} took {best:?} against the registry (budget {budget:?})"
            );
        }
    }

    /// The rows `needle` comes to over the applications and the answers,
    /// as `(title, target)` in the order the palette shows them.
    fn answered(needle: &str) -> Vec<(String, Target)> {
        search(
            needle,
            &[&apps(), &Answers::default(), &WebSearch::default()],
            0,
        )
        .into_iter()
        .flat_map(|group| group.results)
        .map(|hit| (hit.title, hit.target))
        .collect()
    }

    #[test]
    fn a_sum_and_a_conversion_lead_and_are_copied() {
        let rows = answered("12*7");
        assert_eq!(
            rows.first(),
            Some(&("84".to_owned(), Target::Copy { text: "84".into() }))
        );
        let rows = answered("5 km in miles");
        assert_eq!(
            rows.first(),
            Some(&(
                "3.1069 mi".to_owned(),
                Target::Copy {
                    text: "3.1069 mi".into()
                }
            ))
        );
        let hit = Answers::default().query("5 km in miles").remove(0);
        assert_eq!(hit.subtitle.as_deref(), Some("5 km ="));
        assert_eq!(hit.id, "unit:5 km in miles");
    }

    #[test]
    fn every_query_can_be_searched_on_the_web_after_what_it_names() {
        let rows = answered("sheets");
        assert_eq!(
            rows.first().map(|(title, _)| title.as_str()),
            Some("Sheets")
        );
        assert_eq!(
            rows.last(),
            Some(&(
                "Search the web for sheets".to_owned(),
                Target::Open {
                    uri: "web+search:sheets".into(),
                    fallback: Some("https://duckduckgo.com/?q=sheets".into()),
                }
            )),
            "the web comes last"
        );
        let rows = answered("youtube cats");
        assert_eq!(
            rows.first(),
            Some(&(
                "Search YouTube for cats".to_owned(),
                Target::Open {
                    uri: "https://www.youtube.com/results?search_query=cats".into(),
                    fallback: None,
                }
            )),
            "a query naming a service searches it first"
        );
        assert!(WebSearch::default().query("   ").is_empty());
        assert!(Answers::default().query("   ").is_empty());
    }

    #[test]
    fn an_address_opens_and_answers_are_not_remembered() {
        let hits = Answers::default().query("example.com");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Open example.com");
        assert_eq!(
            hits[0].target,
            Target::Open {
                uri: "https://example.com".into(),
                fallback: None,
            }
        );
        let mut recents = Recents::default();
        recents.record(&hits[0]);
        recents.record(&Answers::default().query("2+2").remove(0));
        assert!(
            recents.0.is_empty(),
            "what one query came to is not a recent"
        );
    }

    #[test]
    fn money_answers_from_the_kept_rates_and_says_when_they_are_old() {
        let rates = crate::answers::currency::Rates {
            base: "EUR".into(),
            rates: [("USD".to_owned(), 1.25)].into_iter().collect(),
            fetched_at: 1_000_000,
        };
        let fresh = Answers {
            rates: Some(rates.clone()),
            now: 1_000_000,
        };
        let row = fresh.query("10 usd in eur").remove(0);
        assert_eq!(row.title, "8.00 EUR");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("10.00 USD ="),
            "fresh rates say nothing"
        );
        let stale = Answers {
            rates: Some(rates),
            now: 1_000_000 + 3 * 24 * 60 * 60,
        };
        let row = stale.query("10 usd in eur").remove(0);
        assert!(
            row.subtitle
                .as_deref()
                .is_some_and(|line| line.contains("3 days ago")),
            "{:?}",
            row.subtitle
        );
        assert!(
            Answers::default().query("10 usd in eur").is_empty(),
            "no rates, no money"
        );
    }

    #[test]
    fn an_installed_app_serves_its_service_and_says_so() {
        let web = WebSearch {
            app_for_scheme: |scheme| (scheme == "tube").then(|| "Tube".into()),
        };
        let row = web.query("youtube cats").remove(0);
        assert_eq!(row.subtitle.as_deref(), Some("Tube"));
        assert_eq!(
            row.target,
            Target::Open {
                uri: "tube:search?q=cats".into(),
                fallback: Some("https://www.youtube.com/results?search_query=cats".into()),
            }
        );
        let row = WebSearch::default().query("youtube cats").remove(0);
        assert_eq!(
            row.subtitle.as_deref(),
            Some("youtube.com"),
            "no Tube: the web serves"
        );
    }

    #[test]
    fn a_service_named_alone_yields_to_its_app_and_a_search_leads() {
        let web = WebSearch::default();
        let youtube = web.query("youtube").remove(0);
        assert_eq!(youtube.title, "YouTube");
        assert!(web.intent(&youtube) < 0.0);
        let reddit = web.query("reddit").remove(0);
        assert!(
            web.intent(&reddit).abs() < f32::EPSILON,
            "no app serves Reddit"
        );
        let search = web.query("youtube cats").remove(0);
        assert!(web.intent(&search) > 0.0);
        assert!(WebSearch::is_fallback(&web.query("weather").remove(0)));
    }

    #[test]
    fn rows_resolve_from_their_address() {
        let web = WebSearch::default();
        for id in ["search:cats", "service:youtube", "query:youtube:cats"] {
            assert_eq!(web.resolve(id).map(|row| row.id).as_deref(), Some(id));
        }
        assert_eq!(
            Answers::default()
                .resolve("sum:2+2")
                .map(|row| row.title)
                .as_deref(),
            Some("4")
        );
    }
}
