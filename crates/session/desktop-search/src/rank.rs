//! The cross-provider ranking every desktop search surface shares: how one
//! provider's hit is scored on the shared ranker, how hits from different
//! providers compare, and how the merged list is grouped by kind.

use std::cmp::Ordering;
use std::collections::HashMap;

use ui::search::{FieldWeight, MatchTier};

/// One searchable field and its weight, as the shared ranker takes it.
pub type RankField<'a> = (&'a str, FieldWeight);

use crate::kind::ResourceKind;

/// Keep every provider (including network directories) behind the same
/// per-keystroke Unicode input ceiling as app-side [`ui::search::SearchCatalog`]
/// providers. The check happens before provider routing, so no provider gets
/// to lower-case or otherwise allocate from an arbitrarily pasted query.
pub fn query_input_is_bounded(needle: &str) -> bool {
    needle
        .chars()
        .take(ui::search::MAX_SEARCH_QUERY_SCALARS.saturating_add(1))
        .count()
        <= ui::search::MAX_SEARCH_QUERY_SCALARS
}

/// Provider-independent lexical fit used at the global merge boundary. A
/// provider still contributes domain knowledge through its normalized score,
/// but an exact visible title must beat a weak metadata/fuzzy match from a
/// different source.
pub fn lexical_strength(
    needle: &str,
    kind: &ResourceKind,
    title: &str,
    subtitle: Option<&str>,
) -> f32 {
    let needle = ui::search::fold_search_text(needle.trim());
    if needle.is_empty() {
        return 0.0;
    }
    let title = ui::search::fold_search_text(title);
    // Application providers decorate the app name with the launch verb.
    // Compare its name on the same basis as a file or note title.
    let title = if matches!(
        kind,
        ResourceKind::Application | ResourceKind::HostApplication
    ) {
        title.strip_prefix("open ").unwrap_or(&title)
    } else {
        &title
    };
    match ui::search::rank::match_tier(&needle, title) {
        Some(MatchTier::Exact) => 4.0,
        Some(MatchTier::Prefix | MatchTier::PrefixWord) => 2.5,
        Some(MatchTier::WordPrefix | MatchTier::Word) => 1.5,
        Some(MatchTier::Contains) => 1.0,
        Some(MatchTier::Spanning | MatchTier::Typo) => 0.5,
        None => subtitle.map_or(0.0, |subtitle| {
            match ui::search::rank::match_tier(&needle, &ui::search::fold_search_text(subtitle)) {
                Some(MatchTier::Exact) => 0.7,
                Some(_) => 0.4,
                None => 0.0,
            }
        }),
    }
}

/// Top of every provider's ranker-derived score.
pub const MAX_PROVIDER_SCORE: i32 = 100;

/// The shared ranker's rank of `fields` for `needle`, on the providers'
/// `0..=MAX_PROVIDER_SCORE` scale. A title prefix is already a perfect hit
/// for a provider — it is what a person types to launch or open something —
/// so the scale tops out there; [`lexical_strength`] still separates an
/// exact title from a prefix across providers.
pub fn rank_fields(needle: &str, fields: &[RankField<'_>]) -> Option<i32> {
    ui::search::ListIndex::default()
        .ranks(needle, [fields.iter().copied()])
        .into_iter()
        .next()
        .flatten()
        .map(provider_score)
}

/// Map a shared field rank onto the provider scale; see [`rank_fields`].
pub fn provider_score(rank: i32) -> i32 {
    let perfect = MatchTier::Prefix.percent();
    rank.clamp(0, perfect)
        .saturating_mul(MAX_PROVIDER_SCORE)
        .checked_div(perfect)
        .unwrap_or(MAX_PROVIDER_SCORE)
}

/// A deliberately small contextual preference: proximity should settle close
/// matches, never make a weak metadata hit beat an exact visible title.
pub const ACTIVE_APP_PROXIMITY_BOOST: f32 = 0.25;

/// A provider-perfect application-name match represents launch intent, not a
/// coincidental lexical occurrence. It must beat an exact filename/note with
/// the same word, while an exact action (strength 5.0) still beats an
/// unrelated app that merely received a weak provider score.
pub const PERFECT_APPLICATION_INTENT_BOOST: f32 = 3.0;

/// A [`provider score`](rank_fields) as a `0.0..=1.0` fraction of the
/// provider's own top. Provider scales are small integers (files top out at
/// 120, apps at 100), well inside `f32`'s exact range.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider score scales are small integers (<=120 by convention); exact precision isn't needed for a 0..1 strength ratio"
)]
fn fraction(score: i32, top: i32) -> f32 {
    score as f32 / top.max(1) as f32
}

