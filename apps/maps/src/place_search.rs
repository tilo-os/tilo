//! Maps in the platform search.
//!
//! Search belongs to the platform: Maps draws no search field. The palette
//! (Ctrl+K, `/`, the panel's magnifier) asks [`PlaceSearchProvider`], which
//! answers at once from what the map already holds — saved places, the
//! places on the map, the points of interest around the view, recent
//! searches — and from the geocoder as a slow page that arrives when the
//! background lookup lands ([`PlaceSearch`]).
//!
//! A hit is anchored at the row it names ([`PlaceHit`]) — the place, the
//! point of interest or the recent search — and the window's opener runs the
//! effect that opens it ([`PlaceHit::opened`]). Saved
//! places sit under the `maps-saved` anchor, so the palette can narrow a
//! search to them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::icons::Icon;
use ui::search::{
    PagePoll, PendingToken, ScopeDecl, SearchCursor, SearchPage, SearchProvider, SearchQuery,
    SearchResult,
};

use crate::geo;
use crate::model::{Category, LatLon, Place};
use crate::resource::MapsEffect;

/// The anchor saved places sit under: the "Saved places" search scope.
pub const SAVED_SCOPE: &str = "maps-saved";
const PLACE_ROW: &str = "maps.place:";
const POI_ROW: &str = "maps.poi:";
const HISTORY_ROW: &str = "maps.search-history:";

/// Live answers kept for queries the palette may still poll.
const MAX_ANSWERS: usize = 16;
/// Most local hits one query materialises.
const MAX_LOCAL: usize = 12;

/// What activating a hit does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceHit {
    /// Open a place the map knows (saved or on the map) by id.
    Place(String),
    /// Open a point of interest around the view by id.
    Poi(String),
    /// Open a place the geocoder found, by id.
    Geocoded(String),
    /// Run a recent search again.
    Recent(String),
}

impl PlaceHit {
    /// The published row the hit opens.
    pub fn resource_id(&self) -> String {
        match self {
            Self::Place(id) | Self::Geocoded(id) => format!("{PLACE_ROW}{id}"),
            Self::Poi(id) => format!("{POI_ROW}{id}"),
            Self::Recent(query) => format!("{HISTORY_ROW}{query}"),
        }
    }

    /// The effect activating the hit runs: a found place opens like one
    /// on the map, once the app has taken it in.
    pub fn effect(&self) -> MapsEffect {
        match self {
            Self::Place(id) | Self::Geocoded(id) => MapsEffect::OpenPlace { place: id.clone() },
            Self::Poi(id) => MapsEffect::OpenPoi { poi: id.clone() },
            Self::Recent(query) => MapsEffect::Search {
                query: query.clone(),
            },
        }
    }

    /// The anchor the hit opens at: the row it names.
    pub fn anchor(&self) -> String {
        self.resource_id()
    }

    /// What opening the anchor `anchor` runs: the effect of the row it
    /// names.
    pub fn opened(anchor: &str) -> Option<MapsEffect> {
        let hit = if let Some(id) = anchor.strip_prefix(PLACE_ROW) {
            Self::Place(id.to_owned())
        } else if let Some(id) = anchor.strip_prefix(POI_ROW) {
            Self::Poi(id.to_owned())
        } else {
            Self::Recent(anchor.strip_prefix(HISTORY_ROW)?.to_owned())
        };
        Some(hit.effect())
    }
}

/// What the map holds when the palette opens: the places and points of
/// interest a query matches at once, the recent searches an empty query
/// lists, and the point distances are measured from.
#[derive(Clone, Debug)]
pub struct LocalPlaces {
    pub places: Vec<Place>,
    pub pois: Vec<Place>,
    pub recents: Vec<String>,
    pub anchor: LatLon,
}

#[derive(Debug, Default)]
struct Exchange {
    /// Whether the geocoder can be reached at all (a capture or a test never
    /// opens a socket).
    reachable: bool,
    next_token: u64,
    /// Queries the palette asked for that the app has not sent yet.
    asked: Vec<String>,
    /// Tokens still awaited, with their query.
    waiting: BTreeMap<u64, String>,
    /// Tokens whose query has been answered (or superseded), until the
    /// palette polls them.
    resolved: BTreeMap<u64, String>,
    /// Places the geocoder answered, by query, newest last.
    answers: Vec<(String, Vec<Place>)>,
}

