use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, TextEncoding};
use tilo_automerge::mark_text_author;

#[test]
fn unicode_authorship_uses_document_offsets_and_does_not_expand() {
    for (encoding, prefix_len, authored_len) in [
        (TextEncoding::UnicodeCodePoint, 1, 2),
        (TextEncoding::Utf8CodeUnit, 2, 6),
        (TextEncoding::Utf16CodeUnit, 1, 3),
    ] {
        let mut doc = AutoCommit::new_with_encoding(encoding);
        let text = doc.put_object(ROOT, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "é").unwrap();
        doc.splice_text(&text, prefix_len, 0, "ö😀").unwrap();
        mark_text_author(&mut doc, &text, prefix_len, "ö😀", "alice").unwrap();
        doc.splice_text(&text, prefix_len + authored_len, 0, "b")
            .unwrap();
        mark_text_author(&mut doc, &text, prefix_len + authored_len, "b", "bob").unwrap();
        assert_eq!(doc.text(&text).unwrap(), "éö😀b");
        let marks = doc.marks(&text).unwrap();
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].name(), "author");
        assert_eq!(marks[0].value().as_str(), Some("alice"));
        assert_eq!(
            (marks[0].start, marks[0].end),
            (prefix_len, prefix_len + authored_len)
        );
        assert_eq!(marks[1].value().as_str(), Some("bob"));
        assert_eq!(
            (marks[1].start, marks[1].end),
            (prefix_len + authored_len, prefix_len + authored_len + 1)
        );
        let pending = doc.pending_ops();
        mark_text_author(&mut doc, &text, 0, "", "alice").unwrap();
        assert_eq!(doc.pending_ops(), pending);
    }
}
