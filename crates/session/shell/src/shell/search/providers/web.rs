//! The web as a search provider: searching the web for what was typed, and
//! the web services people search by name.
//!
//! - **"Search the web for …"** answers every query
//!   ([`SearchProvider::is_fallback`]): after a direct match, and after
//!   asking an agent while one can take the query. Choosing it hands the
//!   query to the browser as `web+search:`, and the browser runs it on the
//!   search engine the person chose in its preferences: the desktop never
//!   needs to know the engine. With no browser that takes the scheme, the
//!   query opens on [`FALLBACK_ENGINE`] in whatever opens web addresses.
//! - **Known services** ([`SERVICES`], one table) answer a query that
//!   starts with their name or a keyword: "youtube cats" offers "Search
//!   `YouTube` for cats", which ranks like an exact name, so among the first
//!   rows. A
//!   service a Tilo app serves (Tube for `YouTube`) searches in that app when
//!   it is installed, and on the web otherwise. The service named alone
//!   ("youtube") offers the service row, which opens its site; a service
//!   with an app of its own yields to that app's row.
//!
//! Nothing typed leaves the machine before a person chooses a row: these
//! rows are built from the table and the query alone. Both are
//! [`tilo_desktop_search::answers::web`]'s, which the web desktop's search
//! offers too; this provider words and runs the rows.
//!
//! [`FALLBACK_ENGINE`]: tilo_desktop_search::answers::web::FALLBACK_ENGINE
//! [`SERVICES`]: tilo_desktop_search::answers::web::SERVICES

use tilo_desktop_search::answers::web::{
    Named, Service, fallback_search_url, named, web_search_uri,
};
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

/// A service row's score when the query names the service alone: below an
/// exact name's, so an app named the same way still leads.
const SERVICE_SCORE: i32 = 60;

/// What a service row gives way at the merge when an app serves the same
/// service (Tube for `YouTube`, Spotify on Flathub): enough that the app's row,
/// installed or installable, leads the service named alone.
const SERVED_SERVICE_YIELD: f32 = -1.5;

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
    match uri_open::open_in_workspace(&web_search_uri(query), chrome) {
        Err(UriOpenError::NoHandler { .. }) => open_uri(&fallback_search_url(query), chrome),
        result => result.map(drop).map_err(uri_refusal),
    }
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
}
