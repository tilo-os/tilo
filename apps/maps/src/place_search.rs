//! Maps in the platform search: the classes the palette lists, and the lookups [`PlaceSearch`]
//! keeps with the geocoder.

use ui::icons::Icon;
use ui::search::SearchQuery;
use ui::tilo_resource::{SearchEntry, SearchWeight};

use crate::failure;
use crate::geo;
use crate::model::{Category, LatLon, Place, Poi};
use crate::resource::MapsEffect;

/// The scope saved places lie inside: the "Saved places" search scope.
pub const SAVED_SCOPE: &str = "maps-saved";
const PLACE_ROW: &str = "maps.place:";
const POI_ROW: &str = "maps.poi:";
const HISTORY_ROW: &str = "maps.search-history:";

/// Geocoder answers kept by query: the palette's latest query, and the places a hit of an earlier
/// one may still open.
const MAX_ANSWERS: usize = 16;
/// The shortest query that means a place to the geocoder.
const MIN_GEOCODED_QUERY: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceHit {
    Place(String),
    Poi(String),
    Geocoded(String),
    Recent(String),
}

impl PlaceHit {
    pub fn resource_id(&self) -> String {
        match self {
            Self::Place(id) | Self::Geocoded(id) => format!("{PLACE_ROW}{id}"),
            Self::Poi(id) => format!("{POI_ROW}{id}"),
            Self::Recent(query) => format!("{HISTORY_ROW}{query}"),
        }
    }

    pub fn effect(&self) -> MapsEffect {
        match self {
            Self::Place(id) | Self::Geocoded(id) => MapsEffect::OpenPlace { place: id.clone() },
            Self::Poi(id) => MapsEffect::OpenPoi { poi: id.clone() },
            Self::Recent(query) => MapsEffect::Search {
                query: query.clone(),
            },
        }
    }

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

#[derive(Debug, Default)]
pub struct PlaceSearch {
    latest: Option<String>,
    unsent: Option<String>,
    sent: Vec<String>,
    answers: Vec<(String, Vec<Place>)>,
}

impl PlaceSearch {
    /// Take the search's newest query. Only an unscoped query long enough to mean a place reaches
    /// the geocoder, once, and only when one can be reached; a saved-places scope stays local.
    pub fn ask(&mut self, query: &SearchQuery, reachable: bool) {
        let text = query.text().trim();
        if query.scope().is_some() || text.chars().count() < MIN_GEOCODED_QUERY {
            self.latest = None;
            return;
        }
        self.latest = Some(text.to_owned());
        let known = self.answer_for(text).is_some()
            || self.sent.iter().any(|sent| sent.eq_ignore_ascii_case(text));
        if reachable && !known {
            self.unsent = Some(text.to_owned());
        }
    }

    pub fn take_query(&mut self) -> Option<String> {
        let query = self.unsent.take()?;
        self.sent.push(query.clone());
        Some(query)
    }

    pub fn answer(&mut self, query: &str, places: Vec<Place>) {
        let query = query.trim();
        self.sent.retain(|sent| !sent.eq_ignore_ascii_case(query));
        self.answers
            .retain(|(asked, _)| !asked.eq_ignore_ascii_case(query));
        self.answers.push((query.to_owned(), places));
        if self.answers.len() > MAX_ANSWERS {
            self.answers.remove(0);
        }
    }

    pub fn found(&self) -> Option<(&str, &[Place])> {
        let query = self.latest.as_deref()?;
        Some((query, self.answer_for(query)?))
    }

    pub fn geocoded(&self, id: &str) -> Option<&Place> {
        self.answers
            .iter()
            .rev()
            .flat_map(|(_, places)| places.iter())
            .find(|place| place.id == id)
    }

    pub fn is_busy(&self) -> bool {
        self.unsent.is_some() || !self.sent.is_empty()
    }

