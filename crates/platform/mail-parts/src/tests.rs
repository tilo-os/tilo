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

#[test]
fn a_body_kept_apart_decodes_a_piece_at_a_time_to_its_file() {
    let size = 700 * 1024 + 5;
    let message = message_with_attachments("decode@example.net", "Scan", &[("scan.pdf", size)]);
    let split = split(&message);
    let part = &split.apart[0];
    // Pieces of every awkward length: cut mid-line and mid-group.
    for step in [1_000_003, 77, 4096, 3] {
        let mut decoder = Decoder::of_part(&split.rest, part.at).unwrap();
        let mut file = Vec::new();
        for piece in part.body.chunks(step) {
            file.extend(decoder.push(piece).unwrap());
        }
        file.extend(decoder.finish().unwrap());
        assert!(
            file == fixture::attachment_bytes("scan.pdf", size),
            "{step}: {} of {size} bytes",
            file.len()
        );
    }
}

#[test]
fn quoted_printable_decodes_across_pieces() {
    let body = "caf=C3=A9 and a long line that wraps=\r\n here\r\nnext =3D line\r\n";
    let message = format!(
        "From: a@example.net\r\nMessage-ID: <qp@example.net>\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{body}"
    );
    let at = message.find(body).unwrap() as u64;
    let mut whole = Vec::new();
    let mut decoder = Decoder::of_part(message.as_bytes(), at).unwrap();
    for piece in body.as_bytes().chunks(5) {
        whole.extend(decoder.push(piece).unwrap());
    }
    whole.extend(decoder.finish().unwrap());
    assert_eq!(
        String::from_utf8(whole).unwrap(),
        "café and a long line that wraps here\r\nnext = line\r\n"
    );
}

/// A server streaming a part's file is told its encoding by name, never the
/// message: the decoder it makes from that name decodes the same file.
#[test]
fn a_decoder_made_from_its_encodings_name_decodes_the_same_file() {
    let size = 300 * 1024 + 1;
    let message = message_with_attachments("named@example.net", "Scan", &[("scan.pdf", size)]);
    let split = split(&message);
    let part = &split.apart[0];
    let of_part = Decoder::of_part(&split.rest, part.at).unwrap();
    assert_eq!(of_part.encoding(), "base64");
    let mut decoder = Decoder::named(of_part.encoding()).unwrap();
    let mut file = Vec::new();
    for piece in part.body.chunks(4099) {
        file.extend(decoder.push(piece).unwrap());
    }
    file.extend(decoder.finish().unwrap());
    assert!(
        file == attachment_bytes("scan.pdf", size),
        "{} bytes",
        file.len()
    );
    for name in ["base64", "quoted-printable", "binary"] {
        assert_eq!(Decoder::named(name).unwrap().encoding(), name);
    }
    assert!(Decoder::named("uuencode").is_none());
}