impl Exchange {
    fn answer_for(&self, query: &str) -> Option<&[Place]> {
        self.answers
            .iter()
            .rev()
            .find(|(asked, _)| asked.eq_ignore_ascii_case(query))
            .map(|(_, places)| places.as_slice())
    }
}

/// The app's side of the exchange with the provider.
#[derive(Clone, Debug, Default)]
pub struct PlaceSearch {
    exchange: Rc<RefCell<Exchange>>,
}

impl PlaceSearch {
    /// Allow (or forbid) queries to reach the geocoder.
    pub fn set_reachable(&self, reachable: bool) {
        self.exchange.borrow_mut().reachable = reachable;
    }

    /// The provider the platform search queries, over what the map holds now.
    pub fn provider(&self, local: LocalPlaces) -> PlaceSearchProvider {
        PlaceSearchProvider {
            exchange: Rc::clone(&self.exchange),
            local: Rc::new(local),
        }
    }

    /// The newest query the palette asked for since the last call. Older
    /// queries are superseded: their tokens finish with nothing.
    pub fn take_query(&self) -> Option<String> {
        let mut exchange = self.exchange.borrow_mut();
        let latest = exchange.asked.pop()?;
        exchange.asked.clear();
        let stale: Vec<u64> = exchange
            .waiting
            .iter()
            .filter(|(_, query)| **query != latest)
            .map(|(token, _)| *token)
            .collect();
        for token in stale {
            if let Some(query) = exchange.waiting.remove(&token) {
                exchange.resolved.insert(token, query);
            }
        }
        Some(latest)
    }

    /// Answer every token waiting on `query` with `places` (empty when the
    /// lookup failed).
    pub fn answer(&self, query: &str, places: Vec<Place>) {
        let mut exchange = self.exchange.borrow_mut();
        let done: Vec<u64> = exchange
            .waiting
            .iter()
            .filter(|(_, asked)| asked.trim().eq_ignore_ascii_case(query.trim()))
            .map(|(token, _)| *token)
            .collect();
        for token in done {
            if let Some(asked) = exchange.waiting.remove(&token) {
                exchange.resolved.insert(token, asked);
            }
        }
        exchange
            .answers
            .retain(|(asked, _)| !asked.eq_ignore_ascii_case(query.trim()));
        exchange.answers.push((query.trim().to_string(), places));
        if exchange.answers.len() > MAX_ANSWERS {
            exchange.answers.remove(0);
        }
    }

    /// A place the geocoder answered, by id.
    pub fn geocoded(&self, id: &str) -> Option<Place> {
        self.exchange
            .borrow()
            .answers
            .iter()
            .rev()
            .flat_map(|(_, places)| places.iter())
            .find(|place| place.id == id)
            .cloned()
    }

    /// Whether a query is waiting to be sent or answered.
    pub fn is_busy(&self) -> bool {
        let exchange = self.exchange.borrow();
        !exchange.asked.is_empty() || !exchange.waiting.is_empty()
    }
}

/// Maps' platform-search provider. See the module docs.
#[derive(Clone, Debug)]
pub struct PlaceSearchProvider {
    exchange: Rc<RefCell<Exchange>>,
    local: Rc<LocalPlaces>,
}

/// The result row for one place: its name, what it is or where it is, and
/// how far away it is.
fn place_result(group: &str, place: &Place, hit: &PlaceHit, anchor: LatLon) -> SearchResult {
    let distance = geo::fmt_distance(geo::haversine_m(place.pos, anchor));
    let subtitle = if place.address.is_empty() {
        format!("{}, {distance}", place.category.label())
    } else {
        format!("{}, {distance}", place.address)
    };
    let result = SearchResult::new(group, &place.name, hit.anchor());
    let result = if place.saved {
        result.within(format!("{SAVED_SCOPE}/{}", place.id))
    } else {
        result
    };
    result.with_subtitle(subtitle).with_icon(if place.saved {
        Icon::Star
    } else {
        place.category.icon()
    })
}

/// Whether `place` answers `needle` (already lower case): by name, address,
/// category, or a category keyword ("coffee" finds cafés).
pub fn place_matches(place: &Place, needle: &str) -> bool {
    place.name.to_lowercase().contains(needle)
        || place.address.to_lowercase().contains(needle)
        || place.category.label().to_lowercase().contains(needle)
        || keyword_match(needle, place.category)
}