    fn answer_for(&self, query: &str) -> Option<&[Place]> {
        self.answers
            .iter()
            .rev()
            .find(|(asked, _)| asked.eq_ignore_ascii_case(query))
            .map(|(_, places)| places.as_slice())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Shown<'a> {
    pub places: &'a [Place],
    pub pois: &'a [Poi],
    pub recents: &'a [String],
    pub found: Option<(&'a str, &'a [Place])>,
    pub anchor: LatLon,
}

struct Spot<'a> {
    name: &'a str,
    address: &'a str,
    category: Category,
    pos: LatLon,
    saved: bool,
}

impl Spot<'_> {
    fn entry(&self, group: &str, hit: &PlaceHit, anchor: LatLon) -> SearchEntry {
        let distance = geo::fmt_distance(geo::haversine_m(self.pos, anchor));
        let subtitle = if self.address.is_empty() {
            format!("{}, {distance}", self.category.label())
        } else {
            format!("{}, {distance}", self.address)
        };
        let icon = if self.saved {
            Icon::Star
        } else {
            self.category.icon()
        };
        let mut entry = SearchEntry::new(self.name, String::new())
            .field(self.category.label(), SearchWeight::Metadata)
            .opening(hit.resource_id(), &hit.effect())
            .grouped(group)
            .subtitled(subtitle)
            .with_icon(icon.name());
        for keyword in keywords(self.category) {
            entry = entry.field(*keyword, SearchWeight::Unranked);
        }
        if self.saved {
            entry = entry.inside(SAVED_SCOPE);
        }
        entry
    }
}

impl<'a> From<&'a Place> for Spot<'a> {
    fn from(place: &'a Place) -> Self {
        Self {
            name: &place.name,
            address: &place.address,
            category: place.category,
            pos: place.pos,
            saved: place.saved,
        }
    }
}

pub fn entries(shown: &Shown<'_>) -> Vec<SearchEntry> {
    let known = |id: &str| shown.places.iter().any(|place| place.id == id);
    let mut entries: Vec<SearchEntry> = shown
        .places
        .iter()
        .map(|place| {
            let group = if place.saved {
                failure::t("maps-saved-places")
            } else {
                failure::t("maps-panel-places")
            };
            Spot::from(place).entry(&group, &PlaceHit::Place(place.id.clone()), shown.anchor)
        })
        .collect();
    entries.extend(shown.pois.iter().filter(|poi| !known(&poi.id)).map(|poi| {
        let spot = Spot {
            name: &poi.name,
            address: &poi.address,
            category: poi.category,
            pos: poi.pos,
            saved: false,
        };
        let entry = spot.entry(
            &failure::t("maps-nearby"),
            &PlaceHit::Poi(poi.id.clone()),
            shown.anchor,
        );
        match &poi.kind {
            Some(kind) => entry.field(kind.as_str(), SearchWeight::Metadata),
            None => entry,
        }
    }));
    entries.extend(shown.recents.iter().map(|query| {
        let hit = PlaceHit::Recent(query.clone());
        SearchEntry::new(query.as_str(), String::new())
            .opening(hit.resource_id(), &hit.effect())
            .grouped(failure::t("maps-recent-searches"))
            .with_icon(Icon::History.name())
    }));
    if let Some((query, found)) = shown.found {
        entries.extend(found.iter().filter(|place| !known(&place.id)).map(|place| {
            Spot::from(place)
                .entry(
                    &failure::t("maps-addresses-places"),
                    &PlaceHit::Geocoded(place.id.clone()),
                    shown.anchor,
                )
                .field(query, SearchWeight::Unranked)
        }));
    }
    entries
}

/// Loose words a category is searched by offline.
fn keywords(cat: Category) -> &'static [&'static str] {
    match cat {
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
    }
}

