#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::fixture::{attachment_bytes, message_with_attachments};
use super::*;
use mail_parser::MimeHeaders as _;

/// The bodies of `split`, as [`join`] takes them.
fn bodies<'a>(split: &'a Split<'a>) -> Vec<(u64, &'a [u8])> {
    split
        .apart
        .iter()
        .map(|part| (part.at, part.body))
        .collect()
}

#[test]
fn a_large_attachment_is_kept_apart_and_put_back_byte_for_byte() {
    let message = message_with_attachments(
        "big@example.net",
        "Plans",
        &[("plans.pdf", 3 * 1024 * 1024), ("note.txt", 100)],
    );
    let split = split(&message);
    assert_eq!(split.apart.len(), 1, "only the large attachment goes apart");
    assert!(
        split.rest.len() < 8 * 1024,
        "{} bytes kept",
        split.rest.len()
    );
    assert_eq!(split.apart[0].size, 3 * 1024 * 1024);
    assert_eq!(join(&split.rest, &bodies(&split)).unwrap(), message);

    // What is kept still reads as the message: its words, and its attachments by name.
    let rest = MessageParser::default().parse(&split.rest).unwrap();
    assert_eq!(rest.subject(), Some("Plans"));
    assert!(rest.body_text(0).unwrap().contains("attached"));
    let names: Vec<_> = rest
        .attachments()
        .map(|part| part.attachment_name().unwrap())
        .collect();
    assert_eq!(names, ["plans.pdf", "note.txt"]);
    let joined = join(&split.rest, &bodies(&split)).unwrap();
    let whole = MessageParser::default().parse(&joined).unwrap();
    assert_eq!(
        whole.attachment(0).unwrap().contents(),
        attachment_bytes("plans.pdf", 3 * 1024 * 1024)
    );
}

#[test]
fn a_message_of_many_large_attachments_comes_apart_in_pieces() {
    let message = message_with_attachments(
        "huge@example.net",
        "Photos",
        &[
            ("a.jpg", 20 * 1024 * 1024),
            ("b.jpg", 600 * 1024),
            ("c.jpg", 12 * 1024 * 1024),
        ],
    );
    let split = split(&message);
    assert_eq!(split.apart.len(), 3);
    let pieces: Vec<usize> = split
        .apart
        .iter()
        .map(|part| part.pieces().count())
        .collect();
    assert_eq!(
        pieces,
        [4, 1, 3],
        "base64 grows each by a third, in 8 MiB pieces"
    );
    assert!(
        split
            .apart
            .iter()
            .all(|part| part.pieces().all(|piece| piece.len() <= PIECE_BYTES))
    );
    assert_eq!(join(&split.rest, &bodies(&split)).unwrap(), message);
}

#[test]
fn some_parts_put_back_leave_the_others_where_they_now_go() {
    let message = message_with_attachments(
        "some@example.net",
        "Two",
        &[("a.bin", 400 * 1024), ("b.bin", 500 * 1024)],
    );
    let split = split(&message);
    let some = [
        (split.apart[0].at, Some(split.apart[0].body)),
        (split.apart[1].at, None),
    ];
    let (partial, left) = join_some(&split.rest, &some).unwrap();
    let at = left[1].unwrap();
    assert_eq!(left[0], None);
    assert_eq!(
        join(&partial, &[(at, split.apart[1].body)]).unwrap(),
        message,
        "the part left apart goes back where join_some says"
    );
}

#[test]
fn a_small_or_unreadable_message_is_kept_whole() {
    let small = message_with_attachments("small@example.net", "Small", &[("a.txt", 1024)]);
    let kept = split(&small);
    assert!(kept.apart.is_empty());
    assert_eq!(kept.rest, small);
    let plain = b"Subject: hi\r\n\r\nhello\r\n";
    assert_eq!(split(plain).rest, plain);
}

#[test]
fn an_mbox_separator_line_and_bare_newlines_still_come_apart() {
    let mut message = b"From - Tue Nov 03 10:00:00 2026\n".to_vec();
    let body = message_with_attachments("lf@example.net", "LF", &[("a.bin", 700 * 1024)]);
    message.extend(body.iter().filter(|byte| **byte != b'\r'));
    let split = split(&message);
    assert_eq!(split.apart.len(), 1);
    assert_eq!(join(&split.rest, &bodies(&split)).unwrap(), message);
}

#[test]
fn a_body_that_would_go_past_the_message_is_refused() {
    assert_eq!(join(b"abc", &[(4, b"x")]), None);
    assert_eq!(join(b"abc", &[(2, b"x"), (1, b"y")]), None);
    assert_eq!(join(b"abc", &[(1, b"x"), (3, b"y")]).unwrap(), b"axbcy");
}
