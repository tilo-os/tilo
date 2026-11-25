//! What the reader answers without a file: colours, dates and requests.

use super::{Error, colour_of, import, parse_colour, parse_pdf_date, pdf_date};

#[test]
fn a_colour_round_trips_between_its_text_and_its_channels() {
    let colour = parse_colour("#12c863");
    assert_eq!(
        (colour.r, colour.g, colour.b, colour.a),
        (0x12, 0xc8, 0x63, 255)
    );
    assert_eq!(colour_of(colour), "#12c863");
    let faded = parse_colour("#12c86340");
    assert_eq!(faded.a, 0x40);
    // A colour nothing reads is the reader's ink, never a trap.
    assert_eq!(parse_colour("yellow"), parse_colour("#000000"));
}

#[test]
fn a_pdf_date_round_trips_through_the_reader_s_milliseconds() {
    let ms = parse_pdf_date("D:20260102030405Z").expect("a date");
    assert_eq!(pdf_date(ms), "D:20260102030405Z");
    assert_eq!(pdf_date(-1), "");
}

#[test]
fn a_file_that_is_no_pdf_or_image_is_refused_with_a_reason() {
    // A host with no engine at all refuses it as well, saying so; either way it is named.
    let error = import(b"no pdf here").expect_err("not an image either");
    assert!(
        matches!(error, Error::Unreadable(_) | Error::Unavailable(_)),
        "{error}"
    );
}