/// Loose keyword → category synonyms for offline search.
pub fn keyword_match(needle: &str, cat: Category) -> bool {
    let syn: &[&str] = match cat {
        Category::Cafe => &["coffee", "cafe", "café", "espresso", "breakfast"],
        Category::Restaurant => &["food", "eat", "lunch", "dinner", "restaurant"],
        Category::Bar => &["beer", "drink", "pub", "bar", "biergarten"],
        Category::Museum => &["art", "museum", "gallery", "history"],
        Category::Park => &["park", "green", "garden", "nature"],
        Category::Transit => &["station", "train", "metro", "transit", "bahnhof"],
        Category::Hotel => &["hotel", "stay", "sleep", "room"],
        Category::Shop => &["shop", "market", "store", "buy"],
        Category::Landmark => &["sight", "landmark", "tourist"],
        _ => &[],
    };
    syn.iter()
        .any(|s| s.contains(needle) || needle.contains(*s))
}

impl PlaceSearchProvider {
    fn local_results(&self, text: &str, limit: usize) -> Vec<SearchResult> {
        let local = &self.local;
        if text.is_empty() {
            return local
                .recents
                .iter()
                .take(limit)
                .map(|query| {
                    SearchResult::new(
                        "Recent searches",
                        query,
                        PlaceHit::Recent(query.clone()).anchor(),
                    )
                    .with_icon(Icon::History)
                })
                .collect();
        }
        let needle = text.to_lowercase();
        let mut places: Vec<&Place> = local
            .places
            .iter()
            .filter(|place| place_matches(place, &needle))
            .collect();
        places.sort_by(|a, b| {
            b.saved.cmp(&a.saved).then_with(|| {
                geo::haversine_m(a.pos, local.anchor)
                    .total_cmp(&geo::haversine_m(b.pos, local.anchor))
            })
        });
        let mut results: Vec<SearchResult> = places
            .into_iter()
            .take(limit)
            .map(|place| {
                let group = if place.saved {
                    "Saved places"
                } else {
                    "Places"
                };
                place_result(
                    group,
                    place,
                    &PlaceHit::Place(place.id.clone()),
                    local.anchor,
                )
            })
            .collect();
        let room = limit.saturating_sub(results.len());
        let mut pois: Vec<&Place> = local
            .pois
            .iter()
            .filter(|poi| place_matches(poi, &needle))
            .filter(|poi| !local.places.iter().any(|place| place.id == poi.id))
            .collect();
        pois.sort_by(|a, b| {
            geo::haversine_m(a.pos, local.anchor).total_cmp(&geo::haversine_m(b.pos, local.anchor))
        });
        results.extend(
            pois.into_iter().take(room).map(|poi| {
                place_result("Nearby", poi, &PlaceHit::Poi(poi.id.clone()), local.anchor)
            }),
        );
        results
    }

    fn geocoded_results(&self, places: &[Place]) -> Vec<SearchResult> {
        places
            .iter()
            .filter(|place| !self.local.places.iter().any(|known| known.id == place.id))
            .map(|place| {
                place_result(
                    "Addresses and places",
                    place,
                    &PlaceHit::Geocoded(place.id.clone()),
                    self.local.anchor,
                )
            })
            .collect()
    }
}

