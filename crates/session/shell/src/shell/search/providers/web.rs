//! The web as a search provider: searching the web for what was typed, and
//! the web services people search by name.
//!
//! - **"Search the web for …"** answers every query and is always the last
//!   row ([`SearchProvider::is_fallback`]). Enter hands the query to the
//!   browser as `web+search:`, and the browser runs it on the search engine
//!   the person chose in its preferences: the desktop never needs to know
//!   the engine. With no browser that takes the scheme, the query opens on
//!   [`FALLBACK_ENGINE`] in whatever opens web addresses.
//! - **Known services** ([`SERVICES`], one table) answer a query that starts
//!   with their name or a keyword: "youtube cats" offers "Search `YouTube` for
//!   cats", which ranks like an exact name, so among the first rows. A
//!   service a Tilo app serves (Tube for `YouTube`) searches in that app when
//!   it is installed, and on the web otherwise. The service named alone
//!   ("youtube") offers the service row, which opens its site; a service
//!   with an app of its own yields to that app's row.
//!
//! Nothing typed leaves the machine before a person chooses a row: these
//! rows are built from the table and the query alone.

use std::fmt::Write as _;

use ui::icons::Icon;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::providers::quick_answers::{open_uri, uri_refusal};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::uri_open::{self, UriOpenError};

pub const PROVIDER_ID: &str = "web";

/// "Search the web for …": `search:<query>`.
const SEARCH_ID: &str = "search";
/// A service named alone: `service:<service>`.
const SERVICE_ID: &str = "service";
/// A service searched for something: `query:<service>:<query>`.
const QUERY_ID: &str = "query";

/// The scheme the browser takes a web search under; it runs the query on
/// the person's own search engine (`apps/browser`, `launch_search`).
const WEB_SEARCH_SCHEME: &str = "web+search";

/// Where a web search goes when no browser takes [`WEB_SEARCH_SCHEME`]: the
/// browser's own default engine, one that does not profile the person
/// searching.
const FALLBACK_ENGINE: &str = "https://duckduckgo.com/?q=";

/// The placeholder a search template's query goes in.
const QUERY: &str = "{query}";

/// A service row's score when the query names the service alone: below an
/// exact name's, so an app named the same way still leads.
const SERVICE_SCORE: i32 = 60;

/// What a service row gives way at the merge when an app serves the same
/// service (Tube for `YouTube`, Spotify on Flathub): enough that the app's row,
/// installed or installable, leads the service named alone.
const SERVED_SERVICE_YIELD: f32 = -1.5;

/// The shortest typed name prefix that offers a service ("you" for
/// `YouTube`); a shorter one is too many things.
const MIN_PREFIX_SCALARS: usize = 3;

/// An app that serves a known service on this desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServingApp {
    /// A Tilo app, and the URI that opens it on a search (`{query}` stands
    /// for the percent-encoded query) when it can search. The URI's scheme
    /// is what the app claims, so whichever installed app claims it serves.
    Tilo {
        package_id: &'static str,
        search: Option<&'static str>,
    },
    /// An app on Flathub. A query still goes to the web: the app takes no
    /// search from outside.
    Flathub { app_id: &'static str },
}

/// A web service people search by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    /// Stable id, part of the row's address.
    pub id: &'static str,
    /// Its name as people write it.
    pub name: &'static str,
    /// The other words that name it, lowercase. The name is always one.
    pub keywords: &'static [&'static str],
    pub icon: Icon,
    /// The site, as the row's second line shows it.
    pub domain: &'static str,
    /// The page the service row opens.
    pub home: &'static str,
    /// The search URL, `{query}` standing for the percent-encoded query.
    pub search: &'static str,
    /// The app that serves it, if one does.
    pub app: Option<ServingApp>,
}

