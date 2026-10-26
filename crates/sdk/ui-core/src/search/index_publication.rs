//! An app's search entries as the system's persistent search index takes
//! them.
//!
//! A running app publishes what its search finds (each entry's title,
//! secondary line and short fields, keyed by the stable resource id a hit
//! opens) to the session over its certified broker route, and the session
//! keeps it after the app closes, so the app's content stays findable
//! whether or not it runs. Only short metadata crosses: prose bodies stay in
//! the app, and a secret-bearing entry ([`IndexEntry::secret`]) carries what
//! the app chose to show of it (a name, a username, a site), never a secret.
//!
//! The entries, the deltas and the bounds they travel within are the broker
//! protocol's (`runtime_collab::search_index`), shared with the session that
//! indexes them; this module names them for the toolkit and maps a field's
//! published weight onto the ranker's.

pub use runtime_collab::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexState, IndexWeight, MAX_DELTA_BYTES, MAX_ENTRIES,
    MAX_FIELDS, MAX_ID_BYTES, MAX_TEXT_BYTES, Publisher,
};

/// The ranker's weight for a match in an entry field of `weight`.
#[must_use]
pub fn field_weight(weight: IndexWeight) -> super::FieldWeight {
    match weight {
        IndexWeight::Title => super::FieldWeight::TITLE,
        IndexWeight::Identity => super::FieldWeight::IDENTITY,
        IndexWeight::Subtitle => super::FieldWeight::SUBTITLE,
        IndexWeight::Metadata => super::FieldWeight::METADATA,
        IndexWeight::Body => super::FieldWeight::BODY,
    }
}

#[cfg(test)]
mod tests {
    use super::{IndexEntry, IndexWeight, field_weight};

    #[test]
    fn a_published_field_weight_is_the_rankers_weight_of_that_field() {
        assert_eq!(
            field_weight(IndexWeight::Title),
            super::super::FieldWeight::TITLE
        );
        assert_eq!(
            field_weight(IndexWeight::Identity),
            super::super::FieldWeight::IDENTITY
        );
        assert_eq!(
            field_weight(IndexWeight::Subtitle),
            super::super::FieldWeight::SUBTITLE
        );
        assert_eq!(
            field_weight(IndexWeight::Metadata),
            super::super::FieldWeight::METADATA
        );
        assert_eq!(
            field_weight(IndexWeight::Body),
            super::super::FieldWeight::BODY
        );
        let entry = IndexEntry {
            key: "note:1".into(),
            resource: "note:1".into(),
            action: None,
            title: "Dune".into(),
            subtitle: Some("Herbert".into()),
            group: None,
            icon: None,
            boost: 0,
            fields: vec![("arrakis".into(), IndexWeight::Metadata)],
            secret: false,
        };
        let searched: Vec<(&str, IndexWeight)> = entry.searchable().collect();
        assert_eq!(
            searched,
            [
                ("Dune", IndexWeight::Title),
                ("Herbert", IndexWeight::Subtitle),
                ("arrakis", IndexWeight::Metadata),
            ]
        );
    }
}
