//! `qdoc` — the CRDT-native canonical document model and format-provider API.
//!
//! Layer 1 (generic): a file is an automerge document; this crate provides the
//! `FormatProvider` contract + registry. Layer 2 (standard schema): the rich-text
//! `Document` model that md/odt/docx map onto. See
//! `docs/superpowers/specs/2026-06-24-crdt-native-file-formats-design.md`.

pub mod order;

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