/// The web services search knows. Adding one is adding a row.
pub const SERVICES: &[Service] = &[
    Service {
        id: "youtube",
        name: "YouTube",
        keywords: &["yt"],
        icon: Icon::Play,
        domain: "youtube.com",
        home: "https://www.youtube.com/",
        search: "https://www.youtube.com/results?search_query={query}",
        app: Some(ServingApp::Tilo {
            package_id: "so.tilo.tube",
            search: Some("tube:search?q={query}"),
        }),
    },
    Service {
        id: "spotify",
        name: "Spotify",
        keywords: &[],
        icon: Icon::Music,
        domain: "open.spotify.com",
        home: "https://open.spotify.com/",
        search: "https://open.spotify.com/search/{query}",
        app: Some(ServingApp::Flathub {
            app_id: "com.spotify.Client",
        }),
    },
    Service {
        id: "wikipedia",
        name: "Wikipedia",
        keywords: &["wiki"],
        icon: Icon::Globe,
        domain: "wikipedia.org",
        home: "https://www.wikipedia.org/",
        search: "https://www.wikipedia.org/search-redirect.php?search={query}",
        app: None,
    },
    Service {
        id: "maps",
        name: "Maps",
        keywords: &["google maps", "openstreetmap", "osm"],
        icon: Icon::MapPin,
        domain: "openstreetmap.org",
        home: "https://www.openstreetmap.org/",
        search: "https://www.openstreetmap.org/search?query={query}",
        app: Some(ServingApp::Tilo {
            package_id: "so.tilo.maps",
            search: Some("geo:0,0?q={query}"),
        }),
    },
    Service {
        id: "github",
        name: "GitHub",
        keywords: &["gh"],
        icon: Icon::Code,
        domain: "github.com",
        home: "https://github.com/",
        search: "https://github.com/search?q={query}",
        app: None,
    },
    Service {
        id: "reddit",
        name: "Reddit",
        keywords: &[],
        icon: Icon::MessageSquare,
        domain: "reddit.com",
        home: "https://www.reddit.com/",
        search: "https://www.reddit.com/search/?q={query}",
        app: None,
    },
    Service {
        id: "amazon",
        name: "Amazon",
        keywords: &[],
        icon: Icon::ShoppingBag,
        domain: "amazon.com",
        home: "https://www.amazon.com/",
        search: "https://www.amazon.com/s?k={query}",
        app: None,
    },
    Service {
        id: "netflix",
        name: "Netflix",
        keywords: &[],
        icon: Icon::Film,
        domain: "netflix.com",
        home: "https://www.netflix.com/",
        search: "https://www.netflix.com/search?q={query}",
        app: None,
    },
];

impl Service {
    fn by_id(id: &str) -> Option<&'static Self> {
        SERVICES.iter().find(|service| service.id == id)
    }

    /// Every word that names the service, lowercase: its name first.
    fn names(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.name.to_lowercase())
            .chain(self.keywords.iter().map(|word| word.to_lowercase()))
    }

    /// The URI that searches the serving Tilo app, and the scheme it claims.
    fn app_search(&self, query: &str) -> Option<(String, &'static str)> {
        let Some(ServingApp::Tilo {
            search: Some(template),
            ..
        }) = self.app
        else {
            return None;
        };
        let (scheme, _) = template.split_once(':')?;
        Some((template.replace(QUERY, &percent_encode(query)), scheme))
    }

    fn web_search(&self, query: &str) -> String {
        self.search.replace(QUERY, &percent_encode(query))
    }
}

