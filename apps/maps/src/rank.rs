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

/// Two candidates closer together than this (metres) with the same normalised
/// name are treated as the *same* place and de-duplicated. Comfortably larger
/// than the jitter between two OSM records for one feature, yet far smaller than
/// the gap between genuine namesakes in different towns.
const DEDUPE_RADIUS_M: f64 = 150.0;

/// How well `name` matches the typed `query`, as a tier where **lower is
/// better**. Accent- and case-insensitive.
///
///   * `0` — exact match (`"München"` for query `"munchen"`),
///   * `1` — the name *starts with* the query (`"Munich Airport"` / `"muni"`),
///   * `2` — the name *contains* the query somewhere,
///   * `3` — no textual overlap (kept, but ranked below anything that matched).
///
/// An empty query collapses every candidate to tier `0`, so ranking falls back
/// to pure proximity — the right behaviour for the offline substring lists that
/// have already filtered by name.
pub fn match_tier(query: &str, name: &str) -> u8 {
    let q = normalise(query);
    if q.is_empty() {
        return 0;
    }
    let n = normalise(name);
    if n == q {
        0
    } else if n.starts_with(&q) {
        1
    } else if n.contains(&q) {
        2
    } else {
        3
    }
}

/// Lowercase, fold common Latin accents to their base letter, and collapse
/// runs of whitespace so `"  Café   Glück "` and `"cafe gluck"` compare equal.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // trims leading whitespace
    for ch in s.trim().chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = false;
        for folded in fold(ch) {
            out.extend(folded.to_lowercase());
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Fold a single accented Latin letter to its base ASCII letter(s). Anything
/// without a mapping passes through unchanged.
fn fold(ch: char) -> impl Iterator<Item = char> {
    let base: &[char] = match ch {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => &['a'],
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => &['e'],
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => &['i'],
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ø' => &['o'],
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => &['u'],
        'ñ' | 'Ñ' => &['n'],
        'ç' | 'Ç' => &['c'],
        'ß' => &['s', 's'],
        other => return OneOrChar::Pass(std::iter::once(other)),
    };
    OneOrChar::Folded(base.iter().copied())
}

/// Tiny iterator that yields either the original char or a folded slice without
/// allocating.
enum OneOrChar {
    Pass(std::iter::Once<char>),
    Folded(std::iter::Copied<std::slice::Iter<'static, char>>),
}

impl Iterator for OneOrChar {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        match self {
            OneOrChar::Pass(it) => it.next(),
            OneOrChar::Folded(it) => it.next(),
        }
    }
}

/// Re-order `places` by relevance to `query`, breaking ties by proximity to
/// `anchor`, and collapse true duplicates in place.
///
/// The sort is stable, so candidates that are equal on both tier and distance
/// keep their upstream (importance) order — a sensible last-resort tiebreak.
pub fn rank(places: &mut Vec<Place>, anchor: LatLon, query: &str) {
    places.sort_by(|a, b| {
        match_tier(query, &a.name)
            .cmp(&match_tier(query, &b.name))
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

    /// The reported bug: two places with the *exact same name*, one near and one
    /// far. The near one must rank first regardless of the upstream order.
    #[test]
    fn near_same_named_place_ranks_above_a_far_one() {
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

    /// An exact text match outranks a merely-partial one even when the partial
    /// match is a little closer — relevance tiers before distance.
    #[test]
    fn exact_text_match_beats_partial_even_if_slightly_closer() {
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

    /// A duplicate of the same place (same name, ~same point) collapses; a
    /// genuine namesake in another region is kept and disambiguated.
    #[test]
    fn duplicates_collapse_but_distant_namesakes_survive() {
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

    /// Accent- and case-insensitive matching: "munchen" matches "München".
    #[test]
    fn matching_folds_accents_and_case() {
        assert_eq!(match_tier("munchen", "München"), 0);
        assert_eq!(match_tier("CAFE", "Café Glück"), 1);
        assert_eq!(match_tier("gluck", "Café Glück"), 2);
        assert_eq!(match_tier("zzz", "München"), 3);
    }

    /// An empty query ranks purely by proximity (the offline-list fallback).
    #[test]
    fn empty_query_is_pure_proximity() {
        let anchor = LatLon::new(0.0, 0.0);
        let mut places = vec![
            place("far", "B", LatLon::new(1.0, 1.0)),
            place("near", "A", LatLon::new(0.1, 0.1)),
        ];
        rank(&mut places, anchor, "");
        assert_eq!(places[0].id, "near");
    }
}
