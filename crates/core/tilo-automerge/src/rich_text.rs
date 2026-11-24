//! A rich text's paragraphs in their order, as a reader outside its app reads them.
//!
//! A rich text (`tilo-doc`'s layout) keeps its paragraphs in maps by id: its body, its page
//! header and footer, the body of each note and comment, and under a paragraph the ones it
//! holds (a table's cells). A paragraph's place among the others of its map is a key of
//! bytes: a base-256 fraction, so a paragraph put between two others takes a key between
//! theirs and no other key moves. Paragraphs read by their keys, and by their ids where two
//! people gave two paragraphs one key.
//!
//! That rule is written here once, below every crate that orders paragraphs by it: the
//! document's own model (`tilo-doc`, through `tilo-resource`'s order keys) and the reader of
//! a whole document as JSON ([`crate::json::read_document`]). The reader cannot ask the model:
//! `tilo-doc` is built on `tilo-resource` and that on `tilo-actions`, whose reads this reader
//! answers. So the names of the layout it goes by are here too, and `tilo-doc`'s schema takes
//! them from here.

use std::cmp::Ordering;

/// The key a rich text names its layout under.
pub const SCHEMA_KEY: &str = "schema";
/// What a rich text names its layout.
pub const SCHEMA_ID: &str = "tilo.richtext";
/// The body's paragraphs by id; also a note's and a comment's.
pub const BLOCKS: &str = "blocks";
/// The page header's paragraphs by id.
pub const HEADER: &str = "header";
/// The page footer's paragraphs by id.
pub const FOOTER: &str = "footer";
/// The notes by id, each with its paragraphs under [`BLOCKS`].
pub const NOTES: &str = "notes";
/// The comments and their replies by id, each with its paragraphs under [`BLOCKS`].
pub const COMMENTS: &str = "comments";
/// The paragraphs a paragraph holds, by id.
pub const CHILDREN: &str = "children";
/// A paragraph's place among the others of its map: a key of bytes.
pub const ORDER: &str = "order";
/// Where a paragraph read into a list says its id: the key it is kept under.
pub const ID: &str = "id";
/// A paragraph's kind: a paragraph, a heading, a list item, a table, a cell, a picture.
pub const KIND: &str = "kind";
/// The attributes a paragraph carries: a style's id, a heading's level, and a picture's content.
pub const ATTRS: &str = "attrs";
/// What a picture paragraph names its content by, under [`ATTRS`]: the hash its bytes read at.
pub const ATTR_BLOB: &str = "blob";
/// The content a rich text references: a map by hash, each with what the content is. The bytes
/// live in the workspace's content store, beside the record.
pub const BLOBS: &str = "blobs";

/// The digits of `key`, right-padded with the insignificant trailing zeros that
/// a shorter fraction implies, so two keys can be compared digit for digit.
fn digits(key: &[u8], width: usize) -> impl Iterator<Item = u8> + '_ {
    key.iter().copied().chain(std::iter::repeat(0)).take(width)
}

/// Compare two keys as base-256 fractions (missing/trailing digits are 0).
#[must_use]
pub fn key_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let width = a.len().max(b.len());
    digits(a, width).cmp(digits(b, width))
}

/// The order of two paragraphs of one map, each as its key and its id: by their keys as
/// fractions ([`key_cmp`]), then by their ids.
#[must_use]
pub fn paragraph_cmp(a: (&[u8], &str), b: (&[u8], &str)) -> Ordering {
    key_cmp(a.0, b.0).then_with(|| a.1.cmp(b.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::{Equal, Greater, Less};

    #[test]
    fn paragraphs_order_by_their_keys_as_fractions_then_by_their_ids() {
        // A key is a fraction: a shorter one is not smaller for being shorter, and trailing
        // zeros say nothing.
        assert_eq!(key_cmp(&[128], &[128, 0]), Equal);
        assert_eq!(key_cmp(&[128], &[128, 1]), Less);
        assert_eq!(key_cmp(&[129], &[128, 255]), Greater);
        assert_eq!(key_cmp(&[], &[0, 0]), Equal);
        // Two paragraphs given one key read by their ids.
        assert_eq!(paragraph_cmp((&[128], "b"), (&[128, 0], "a")), Greater);
        assert_eq!(paragraph_cmp((&[64], "b"), (&[128], "a")), Less);
        assert_eq!(paragraph_cmp((&[128], "a"), (&[128], "a")), Equal);
    }
}
