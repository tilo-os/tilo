//! Desktop search's per-provider field scores on the shared ranker; the
//! cross-provider merge and kind grouping live in [`ui::search::system`].

use ui::search::FieldWeight;
use ui::search::system::{RankField, provider_score, rank_fields};

/// An app's searchable fields: its name in every language and its identity,
/// its kind, and every other word it is known by — the keywords its
/// manifest declares (the generated registry knows them for every
/// first-party app), and the initials of its name.
pub fn application_fields(
    name: &str,
    package_id: &str,
    category: Option<&str>,
) -> Vec<(String, FieldWeight)> {
    #[cfg(test)]
    FIELD_BUILDS.with(|builds| builds.set(builds.get().saturating_add(1)));
    let keywords = crate::availability::keywords_for(package_id);
    ui::search::system::application_search_fields(
        name,
        package_id,
        category,
        crate::availability::names_for(package_id).iter().copied(),
        keywords.iter().copied(),
    )
}

#[cfg(test)]
thread_local! {
    /// Apps whose searchable text this thread has built.
    pub static FIELD_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Every row's rank on the provider scale, on the source's own `ranker`: the
/// rows are indexed once and kept while they stay the same, so a keystroke
/// only searches.
pub fn rank_rows(
    ranker: &ui::search::ListIndex,
    needle: &str,
    rows: &[Vec<(String, FieldWeight)>],
) -> Vec<Option<i32>> {
    ranker
        .ranks(
            needle,
            rows.iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        )
        .into_iter()
        .map(|rank| rank.map(provider_score))
        .collect()
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

/// The shared ranker over an app-published action, by
/// [`ui::search::system::action_search_fields`].
pub fn action_match_score(
    needle: &str,
    action_label: &str,
    keywords: &[String],
    resource_label: &str,
) -> Option<i32> {
    ui::search::system::rank_action_fields(
        needle,
        &ui::search::system::action_search_fields(action_label, keywords, resource_label),
    )
}

/// [`action_match_score`] for every action of `actions` at once, row for row, each given as
/// its label, its other words and the label of what it acts on: one index over the list, so
/// a session's hundreds of published actions are ranked as one list, never one index each.
pub fn action_match_scores<'a>(
    needle: &str,
    actions: impl IntoIterator<Item = (&'a str, &'a [String], &'a str)>,
) -> Vec<Option<i32>> {
    ui::search::system::rank_actions_fields(
        needle,
        actions
            .into_iter()
            .map(|(label, keywords, resource_label)| {
                ui::search::system::action_search_fields(label, keywords, resource_label)
            }),
    )
}

/// The shared ranker over an action on one item of a record: the item's
/// name finds it, and its actions come with it.
pub fn item_action_match_score(needle: &str, item_label: &str) -> Option<i32> {
    rank_fields(needle, &[(item_label, FieldWeight::TITLE)])
}

/// [`item_action_match_score`] for every item of `labels` at once, row for row: one index
/// over the list.
pub fn item_action_match_scores<'a>(
    needle: &str,
    labels: impl IntoIterator<Item = &'a str>,
) -> Vec<Option<i32>> {
    ui::search::ListIndex::default()
        .ranks(
            needle,
            labels
                .into_iter()
                .map(|label| [(label, FieldWeight::TITLE)]),
        )
        .into_iter()
        .map(|rank| rank.map(provider_score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::system::MAX_PROVIDER_SCORE;

    /// The package id in full, or its last segment, identifies exactly one
    /// app and weighs as its name: `hex-editor` typed for
    /// `so.tilo.hex-editor` must outrank an incidental substring.
    #[test]
    fn application_identity_matches_name_id_and_last_segment() {
        let ranker =
            ui::search::ListIndex::default().with_ranks(ui::search::system::application_ranks);
        let rows = [
            application_fields("Sheets", "so.tilo.sheets", None),
            application_fields("Hex Editor", "so.tilo.hex-editor", None),
        ];
        assert_eq!(
            rank_rows(&ranker, "sheets", &rows)[0],
            Some(MAX_PROVIDER_SCORE)
        );
        assert_eq!(
            rank_rows(&ranker, "hex-editor", &rows)[1],
            Some(MAX_PROVIDER_SCORE)
        );
        assert_eq!(rank_rows(&ranker, "zzz", &rows)[0], None);
    }

    /// An app row is found by a word that names it begun — its name, its
    /// kind, a word it is known by — never by a near miss or by a word it
    /// appears inside: "notes" is Notes, not Nodes or Footnotes, and not the
    /// tenth of the catalogue whose blurb mentions notes.
    #[test]
    fn an_app_is_found_by_a_name_begun_never_by_a_near_miss() {
        let ranker =
            ui::search::ListIndex::default().with_ranks(ui::search::system::application_ranks);
        let rows = [
            application_fields("Notes", "so.tilo.notes", Some("Office")),
            application_fields("Nodes", "so.tilo.nodes", None),
            application_fields("Footnotes", "so.tilo.footnotes", None),
            application_fields("Notepad", "so.tilo.notepad", None),
        ];
        let ranks = rank_rows(&ranker, "notes", &rows);
        assert!(ranks[0].is_some(), "the app called Notes");
        assert_eq!(ranks[1], None, "a name one typo away is not its name");
        assert_eq!(ranks[2], None, "nor is the word inside another");
        assert_eq!(ranks[3], None, "nor is one that begins with less typed");
        let begun = rank_rows(
            &ranker,
            "note",
            &[application_fields("Notepad", "so.tilo.notepad", None)],
        )[0];
        assert!(begun.is_some(), "a name begun is: Note for Notepad");
    }

    /// Words of the appearance setting's rows, as the settings service
    /// publishes them: its name and its options' names in both languages.
    const APPEARANCE_STYLE: [&str; 7] = [
        "theme",
        "light mode",
        "dark mode",
        "thema",
        "hell",
        "hellmodus",
        "helligkeit",
    ];

    /// Typing a word that merely begins like another finds neither the theme
    /// rows nor Brightness, though each holds a word of that beginning: a
    /// typo is a whole word mistyped, and a word begun is the row's own name
    /// from its start.
    #[test]
    fn a_word_merely_beginning_alike_does_not_find_the_setting_rows() {
        let words: Vec<String> = APPEARANCE_STYLE.map(str::to_owned).to_vec();
        let rows = [
            ("Light mode", words.as_slice(), "Theme"),
            ("Dark mode", words.as_slice(), "Theme"),
            ("Brightness", words.as_slice(), "Display"),
        ];
        let scores = |needle: &str| {
            action_match_scores(
                needle,
                rows.iter().map(|(label, words, on)| (*label, *words, *on)),
            )
        };
        assert_eq!(scores("hello"), [None, None, None]);
        assert!(scores("hellmodus")[0].is_some(), "the German name of it");
        assert!(scores("theme")[0].is_some() && scores("theme")[1].is_some());
    }

    /// A row found by a prefix of its name outranks one found only by a
    /// typo of its name: what a person is typing leads what they remember
    /// wrongly.
    #[test]
    fn a_prefix_match_outranks_a_typo() {
        let rows = [
            ("Bookends", Vec::new(), "Desktop"),
            ("Books", Vec::new(), "Desktop"),
        ];
        let scores = action_match_scores(
            "booke",
            rows.iter()
                .map(|(label, words, on)| (*label, words.as_slice(), *on)),
        );
        let (prefix, typo) = (
            scores[0].expect("Bookends begun"),
            scores[1].expect("Books mistyped"),
        );
        assert!(prefix > typo, "{prefix} > {typo}");
        assert_eq!(prefix, MAX_PROVIDER_SCORE, "a name begun is a perfect hit");
    }
}
