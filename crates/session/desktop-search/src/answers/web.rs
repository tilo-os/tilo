//! The web searches a query can be, worked out from the query and one
//! table alone: "Search the web for …", and the web services people search
//! by name ("youtube cats" searches `YouTube` for cats, "youtube" opens it).
//! The native shell's web provider and the portable palette's
//! [`crate::providers::WebSearch`] both read it.

use std::fmt::Write as _;

use ui::icons::Icon;

/// The scheme the browser takes a web search under; it runs the query on
/// the person's own search engine (`apps/browser`, `launch_search`).
pub const WEB_SEARCH_SCHEME: &str = "web+search";

/// Where a web search goes when no browser takes [`WEB_SEARCH_SCHEME`]: the
/// browser's own default engine, one that does not profile the person
/// searching. A desktop inside a web browser has no `web+search:` handler
/// of its own and opens searches here, in a new browser tab.
pub const FALLBACK_ENGINE: &str = "https://duckduckgo.com/?q=";

/// The placeholder a search template's query goes in.
const QUERY: &str = "{query}";

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
    pub fn by_id(id: &str) -> Option<&'static Self> {
        SERVICES.iter().find(|service| service.id == id)
    }

    /// Every word that names the service, lowercase: its name first.
    pub fn names(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.name.to_lowercase())
            .chain(self.keywords.iter().map(|word| word.to_lowercase()))
    }

    /// The URI that searches the serving Tilo app, and the scheme it claims.
    pub fn app_search(&self, query: &str) -> Option<(String, &'static str)> {
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

    pub fn web_search(&self, query: &str) -> String {
        self.search.replace(QUERY, &percent_encode(query))
    }
}

/// What a query asks of the service table.
#[derive(Debug, PartialEq, Eq)]
pub enum Named {
    Service(&'static Service),
    /// The service's name or a keyword, then what to search it for.
    Search(&'static Service, String),
}

/// What `typed` names: a service searched for something ("youtube cats",
/// the longest naming word winning: "google maps berlin"), or a service
/// alone ("youtube", "yout"). `None` for anything else.
pub fn named(typed: &str) -> Option<Named> {
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

/// `text` percent-encoded for a URL's path or query: unreserved bytes kept,
/// every other byte (a space too) as `%XX`.
pub fn percent_encode(text: &str) -> String {
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

/// `query` handed to the browser as a web search on the person's engine.
pub fn web_search_uri(query: &str) -> String {
    format!("{WEB_SEARCH_SCHEME}:{}", percent_encode(query))
}

/// `query` searched on [`FALLBACK_ENGINE`].
pub fn fallback_search_url(query: &str) -> String {
    format!("{FALLBACK_ENGINE}{}", percent_encode(query))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_serving_app_is_searched_by_its_own_uri() {
        assert_eq!(
            Service::by_id("youtube")
                .and_then(|service| service.app_search("cats & dogs"))
                .map(|(uri, _)| uri)
                .as_deref(),
            Some("tube:search?q=cats%20%26%20dogs")
        );
    }

    #[test]
    fn a_query_is_encoded_whole() {
        assert_eq!(percent_encode("wetter köln"), "wetter%20k%C3%B6ln");
        assert_eq!(percent_encode("a&b=c/d"), "a%26b%3Dc%2Fd");
        assert_eq!(web_search_uri("a b"), "web+search:a%20b");
        assert_eq!(
            fallback_search_url("a b"),
            "https://duckduckgo.com/?q=a%20b"
        );
    }
}