impl SearchProvider for PlaceSearchProvider {
    fn name(&self) -> &'static str {
        "Places"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.local_results(query.trim(), MAX_LOCAL)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.local_results(query.trim(), limit.min(MAX_LOCAL))
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let text = query.text().trim();
        if cursor.is_some() || work_budget == 0 || result_limit == 0 {
            return SearchPage::empty();
        }
        let local: Vec<SearchResult> = self
            .local_results(text, result_limit.min(MAX_LOCAL))
            .into_iter()
            .filter(|hit| query.in_scope(&hit.anchor()))
            .collect();
        // Only a free search reaches the geocoder: a search narrowed to the
        // saved places, or one too short to mean a place, stays local.
        if query.scope().is_some() || text.chars().count() < 3 {
            return SearchPage::of(local, None);
        }
        let mut exchange = self.exchange.borrow_mut();
        if let Some(answered) = exchange.answer_for(text) {
            let mut results = local;
            results.extend(self.geocoded_results(answered));
            return SearchPage::of(results, None);
        }
        if !exchange.reachable {
            return SearchPage::of(local, None);
        }
        exchange.next_token = exchange.next_token.saturating_add(1);
        let token = exchange.next_token;
        exchange.asked.push(text.to_string());
        exchange.waiting.insert(token, text.to_string());
        drop(exchange);
        SearchPage::pending(local, PendingToken::new(token))
    }

    fn poll_page(&self, token: &PendingToken) -> PagePoll {
        let mut exchange = self.exchange.borrow_mut();
        if exchange.waiting.contains_key(&token.id()) {
            return PagePoll::Pending;
        }
        // The query this token stood for has its answer now, or was
        // superseded (or is unknown) and finishes empty.
        let answered = exchange
            .resolved
            .remove(&token.id())
            .and_then(|query| exchange.answer_for(&query).map(<[Place]>::to_vec))
            .unwrap_or_default();
        drop(exchange);
        PagePoll::Ready(SearchPage::of(self.geocoded_results(&answered), None))
    }

    fn search_scopes(&self) -> Vec<ScopeDecl> {
        vec![ScopeDecl::new(SAVED_SCOPE, "Saved places")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, name: &str, category: Category, saved: bool) -> Place {
        let mut place = Place::new(id, name, category, "Munich", LatLon::new(48.137, 11.575));
        place.saved = saved;
        place
    }

    fn local() -> LocalPlaces {
        LocalPlaces {
            places: vec![
                place("cafe-glueck", "Café Glück", Category::Cafe, true),
                place("marienplatz", "Marienplatz", Category::Landmark, false),
            ],
            pois: vec![place("poi-1", "Hofbräuhaus", Category::Bar, false)],
            recents: vec!["Marienplatz".into(), "coffee near me".into()],
            anchor: LatLon::new(48.137, 11.575),
        }
    }

    #[test]
    fn local_places_answer_at_once_and_the_geocoder_later() {
        let search = PlaceSearch::default();
        let provider = search.provider(local());

        // An empty query lists the recent searches.
        let recents = provider.search_page(&SearchQuery::new(""), None, 10, 10);
        assert_eq!(recents.results.len(), 2);
        assert_eq!(
            recents.results[0].anchor(),
            PlaceHit::Recent("Marienplatz".into()).anchor()
        );

        // A keyword finds the saved café at once; unreachable, nothing waits.
        let page = provider.search_page(&SearchQuery::new("coffee"), None, 10, 10);
        assert!(page.pending.is_none());
        assert_eq!(page.results[0].title, "Café Glück");
        assert_eq!(page.results[0].kind, "Saved places");
        assert!(search.take_query().is_none());

        // Reachable: the local hits come now, the geocoder's later.
        search.set_reachable(true);
        let page = provider.search_page(&SearchQuery::new("Marien"), None, 10, 10);
        let token = page.pending.clone().expect("the geocoder answers later");
        assert_eq!(page.results[0].title, "Marienplatz");
        assert_eq!(provider.poll_page(&token), PagePoll::Pending);
        assert_eq!(search.take_query().as_deref(), Some("Marien"));

        search.answer(
            "Marien",
            vec![place("geo-1", "Marienhof", Category::Other, false)],
        );
        let PagePoll::Ready(ready) = provider.poll_page(&token) else {
            panic!("answered");
        };
        assert_eq!(ready.results[0].title, "Marienhof");
        let anchor = ready.results[0].anchor();
        assert_eq!(anchor, "maps.place:geo-1");
        assert_eq!(
            PlaceHit::opened(&anchor),
            Some(MapsEffect::OpenPlace {
                place: "geo-1".into()
            }),
            "the found place opens like one on the map"
        );
        assert_eq!(
            search.geocoded("geo-1").map(|p| p.name),
            Some("Marienhof".into())
        );
    }

    #[test]
    fn saved_places_are_a_search_scope() {
        let provider = PlaceSearch::default().provider(local());
        assert_eq!(provider.search_scopes()[0].anchor, SAVED_SCOPE);
        let page = provider.search_page(&SearchQuery::new("Café"), None, 10, 10);
        let saved_hit = page.results[0].anchor();
        assert!(ui::search::scope_contains(SAVED_SCOPE, &saved_hit));
        let page = provider.search_page(&SearchQuery::new("Marienplatz"), None, 10, 10);
        let other = page.results[0].anchor();
        assert!(!ui::search::scope_contains(SAVED_SCOPE, &other));
    }
}