/// One hit's strength against every other provider's: its lexical fit, its
/// provider's normalized score, launch intent for a perfect application
/// match, and the caller's contextual `proximity` boost.
pub fn merged_strength(
    needle: &str,
    kind: &ResourceKind,
    title: &str,
    subtitle: Option<&str>,
    score: i32,
    top: i32,
    proximity: f32,
) -> f32 {
    let top = top.max(1);
    let application_intent = if matches!(
        kind,
        ResourceKind::Application | ResourceKind::HostApplication
    ) && score >= top
    {
        PERFECT_APPLICATION_INTENT_BOOST
    } else {
        0.0
    };
    lexical_strength(needle, kind, title, subtitle)
        + fraction(score, top)
        + application_intent
        + proximity
}

/// Order merged hits: groups by how well their best member matched, then
/// the fixed kind order, then strength within a group. Enter activates the
/// top row, so an app's name must not open an action that merely mentions
/// the word. The sort is stable: equally strong hits keep their provider's
/// own order rather than being re-alphabetised by title.
pub fn order_by_kind<T>(merged: &mut [(T, f32)], kind: impl Fn(&T) -> &ResourceKind) {
    let mut best: HashMap<(u8, String), f32> = HashMap::new();
    for (hit, strength) in merged.iter() {
        let slot = best.entry(kind(hit).sort_key()).or_insert(f32::MIN);
        *slot = slot.max(*strength);
    }
    merged.sort_by(|(a, a_strength), (b, b_strength)| {
        let (a_key, b_key) = (kind(a).sort_key(), kind(b).sort_key());
        let a_group = best.get(&a_key).copied().unwrap_or(f32::MIN);
        let b_group = best.get(&b_key).copied().unwrap_or(f32::MIN);
        b_group
            .partial_cmp(&a_group)
            .unwrap_or(Ordering::Equal)
            // Equally strong groups fall back to the fixed kind order.
            .then_with(|| a_key.cmp(&b_key))
            .then_with(|| {
                b_strength
                    .partial_cmp(a_strength)
                    .unwrap_or(Ordering::Equal)
            })
    });
}

/// One contiguous run of same-kind results, as [`group_by_kind`] arranges
/// them.
#[derive(Debug)]
pub struct KindGroup<T> {
    pub kind: ResourceKind,
    /// The rows to render — at most the cap the caller asked for.
    pub results: Vec<T>,
    /// How many results this kind actually had, before the cap.
    pub total: usize,
}

