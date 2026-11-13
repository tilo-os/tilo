//! Relevance ranking for geocoded places. Nominatim orders by importance, which puts a far namesake
//! above the near place meant; this ranks text match first, then distance to the search anchor, and
//! collapses true duplicates while keeping distinct places that share a name.

use crate::geo::haversine_m;
use crate::model::{LatLon, Place};
use ui::search::MatchTier;

/// Two candidates closer together than this (metres) with the same normalised name are treated as
/// the *same* place and de-duplicated. Comfortably larger than the jitter between two OSM records
/// for one feature, yet far smaller than the gap between genuine namesakes in different towns.
const DEDUPE_RADIUS_M: f64 = 150.0;

pub fn match_tier(query: &str, name: &str) -> Option<MatchTier> {
    let q = normalise(query);
    if q.is_empty() {
        return Some(MatchTier::Exact);
    }
    ui::search::rank::match_tier(&q, &normalise(name))
}

fn normalise(s: &str) -> String {
    ui::search::fold_search_text(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Re-order by relevance to `query`, ties broken by distance to `anchor`, dropping duplicates in
/// place. The sort is stable, so equal candidates keep their upstream order.
pub fn rank(places: &mut Vec<Place>, anchor: LatLon, query: &str) {
    places.sort_by(|a, b| {
        match_tier(query, &b.name)
            .cmp(&match_tier(query, &a.name))
            .then_with(|| haversine_m(a.pos, anchor).total_cmp(&haversine_m(b.pos, anchor)))
    });
    dedupe_same_place(places);
}

/// Drop a candidate when a better-ranked one shares its normalised name and sits within
/// [`DEDUPE_RADIUS_M`]; distant namesakes survive.
fn dedupe_same_place(places: &mut Vec<Place>) {
    let mut kept: Vec<(String, LatLon)> = Vec::with_capacity(places.len());
    places.retain(|p| {
        let key = normalise(&p.name);
        let dup = kept
            .iter()
            .any(|(name, pos)| name == &key && haversine_m(*pos, p.pos) < DEDUPE_RADIUS_M);
        if dup {
            false
        } else {
            kept.push((key, p.pos));
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn place(id: &str, name: &str, pos: LatLon) -> Place {
        Place::new(id, name, Category::Cafe, "", pos)
    }

    #[test]
    fn ranking_weighs_text_match_proximity_duplicates_and_folding() {
        {
            let anchor = LatLon::new(48.137, 11.575); // Munich
            let mut places = vec![
                place("far", "Sankt Nikolai", LatLon::new(53.55, 9.99)),
                place("near", "Sankt Nikolai", LatLon::new(48.16, 11.58)),
            ];
            let far_km = haversine_m(anchor, places[0].pos) / 1000.0;
            let near_km = haversine_m(anchor, places[1].pos) / 1000.0;
            assert!(
                far_km > 400.0 && near_km < 5.0,
                "fixture: far {far_km:.0}km, near {near_km:.1}km"
            );

            rank(&mut places, anchor, "Sankt Nikolai");

            assert_eq!(
                places[0].id, "near",
                "the nearer same-named place ranks first"
            );
            assert_eq!(places[1].id, "far", "the distant namesake follows");
        }

        {
            let anchor = LatLon::new(48.137, 11.575);
            let mut places = vec![
                place("partial", "Munich Airport", LatLon::new(48.20, 11.60)),
                place("exact", "Munich", LatLon::new(48.30, 11.70)),
            ];
            rank(&mut places, anchor, "munich");
            assert_eq!(
                places[0].id, "exact",
                "exact name match ranks above a prefix match"
            );
        }

        {
            let anchor = LatLon::new(48.137, 11.575);
            let mut places = vec![
                place("a", "Marienplatz", LatLon::new(48.1374, 11.5755)),
                place("a-dup", "Marienplatz", LatLon::new(48.1375, 11.5756)), // ~15 m away
                place("elsewhere", "Marienplatz", LatLon::new(49.45, 11.08)), // Nuremberg
            ];
            rank(&mut places, anchor, "Marienplatz");
            let ids: Vec<_> = places.iter().map(|p| p.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["a", "elsewhere"],
                "co-located dup dropped, namesake kept"
            );
        }

        {
            assert_eq!(match_tier("munchen", "München"), Some(MatchTier::Exact));
            assert_eq!(match_tier("CAFE", "Café Glück"), Some(MatchTier::Prefix));
            assert_eq!(
                match_tier("gluck", "Café Glück"),
                Some(MatchTier::WordPrefix)
            );
            assert_eq!(match_tier("luck", "Café Glück"), Some(MatchTier::Contains));
            assert_eq!(match_tier("zzz", "München"), None);
        }

        {
            let anchor = LatLon::new(0.0, 0.0);
            let mut places = vec![
                place("far", "B", LatLon::new(1.0, 1.0)),
                place("near", "A", LatLon::new(0.1, 0.1)),
            ];
            rank(&mut places, anchor, "");
            assert_eq!(places[0].id, "near");
        }
    }
}