/// What a query asks of the service table.
#[derive(Debug, PartialEq, Eq)]
enum Named {
    /// The service by its name or a keyword, or the start of its name.
    Service(&'static Service),
    /// The service's name or a keyword, then what to search it for.
    Search(&'static Service, String),
}

/// What `typed` names: a service searched for something ("youtube cats",
/// the longest naming word winning: "google maps berlin"), or a service
/// alone ("youtube", "yout"). `None` for anything else.
fn named(typed: &str) -> Option<Named> {
    let typed = typed.trim();
    let lower = typed.to_lowercase();
    let mut search: Option<(usize, &'static Service, String)> = None;
    for service in SERVICES {
        for word in service.names() {
            if lower == word {
                return Some(Named::Service(service));
            }
            // Lowercasing keeps byte offsets for the names in the table,
            // which are ASCII; `get` refuses any other split.
            let Some(rest) = lower
                .strip_prefix(word.as_str())
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .and_then(|_| typed.get(word.len()..))
            else {
                continue;
            };
            let rest = rest.trim();
            if !rest.is_empty() && search.as_ref().is_none_or(|(len, ..)| word.len() > *len) {
                search = Some((word.len(), service, rest.to_owned()));
            }
        }
    }
    if let Some((_, service, query)) = search {
        return Some(Named::Search(service, query));
    }
    if lower.chars().count() < MIN_PREFIX_SCALARS {
        return None;
    }
    SERVICES
        .iter()
        .find(|service| service.name.to_lowercase().starts_with(&lower))
        .map(Named::Service)
}

/// The installed app that claims `scheme`, by its name; `None` when none
/// does. Read from the launcher's last enumeration, never the registry, so
/// a keystroke does no disk work.
fn installed_app_for_scheme(scheme: &str) -> Option<String> {
    crate::shell::launcher::installed_apps_without_waiting()
        .into_iter()
        .find(|app| app.opens_uri_schemes.iter().any(|claim| claim == scheme))
        .map(|app| app.display_name)
}

pub struct WebProvider {
    /// `[web search, services]`.
    kinds: Vec<ResourceKind>,
    /// The installed app that claims a scheme, by name.
    app_for_scheme: fn(&str) -> Option<String>,
}

impl Default for WebProvider {
    fn default() -> Self {
        Self {
            kinds: vec![
                ResourceKind::Custom(t("search-kind-web")),
                ResourceKind::Custom(t("search-kind-websites")),
            ],
            app_for_scheme: installed_app_for_scheme,
        }
    }
}

impl WebProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider that sees the installed apps `app_for_scheme` reports.
    #[cfg(test)]
    pub(crate) fn with_installed(app_for_scheme: fn(&str) -> Option<String>) -> Self {
        Self {
            app_for_scheme,
            ..Self::default()
        }
    }

    fn web_kind(&self) -> ResourceKind {
        self.kinds
            .first()
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-web")))
    }

    fn service_kind(&self) -> ResourceKind {
        self.kinds
            .get(1)
            .cloned()
            .unwrap_or_else(|| ResourceKind::Custom(t("search-kind-websites")))
    }

    /// "Search the web for `query`".
    fn web_search_row(&self, query: &str) -> SearchResult {
        let typed = query.to_owned();
        row(
            self.web_kind(),
            format!("{SEARCH_ID}:{query}"),
            t_args("search-web-for", &[("query", query.into())]),
            Some(t("search-web-subtitle")),
            1,
            move |chrome| search_the_web(&typed, chrome),
        )
    }

    /// The service named alone: opens its site.
    fn service_row(&self, service: &'static Service) -> SearchResult {
        row(
            self.service_kind(),
            format!("{SERVICE_ID}:{}", service.id),
            service.name.to_owned(),
            Some(service.domain.to_owned()),
            SERVICE_SCORE,
            move |chrome| open_uri(service.home, chrome),
        )
    }

    /// "Search `service` for `query`": in the app that serves it when one is
    /// installed, on the web otherwise.
    fn service_search_row(&self, service: &'static Service, query: &str) -> SearchResult {
        let in_app = service
            .app_search(query)
            .and_then(|(uri, scheme)| Some((uri, (self.app_for_scheme)(scheme)?)));
        let web = service.web_search(query);
        let (subtitle, open): (String, OpenRow) = match in_app {
            Some((uri, app)) => (
                app,
                Box::new(move |chrome: &Chrome| {
                    match uri_open::open_in_workspace(&uri, chrome) {
                        // Removed since the row was built: the web serves.
                        Err(UriOpenError::NoHandler { .. }) => open_uri(&web, chrome),
                        result => result.map(drop).map_err(uri_refusal),
                    }
                }),
            ),
            None => (
                service.domain.to_owned(),
                Box::new(move |chrome: &Chrome| open_uri(&web, chrome)),
            ),
        };
        row(
            self.service_kind(),
            format!("{QUERY_ID}:{}:{query}", service.id),
            t_args(
                "search-web-service-for",
                &[("service", service.name.into()), ("query", query.into())],
            ),
            Some(subtitle),
            self.max_score(),
            open,
        )
    }

    fn rows(&self, typed: &str, wanted: &[ResourceKind]) -> Vec<SearchResult> {
        let typed = typed.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if wanted.contains(&self.service_kind()) {
            match named(typed) {
                Some(Named::Search(service, query)) => {
                    out.push(self.service_search_row(service, &query));
                }
                Some(Named::Service(service)) => out.push(self.service_row(service)),
                None => {}
            }
        }
        if wanted.contains(&self.web_kind()) {
            out.push(self.web_search_row(typed));
        }
        out
    }
}

fn row(
    kind: ResourceKind,
    id: String,
    title: String,
    subtitle: Option<String>,
    score: i32,
    open: impl Fn(&Chrome) -> Result<(), ActionError> + 'static,
) -> SearchResult {
    let mut row = SearchResult::nullary_fallible(
        kind,
        PROVIDER_ID,
        id,
        title,
        subtitle,
        score,
        move |_doc, _root, chrome| open(chrome),
    );
    row.contract = Some(tilo_actions::ActionContract::view());
    row
}

/// Hand `query` to the browser, which searches on the person's engine; with
/// no browser that takes a web search, open it on [`FALLBACK_ENGINE`].
fn search_the_web(query: &str, chrome: &Chrome) -> Result<(), ActionError> {
    let encoded = percent_encode(query);
    match uri_open::open_in_workspace(&format!("{WEB_SEARCH_SCHEME}:{encoded}"), chrome) {
        Err(UriOpenError::NoHandler { .. }) => {
            open_uri(&format!("{FALLBACK_ENGINE}{encoded}"), chrome)
        }
        result => result.map(drop).map_err(uri_refusal),
    }
}

/// `text` percent-encoded for a URL's path or query: unreserved bytes kept,
/// every other byte (a space too) as `%XX`.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

impl SearchProvider for WebProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Web"
    }