/// Ranked hits arranged into contiguous per-kind groups and trimmed to a
/// `row_budget` shared round-robin: a palette paints ~10 rows, and an
/// outscoring kind (forty file hits) can otherwise push a correctly-ranked
/// app off the bottom. A lone group is exempt — narrowing to one kind means
/// "show me all of these". A zero budget keeps everything.
pub fn group_by_kind<T>(
    ranked: Vec<T>,
    kind: impl Fn(&T) -> &ResourceKind,
    row_budget: usize,
) -> Vec<KindGroup<T>> {
    let mut groups: Vec<KindGroup<T>> = Vec::new();
    for hit in ranked {
        match groups.last_mut() {
            Some(group) if group.kind == *kind(&hit) => {
                group.total = group.total.saturating_add(1);
                group.results.push(hit);
            }
            _ => groups.push(KindGroup {
                kind: kind(&hit).clone(),
                total: 1,
                results: vec![hit],
            }),
        }
    }
    if row_budget == 0
        || groups.len() < 2
        || groups.iter().map(|g| g.total).sum::<usize>() <= row_budget
    {
        return groups;
    }
    // Fair share, front to back: each group takes an equal cut of whatever
    // budget remains, and what it can't fill rolls forward (never backward,
    // or later headers push further down the card).
    let n = groups.len();
    let mut remaining = row_budget;
    for (i, group) in groups.iter_mut().enumerate() {
        let groups_left = n.saturating_sub(i).max(1);
        let share = remaining.checked_div(groups_left).unwrap_or(1).max(1);
        let keep = share.min(group.total);
        group.results.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    groups
}

/// The package id in full names exactly one app, so an exact id is as
/// strong as the name; a shared id prefix names a publisher and stays below
/// an exact name.
pub const PACKAGE_ID_WEIGHT: FieldWeight = FieldWeight::new(88);

/// The shared ranker over an app's fields. The package id in full, or its
/// last segment, identifies exactly one app and weighs as its name —
/// `hex-editor` typed for `so.tilo.hex-editor` is the app's identity and must
/// outrank an incidental substring. Category ranks below the id because it's
/// a bucket, not an identity. Multi-word queries may span fields, so `office
/// spreadsheet` can find an app whose category and description carry one
/// word each. An empty needle matches everything at `0`, so an empty palette
/// lists installed apps.
pub fn application_match_score(
    needle: &str,
    name: &str,
    package_id: &str,
    category: Option<&str>,
    description: Option<&str>,
) -> Option<i32> {
    let last_segment = package_id.rsplit('.').next().unwrap_or(package_id);
    rank_fields(
        needle,
        &[
            (name, FieldWeight::TITLE),
            (last_segment, FieldWeight::TITLE),
            (package_id, PACKAGE_ID_WEIGHT),
            (category.unwrap_or_default(), FieldWeight::METADATA),
            (description.unwrap_or_default(), FieldWeight::BODY),
        ],
    )
}

/// Product-owned ordering for the empty Applications palette. Search queries
/// continue to rank purely by match quality; this only prevents a fresh system
/// from presenting forty alphabetised tools as though they were equally likely
/// starting points. The first group is deliberately useful to curious
/// tinkerers without assuming they are developers.
pub fn empty_palette_presentation(package_id: &str) -> (&'static str, i32) {
    match package_id {
        "so.tilo.browser" => ("Start here", 90),
        "so.tilo.file-explorer" => ("Start here", 89),
        "so.tilo.notes" => ("Start here", 88),
        "so.tilo.terminal" => ("Start here", 87),
        "so.tilo.settings" => ("Start here", 86),
        "so.tilo.store" => ("Start here", 85),

        "so.tilo.calendar" => ("Everyday", 74),
        "so.tilo.mail" => ("Everyday", 73),
        "so.tilo.contacts" => ("Everyday", 72),
        "so.tilo.chat" => ("Everyday", 71),
        "so.tilo.calculator" => ("Everyday", 70),

        "so.tilo.word-processor" => ("Work", 59),
        "so.tilo.sheets" => ("Work", 58),
        "so.tilo.slides" => ("Work", 57),
        "so.tilo.pdf-viewer" => ("Work", 56),
        "so.tilo.typst-editor" => ("Work", 55),

        "so.tilo.music" => ("Media", 49),
        "so.tilo.tube" => ("Media", 48),
        "so.tilo.video-player" => ("Media", 47),
        "so.tilo.photos" => ("Media", 46),
        "so.tilo.camera" => ("Media", 45),
        "so.tilo.weather" => ("Everyday", 44),
        "so.tilo.maps" => ("Everyday", 43),
        "so.tilo.feeds" => ("Everyday", 42),
        "so.tilo.ebook" => ("Media", 41),
        "so.tilo.totp" => ("Everyday", 40),

        "so.tilo.monitor" | "so.tilo.disks" | "so.tilo.logs" | "so.tilo.firewall"
        | "so.tilo.vpn" | "so.tilo.remote" | "so.tilo.backups" | "so.tilo.imager"
        | "so.tilo.themes" => ("System tools", 25),
        _ => ("More apps", 10),
    }
}

/// How well `needle` names a record, by the shared ranker. Title first; the
/// identity is matched last and lowest — a file record's id contains its
/// whole path, so a directory-name match is a weaker signal than the title.
pub fn record_match_score(needle: &str, title: &str, id: &str) -> Option<i32> {
    rank_fields(
        needle,
        &[(title, FieldWeight::TITLE), (id, FieldWeight::IDENTITY)],
    )
}

/// The shared ranker over a window's names, every one of which names it.
pub fn window_match_score(needle: &str, names: &[&str]) -> Option<i32> {
    let fields: Vec<RankField<'_>> = names
        .iter()
        .map(|name| (*name, FieldWeight::TITLE))
        .collect();
    rank_fields(needle, &fields)
}

/// The shared ranker over an app-published action: its label names it, the
/// resource it acts on narrows it, its id and owning app identify it.
pub fn action_match_score(
    needle: &str,
    action_label: &str,
    resource_label: &str,
    action_id: &str,
    app_id: &str,
) -> Option<i32> {
    rank_fields(
        needle,
        &[
            (action_label, FieldWeight::TITLE),
            (resource_label, FieldWeight::SUBTITLE),
            (action_id, FieldWeight::IDENTITY),
            (app_id, FieldWeight::METADATA),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_title_outranks_prefix_across_kinds() {
        let exact = lexical_strength("sheets", &ResourceKind::Note, "Sheets", None);
        let prefix = lexical_strength("sheets", &ResourceKind::Note, "Sheets budget", None);
        let subtitle = lexical_strength("sheets", &ResourceKind::Note, "Budget", Some("Sheets"));
        assert!(exact > prefix && prefix > subtitle && subtitle > 0.0);
    }

    #[test]
    fn launch_verb_does_not_weaken_an_application_title() {
        assert!(
            (lexical_strength("sheets", &ResourceKind::Application, "Open Sheets", None) - 4.0)
                .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn perfect_application_match_beats_an_exact_note_of_the_same_word() {
        let app = merged_strength(
            "sheets",
            &ResourceKind::Application,
            "Open Sheets",
            None,
            MAX_PROVIDER_SCORE,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        let note = merged_strength(
            "sheets",
            &ResourceKind::Note,
            "Sheets",
            None,
            MAX_PROVIDER_SCORE,
            MAX_PROVIDER_SCORE,
            0.0,
        );
        assert!(app > note);
    }

    #[test]
    fn groups_follow_their_best_member_then_kind_order() {
        let mut merged = vec![
            (("a", ResourceKind::Note), 1.0),
            (("b", ResourceKind::Application), 0.5),
            (("c", ResourceKind::Note), 3.0),
            (("d", ResourceKind::Action), 3.0),
        ];
        order_by_kind(&mut merged, |(_, kind)| kind);
        let order: Vec<&str> = merged.iter().map(|((id, _), _)| *id).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }

    #[test]
    fn row_budget_is_shared_between_groups() {
        let ranked: Vec<ResourceKind> = std::iter::repeat_n(ResourceKind::File, 40)
            .chain([ResourceKind::Application])
            .collect();
        let groups = group_by_kind(ranked, |kind| kind, 10);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].total, 40);
        assert_eq!(groups[0].results.len(), 5);
        assert_eq!(groups[1].results.len(), 1);
    }

    #[test]
    fn application_identity_matches_name_id_and_last_segment() {
        let name = application_match_score("sheets", "Sheets", "so.tilo.sheets", None, None);
        let segment =
            application_match_score("hex-editor", "Hex Editor", "so.tilo.hex-editor", None, None);
        assert_eq!(name, Some(MAX_PROVIDER_SCORE));
        assert_eq!(segment, Some(MAX_PROVIDER_SCORE));
        assert_eq!(
            application_match_score("zzz", "Sheets", "so.tilo.sheets", None, None),
            None
        );
    }

    #[test]
    fn oversized_queries_are_refused_before_routing() {
        assert!(query_input_is_bounded("sheets"));
        assert!(!query_input_is_bounded(
            &"x".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS + 1)
        ));
    }
}
