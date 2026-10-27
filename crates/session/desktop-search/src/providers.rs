//! Portable sources: everything a desktop can search without a compositor.
//! Each takes a snapshot the host collects when search opens.

use std::cell::RefCell;
use std::collections::BTreeMap;

use tilo_actions::{ActionAddress, Availability, Resource};
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
                    target: Target::Command {
                        id: command.id.clone(),
                    },
                })
            })
            .collect()
    }
}

/// Every resource of `resources`, nested ones included, by id.
fn index(resources: &[Resource]) -> BTreeMap<String, &Resource> {
    fn visit<'a>(resource: &'a Resource, out: &mut BTreeMap<String, &'a Resource>) {
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

fn action_target(resource: &Resource, action: &str) -> Option<ActionTarget> {
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
pub struct Actions(pub Vec<Resource>);

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
            .filter(|flat| offered(flat.action))
            .filter_map(|flat| {
                // An open record's actions answer to what they do. An item's
                // (one message of a chat, one card of a board) answer only to
                // the item named: "edit message" must not list the same edit
                // once for every message there is.
                let score = if records.contains(flat.resource_id.as_str()) {
                    rank::action_match_score(needle, &flat.action.label, &flat.resource_label)?
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
    resources: Vec<Resource>,
    /// The part of the application a search it opened is narrowed to (a
    /// [`ui::search::ScopeDecl`] anchor).
    scope: Option<String>,
    limit: usize,
    /// The answers still coming, by provider.
    pending: RefCell<Vec<(usize, PendingToken)>>,
}

impl Content {
    pub fn new(
        providers: Vec<Box<dyn ui::search::SearchProvider>>,
        resources: Vec<Resource>,
    ) -> Self {
        Self {
            providers,
            resources,
            scope: None,
            limit: 16,
            pending: RefCell::default(),
        }
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
        if needle.trim().is_empty() {
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

pub const RECENT_KIND: &str = "Recent";

/// What this palette opened lately, newest first, offered while the query
/// is empty.
#[derive(Default)]
pub struct Recents(pub Vec<Hit>);

/// How many recent choices a palette remembers.
pub const MAX_RECENTS: usize = 6;

impl Recents {
    /// Remember `hit` as the newest choice.
    pub fn record(&mut self, hit: &Hit) {
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

    fn resource_with_action(id: &str, label: &str, action: &str, action_label: &str) -> Resource {
        let mut resource = Resource::new(id, "notes.page", label);
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
    fn unavailable_and_programmatic_actions_are_not_offered() {
        let mut resource = resource_with_action("r", "Doc", "print", "Print");
        resource.actions[0].type_mut().contract.availability = Availability::Unavailable {
            code: "offline".into(),
            reason: "Offline".into(),
        };
        assert!(Actions(vec![resource]).query("print").is_empty());
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
        let mut library = Resource::new("videos", "videos.library", "Videos");
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
}