    fn display_name(&self) -> String {
        t("search-provider-web")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    /// Which app serves a service changes as apps come and go.
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        format!("web:{}", crate::shell::launcher::installed_apps_epoch())
    }

    fn is_fallback(&self, result: &SearchResult) -> bool {
        result
            .id
            .split_once(':')
            .is_some_and(|(row, _)| row == SEARCH_ID)
    }

    fn intent_strength(&self, result: &SearchResult, _needle: &str) -> f32 {
        let Some((row, rest)) = result.id.split_once(':') else {
            return 0.0;
        };
        match row {
            // A query naming a service is worth what the row's title typed
            // whole is: the query named the service as plainly as a title.
            QUERY_ID => ui::search::system::lexical_strength(
                &result.title,
                &result.kind,
                &result.title,
                None,
            ),
            SERVICE_ID if Service::by_id(rest).is_some_and(|service| service.app.is_some()) => {
                SERVED_SERVICE_YIELD
            }
            _ => 0.0,
        }
    }

    fn row_icon(&self, result: &SearchResult) -> Option<Icon> {
        let (row, rest) = result.id.split_once(':')?;
        match row {
            SEARCH_ID => Some(Icon::Globe),
            SERVICE_ID => Service::by_id(rest).map(|service| service.icon),
            QUERY_ID => {
                let (service, _) = rest.split_once(':')?;
                Service::by_id(service).map(|service| service.icon)
            }
            _ => None,
        }
    }

    /// A row's address carries what it answers (`search:cats`,
    /// `query:youtube:cats`), so it resolves without the query that found it.
    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        let (row, rest) = id.split_once(':')?;
        let found = match row {
            SEARCH_ID if !rest.trim().is_empty() => self.web_search_row(rest),
            SERVICE_ID => self.service_row(Service::by_id(rest)?),
            QUERY_ID => {
                let (service, query) = rest.split_once(':')?;
                self.service_search_row(Service::by_id(service)?, query)
            }
            _ => return None,
        };
        (found.id == id).then_some(found)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.rows(ctx.needle, ctx.wanted)
    }
}

