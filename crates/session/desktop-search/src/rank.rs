//! Desktop search's per-provider field scores on the shared ranker; the
//! cross-provider merge and kind grouping live in [`ui::search::system`].

use ui::search::FieldWeight;
use ui::search::system::{RankField, application_fields, rank_fields};

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
    rank_fields(
        needle,
        &application_fields(name, package_id, category, description),
    )
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
    use ui::search::system::MAX_PROVIDER_SCORE;

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
}