pub fn keyword_match(needle: &str, cat: Category) -> bool {
    keywords(cat)
        .iter()
        .any(|s| s.contains(needle) || needle.contains(*s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider as _;

    fn place(id: &str, name: &str, category: Category, saved: bool) -> Place {
        let mut place = Place::new(id, name, category, "Munich", LatLon::new(48.137, 11.575));
        place.saved = saved;
        place
    }

    fn poi(id: &str, name: &str, category: Category) -> Poi {
        Poi {
            id: id.into(),
            name: name.into(),
            category,
            pos: LatLon::new(48.138, 11.576),
            address: String::new(),
            opening_hours: None,
            website: None,
            phone: None,
            kind: None,
        }
    }

    fn search(entries: Vec<SearchEntry>, query: &SearchQuery) -> Vec<(String, String)> {
        ui::framework::testing::search_source::<crate::workspace::Workspace>(entries)
            .search_page(query, None, 50, 50)
            .results
            .into_iter()
            .map(|hit| (hit.kind, hit.title))
            .collect()
    }

    #[test]
    fn the_map_answers_by_name_keyword_and_scope() {
        let places = [
            place("cafe-glueck", "Café Glück", Category::Cafe, true),
            place("marienplatz", "Marienplatz", Category::Landmark, false),
        ];
        let pois = [
            poi("poi-1", "Hofbräuhaus", Category::Bar),
            poi("marienplatz", "Marienplatz", Category::Landmark),
        ];
        let recents = ["coffee near me".to_owned()];
        let shown = Shown {
            places: &places,
            pois: &pois,
            recents: &recents,
            found: None,
            anchor: LatLon::new(48.137, 11.575),
        };
        let all = entries(&shown);
        assert_eq!(
            all.len(),
            4,
            "a point of interest the map knows is listed once"
        );

        let coffee = search(all.clone(), &SearchQuery::new("coffee"));
        assert!(
            coffee.contains(&("Saved places".into(), "Café Glück".into())),
            "a keyword finds the saved café: {coffee:?}"
        );
        let beer = search(all.clone(), &SearchQuery::new("beer"));
        assert!(
            beer.contains(&("Nearby".into(), "Hofbräuhaus".into())),
            "{beer:?}"
        );
        let marien = search(all.clone(), &SearchQuery::new("Marien"));
        assert!(
            marien.contains(&("Places".into(), "Marienplatz".into())),
            "{marien:?}"
        );

        let cafe = all
            .iter()
            .find(|entry| entry.title == "Café Glück")
            .unwrap();
        assert_eq!(cafe.anchor.as_deref(), Some("maps.place:cafe-glueck"));
        assert_eq!(cafe.icon.as_deref(), Some(Icon::Star.name()));
        assert!(cafe.subtitle.as_deref().unwrap().starts_with("Munich, "));
        assert!(cafe.in_scope(SAVED_SCOPE), "saved places narrow the search");
        let square = all
            .iter()
            .find(|entry| entry.title == "Marienplatz")
            .unwrap();
        assert!(!square.in_scope(SAVED_SCOPE));
        let recent = all
            .iter()
            .find(|entry| entry.title == "coffee near me")
            .unwrap();
        assert_eq!(
            recent
                .action
                .as_ref()
                .map(|address| address.action.as_str()),
            Some("search"),
            "a recent search runs again"
        );
    }

    #[test]
    fn only_a_free_long_query_reaches_the_geocoder_once() {
        let mut search = PlaceSearch::default();
        search.ask(&SearchQuery::new("ma"), true);
        search.ask(&SearchQuery::new("Marien").within(SAVED_SCOPE), true);
        search.ask(&SearchQuery::new("Marien"), false);
        assert_eq!(search.take_query(), None, "short, scoped or offline");

        search.ask(&SearchQuery::new("Mari"), true);
        search.ask(&SearchQuery::new("Marien"), true);
        assert_eq!(search.take_query().as_deref(), Some("Marien"), "the latest");
        assert_eq!(search.take_query(), None, "sent once");
        assert!(search.is_busy());
        search.ask(&SearchQuery::new("marien "), true);
        assert_eq!(search.take_query(), None, "already on its way");
        assert!(search.found().is_none());

        search.answer(
            "Marien",
            vec![place("geo-1", "Marienhof", Category::Other, false)],
        );
        assert!(!search.is_busy());
        let (query, found) = search.found().expect("answered");
        assert_eq!((query, found[0].name.as_str()), ("marien", "Marienhof"));
        search.ask(&SearchQuery::new("Marien"), true);
        assert_eq!(search.take_query(), None, "answered already");
        search.ask(&SearchQuery::new(""), true);
        assert!(search.found().is_none(), "a cleared search lists no answer");
        assert_eq!(
            search.geocoded("geo-1").map(|place| place.name.as_str()),
            Some("Marienhof"),
            "an earlier answer still opens"
        );
    }

    #[test]
    fn geocoded_places_are_found_by_their_query() {
        let places = [place("known", "Marienplatz", Category::Landmark, false)];
        let found = [
            place("geo-1", "Theatinerstraße 18", Category::Shop, false),
            place("known", "Marienplatz", Category::Landmark, false),
        ];
        let shown = Shown {
            places: &places,
            pois: &[],
            recents: &[],
            found: Some(("theatiner", &found)),
            anchor: LatLon::new(48.137, 11.575),
        };
        let all = entries(&shown);
        assert_eq!(all.len(), 2, "a known place is not listed twice");
        let hits = search(all.clone(), &SearchQuery::new("theatiner"));
        assert!(
            hits.contains(&("Addresses and places".into(), "Theatinerstraße 18".into())),
            "{hits:?}"
        );
        let geo = all
            .iter()
            .find(|entry| entry.title.starts_with("Theatiner"))
            .unwrap();
        assert_eq!(geo.anchor.as_deref(), Some("maps.place:geo-1"));
        assert_eq!(
            PlaceHit::opened("maps.place:geo-1"),
            Some(MapsEffect::OpenPlace {
                place: "geo-1".into()
            }),
            "the found place opens like one on the map"
        );
    }
}