/// What choosing a row does.
type OpenRow = Box<dyn Fn(&Chrome) -> Result<(), ActionError>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds() -> Vec<ResourceKind> {
        WebProvider::with_installed(|_| None).kinds
    }

    #[test]
    fn a_service_is_named_by_its_name_a_keyword_or_its_start() {
        let youtube = Service::by_id("youtube").expect("YouTube is known");
        let maps = Service::by_id("maps").expect("Maps is known");
        assert_eq!(named("youtube"), Some(Named::Service(youtube)));
        assert_eq!(named("YouTube"), Some(Named::Service(youtube)));
        assert_eq!(named("yout"), Some(Named::Service(youtube)));
        assert_eq!(
            named("youtube cats"),
            Some(Named::Search(youtube, "cats".into()))
        );
        assert_eq!(
            named("YT Lo-Fi Mix"),
            Some(Named::Search(youtube, "Lo-Fi Mix".into())),
            "the query keeps how it was typed"
        );
        assert_eq!(
            named("google maps berlin"),
            Some(Named::Search(maps, "berlin".into())),
            "the longest naming word wins"
        );
        assert_eq!(named("yo"), None, "too short to name one thing");
        assert_eq!(named("youtubers"), None);
        assert_eq!(named("weather"), None);
    }

    #[test]
    fn every_query_offers_the_web_and_a_named_service_its_search() {
        let provider = WebProvider::with_installed(|_| None);
        let wanted = kinds();
        let rows = provider.rows("weather", &wanted);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Search the web for weather");
        assert_eq!(rows[0].subtitle.as_deref(), Some("Web"));
        assert!(provider.is_fallback(&rows[0]));

        let rows = provider.rows("youtube cats", &wanted);
        assert_eq!(rows[0].title, "Search YouTube for cats");
        assert_eq!(
            rows[0].subtitle.as_deref(),
            Some("youtube.com"),
            "no Tube installed: the web serves"
        );
        assert!(!provider.is_fallback(&rows[0]));
        assert_eq!(rows[1].title, "Search the web for youtube cats");
        assert!(provider.rows("   ", &wanted).is_empty());
    }

    #[test]
    fn an_installed_app_serves_its_service_and_says_so() {
        let provider =
            WebProvider::with_installed(|scheme| (scheme == "tube").then(|| "Tube".into()));
        let rows = provider.rows("youtube cats", &kinds());
        assert_eq!(rows[0].subtitle.as_deref(), Some("Tube"));
        assert_eq!(
            Service::by_id("youtube")
                .and_then(|service| service.app_search("cats & dogs"))
                .map(|(uri, _)| uri)
                .as_deref(),
            Some("tube:search?q=cats%20%26%20dogs")
        );
    }

    #[test]
    fn a_service_named_alone_yields_to_its_app_and_a_search_leads() {
        let provider = WebProvider::with_installed(|_| None);
        let wanted = kinds();
        let youtube = &provider.rows("youtube", &wanted)[0];
        assert_eq!(youtube.title, "YouTube");
        assert_eq!(youtube.subtitle.as_deref(), Some("youtube.com"));
        assert!(provider.intent_strength(youtube, "youtube") < 0.0);
        let reddit = &provider.rows("reddit", &wanted)[0];
        assert!(
            provider.intent_strength(reddit, "reddit").abs() < f32::EPSILON,
            "no app serves Reddit"
        );
        let search = &provider.rows("youtube cats", &wanted)[0];
        assert!(provider.intent_strength(search, "youtube cats") > 0.0);
    }

    #[test]
    fn rows_resolve_from_their_address() {
        let provider = WebProvider::with_installed(|_| None);
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::bootstrap_default(),
        );
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let wanted = kinds();
        for id in ["search:cats", "service:youtube", "query:youtube:cats"] {
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &wanted,
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            assert_eq!(
                provider.resolve(id, ctx).map(|row| row.id).as_deref(),
                Some(id)
            );
        }
    }

    #[test]
    fn a_query_is_encoded_whole() {
        assert_eq!(percent_encode("wetter köln"), "wetter%20k%C3%B6ln");
        assert_eq!(percent_encode("a&b=c/d"), "a%26b%3Dc%2Fd");
    }
}
