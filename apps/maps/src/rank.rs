//! Relevance ranking for geocoded place results.
//!
//! Nominatim returns candidates in descending "importance", which routinely
//! puts a far-away place above a much nearer one that matches the query just as
//! well — the reported symptom was a place **511 km** away shown above one
//! **2.8 km** away, both with the *exact same name*. Importance-only order is
//! wrong for a maps app: when two candidates match the typed text equally, the
//! one near where the user is looking is almost always the one they mean.
//!
//! This module re-orders candidates so that:
//!   1. better **text matches** rank first (an exact name beats a prefix beats
//!      a mere substring), then
//!   2. within a text-match tier, the **nearest** to the search anchor wins —
//!      so identically-named places are ordered closest-first.
//!
//! It also collapses true duplicates: the same place returned twice (same name,
//! essentially the same coordinate) is de-duplicated, while two genuinely
//! different places that merely share a name (a town and a distant namesake)
//! are both kept — they are disambiguated for the reader by their region label.
//!
//! Pure and free-standing so the ordering is unit-testable without a network
//! round-trip or a running app.

use crate::geo::haversine_m;
use crate::model::{LatLon, Place};
use ui::search::MatchTier;

/// Two candidates closer together than this (metres) with the same normalised
/// name are treated as the *same* place and de-duplicated. Comfortably larger
/// than the jitter between two OSM records for one feature, yet far smaller than
/// the gap between genuine namesakes in different towns.
const DEDUPE_RADIUS_M: f64 = 150.0;

/// How well `name` matches the typed `query` — the shared search ranker's
/// tier, accent- and case-insensitive, with runs of whitespace collapsed.
/// `None` means no textual overlap (kept, but ranked below anything that
/// matched).
///
/// An empty query ranks every candidate as an exact match, so ranking falls
/// back to pure proximity — the right behaviour for the offline substring
/// lists that have already filtered by name.
pub fn match_tier(query: &str, name: &str) -> Option<MatchTier> {
    let q = normalise(query);
    if q.is_empty() {
        return Some(MatchTier::Exact);
    }
    ui::search::rank::match_tier(&q, &normalise(name))
}

/// Fold with the shared search folding and collapse runs of whitespace, so
/// `"  Café   Glück "` and `"cafe gluck"` compare equal.
fn normalise(s: &str) -> String {
    ui::search::fold_search_text(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Re-order `places` by relevance to `query`, breaking ties by proximity to
/// `anchor`, and collapse true duplicates in place.
///
/// The sort is stable, so candidates that are equal on both tier and distance
/// keep their upstream (importance) order — a sensible last-resort tiebreak.
pub fn rank(places: &mut Vec<Place>, anchor: LatLon, query: &str) {
    places.sort_by(|a, b| {
        match_tier(query, &b.name)
            .cmp(&match_tier(query, &a.name))
            .then_with(|| haversine_m(a.pos, anchor).total_cmp(&haversine_m(b.pos, anchor)))
    });
    dedupe_same_place(places);
}

/// Drop a candidate when an earlier (already better-ranked) one shares its
/// normalised name and sits within [`DEDUPE_RADIUS_M`]. Because [`rank`] has
/// already ordered nearest-first within a tier, the kept copy is the closest —
/// distant namesakes survive because they fall outside the radius.
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn ranking_weighs_text_match_proximity_duplicates_and_folding() {
        // Phase `near_same_named_place_ranks_above_a_far_one`.
        // The reported bug: two places with the *exact same name*, one near and one
        // far. The near one must rank first regardless of the upstream order.
        {
            let anchor = LatLon::new(48.137, 11.575); // Munich
            // Far one listed FIRST (as Nominatim importance would),
            // ~511 km away (Hamburg-ish); near one ~2.8 km away.
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

        // Phase `exact_text_match_beats_partial_even_if_slightly_closer`.
        // An exact text match outranks a merely-partial one even when the partial
        // match is a little closer — relevance tiers before distance.
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

        // Phase `duplicates_collapse_but_distant_namesakes_survive`.
        // A duplicate of the same place (same name, ~same point) collapses; a
        // genuine namesake in another region is kept and disambiguated.
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

        // Phase `matching_folds_accents_and_case`.
        // Accent- and case-insensitive matching: "munchen" matches "München".
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

        // Phase `empty_query_is_pure_proximity`.
        // An empty query ranks purely by proximity (the offline-list fallback).
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
