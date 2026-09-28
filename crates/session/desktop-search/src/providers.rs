//! Portable sources: everything a desktop can search without a compositor.
//! Each takes a snapshot the host collects when search opens.

use std::collections::BTreeMap;

use tilo_actions::{ActionAddress, Availability, Resource};

use crate::kind::ResourceKind;
use crate::model::{ActionTarget, Hit, Source, Target};
use crate::rank::{self, MAX_PROVIDER_SCORE};

/// An application the desktop can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub title: String,
    pub category: Option<String>,
    pub description: Option<String>,
    /// Why this desktop cannot open it ("Tilo OS only"); listed, not launched.
    pub unavailable: Option<String>,
}

/// Applications from the manifest registry, offered as `Open <App>`, and
/// those this desktop cannot run, listed with their reason below them.
pub struct Applications(pub Vec<AppEntry>);

impl Source for Applications {
    fn id(&self) -> &'static str {
        "applications"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        self.0
            .iter()
            .filter_map(|app| {
                let matched = rank::application_match_score(
                    needle,
                    &app.title,
                    &app.id,
                    app.category.as_deref(),
                    app.description.as_deref(),
                )?;
                let (section, presented) = rank::empty_palette_presentation(&app.id);
                if let Some(reason) = &app.unavailable {
                    let score = if needle.trim().is_empty() {
                        presented
                    } else {
                        matched
                    } / 4;
                    let subtitle = app.category.as_deref().map_or_else(
                        || reason.clone(),
                        |category| format!("{reason} · {category}"),
                    );
                    return Some(Hit {
                        kind: ResourceKind::Application,
                        provider: self.id().into(),
                        id: app.id.clone(),
                        title: app.title.clone(),
                        subtitle: Some(subtitle),
                        score,
                        target: Target::Unavailable {
                            app: app.id.clone(),
                            reason: reason.clone(),
                        },
                    });
                }
                let (score, subtitle) = if needle.trim().is_empty() {
                    (presented, format!("{section} · {}", app.id))
                } else {
                    let detail = app.category.as_deref().map_or_else(
                        || app.id.clone(),
                        |category| format!("{category} · {}", app.id),
                    );
                    (matched, detail)
                };
                Some(Hit {
                    kind: ResourceKind::Application,
                    provider: self.id().into(),
                    id: app.id.clone(),
                    title: format!("Open {}", app.title),
                    subtitle: Some(subtitle),
                    score,
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
    pub app_title: String,
    pub location: String,
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
                    subtitle: Some(window.location.clone()),
                    score,
                    target: Target::Window { tab: window.tab },
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
                    rank::rank_fields(
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
    !spec.is_programmatic() && matches!(spec.contract.availability, Availability::Available)
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
        tilo_actions::flatten(&self.0)
            .into_iter()
            .filter(|flat| offered(flat.action))
            .filter_map(|flat| {
                let score = rank::action_match_score(
                    needle,
                    &flat.action.label,
                    &flat.resource_label,
                    &flat.action.id,
                    &flat.resource_kind,
                )?;
                Some(Hit {
                    kind: ResourceKind::Action,
                    provider: self.id().into(),
                    id: flat.key.clone(),
                    title: flat.action.label.clone(),
                    subtitle: Some(flat.resource_label.clone()),
                    score,
                    target: Target::Action(Box::new(ActionTarget {
                        resource: flat.resource_id.clone(),
                        resource_kind: flat.resource_kind.clone(),
                        resource_label: flat.resource_label.clone(),
                        spec: flat.action.clone(),
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
pub struct Content {
    providers: Vec<Box<dyn ui::search::SearchProvider>>,
    resources: Vec<Resource>,
    limit: usize,
}

impl Content {
    pub fn new(
        providers: Vec<Box<dyn ui::search::SearchProvider>>,
        resources: Vec<Resource>,
    ) -> Self {
        Self {
            providers,
            resources,
            limit: 16,
        }
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
        let mut hits = Vec::new();
        for provider in &self.providers {
            for result in provider.search_limited(needle, self.limit) {
                let Some(address) = result.action_address() else {
                    continue;
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
                let Some(target) = action_target(resource, &address.action) else {
                    continue;
                };
                let score = rank::record_match_score(needle, &result.title, &address.resource)
                    .unwrap_or(CONTENT_MATCH_SCORE)
                    .min(MAX_PROVIDER_SCORE);
                hits.push(Hit {
                    kind: content_kind(&result.kind),
                    provider: self.id().into(),
                    id: key(&address),
                    title: result.title.clone(),
                    subtitle: result.subtitle.clone(),
                    score,
                    target: Target::Action(Box::new(target)),
                });
            }
        }
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
        Applications(vec![
            AppEntry {
                id: "so.tilo.sheets".into(),
                title: "Sheets".into(),
                category: Some("Office".into()),
                description: None,
                unavailable: None,
            },
            AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notes".into(),
                category: Some("Office".into()),
                description: None,
                unavailable: None,
            },
        ])
    }

    #[test]
    fn an_app_this_desktop_cannot_run_is_listed_with_its_reason_below_those_it_can() {
        let mut entries = apps().0;
        entries.push(AppEntry {
            id: "so.tilo.sheets-server".into(),
            title: "Sheets Server".into(),
            category: Some("System".into()),
            description: None,
            unavailable: Some("Tilo OS only".into()),
        });
        let hits = Applications(entries).query("sheets");
        let server = hits
            .iter()
            .find(|hit| hit.id == "so.tilo.sheets-server")
            .expect("the unavailable app is still found");
        assert_eq!(
            server.title, "Sheets Server",
            "no \"Open\" for an app that cannot open"
        );
        assert_eq!(server.subtitle.as_deref(), Some("Tilo OS only · System"));
        assert_eq!(
            server.target,
            Target::Unavailable {
                app: "so.tilo.sheets-server".into(),
                reason: "Tilo OS only".into()
            }
        );
        let sheets = hits.iter().find(|hit| hit.id == "so.tilo.sheets").unwrap();
        assert!(sheets.score > server.score);
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
            location: "Workspace · Main".into(),
        }]);
        let groups = search("sheets", &[&apps(), &windows], 10);
        let first = &groups[0].results[0];
        assert_eq!(first.title, "Open Sheets");
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
            location: "Workspace".into(),
        }]);
        let groups = search("", &[&apps(), &windows, &actions], 0);
        let kinds: Vec<&str> = groups.iter().map(|g| g.kind.label()).collect();
        assert!(kinds.contains(&"Applications"));
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

    #[test]
    fn unavailable_and_programmatic_actions_are_not_offered() {
        let mut resource = resource_with_action("r", "Doc", "print", "Print");
        resource.actions[0].contract.availability = Availability::Unavailable {
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
        assert_eq!(titles, ["Open Sheets", "Open Notes"]);
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
}
