use cosmic_text::{Attrs, Family, FontSystem, Metrics, Style, Weight, fontdb};

use super::*;
use crate::font_catalog::faces::{caps, only_distinguishes_il};
use crate::text_render::{TextStyle, UI_HYPERLEGIBLE_FAMILY, UI_SANS_FAMILY};

const BASE: Metrics = Metrics {
    font_size: 14.0,
    line_height: 20.0,
};

fn system() -> FontSystem {
    crate::font_catalog::fixture_font_system()
}

fn face_caps(fs: &FontSystem, family: &str, weight: u16, style: Style) -> Arc<FaceCaps> {
    let id = fs
        .db()
        .query(&fontdb::Query {
            families: &[Family::Name(family)],
            weight: Weight(weight),
            style,
            ..fontdb::Query::default()
        })
        .unwrap();
    caps(fs.db(), id).unwrap()
}

fn family<'a>(attrs: &'a Attrs<'_>) -> &'a str {
    match attrs.family {
        Family::Name(name) => name,
        _ => "",
    }
}

fn features(attrs: &Attrs<'_>) -> Vec<[u8; 4]> {
    attrs
        .font_features
        .features
        .iter()
        .map(|feature| *feature.tag.as_bytes())
        .collect()
}

fn plan_text<'t>(
    fs: &FontSystem,
    text: &'t str,
    attrs: Attrs<'static>,
    kind: TextKind,
) -> Vec<(&'t str, Attrs<'static>)> {
    plan(
        fs.db(),
        [(text, attrs)],
        BASE,
        kind,
        Optical::default(),
        false,
    )
}

fn ui(weight: u16) -> Attrs<'static> {
    Attrs::new()
        .family(Family::Name(UI_SANS_FAMILY))
        .weight(Weight(weight))
}

#[test]
fn latin_and_ascii_stay_one_span_in_the_ui_face() {
    let fs = system();
    let spans = plan_text(&fs, "Quarterly report, 2026", ui(400), TextKind::Prose);
    assert_eq!(spans.len(), 1);
    assert_eq!(family(&spans[0].1), UI_SANS_FAMILY);
    assert!(spans[0].1.metrics_opt.is_none());
    let spans = plan_text(&fs, "Café naïve Größe", ui(400), TextKind::Prose);
    assert_eq!(spans.len(), 1, "Latin-1 letters are the UI face's own");
}

#[test]
fn cyrillic_words_are_set_in_onest_at_the_ui_x_height() {
    let fs = system();
    let spans = plan_text(&fs, "Hello «Привет, мир», Alex", ui(400), TextKind::Prose);
    let texts: Vec<&str> = spans.iter().map(|(t, _)| *t).collect();
    assert_eq!(texts, ["Hello ", "«Привет, мир»,", " Alex"]);
    let (_, cyrillic) = &spans[1];
    assert_eq!(family(cyrillic), "Onest");
    assert!(features(cyrillic).contains(b"ss02"));
    // Regular maps to the Onest weight whose stems match the UI face's.
    assert_eq!(cyrillic.weight, Weight(390));
    let metrics = Metrics::from(cyrillic.metrics_opt.unwrap());
    assert_eq!(
        metrics.line_height, BASE.line_height,
        "leading never changes"
    );
    let primary = face_caps(&fs, UI_SANS_FAMILY, 400, Style::Normal);
    let onest = face_caps(&fs, "Onest", 390, Style::Normal);
    let ui_x = BASE.font_size * primary.x_height_em(400.0).unwrap();
    let onest_x = metrics.font_size * onest.x_height_em(390.0).unwrap();
    assert!((ui_x - onest_x).abs() < 0.01, "{ui_x} vs {onest_x}");
    assert!(
        metrics.font_size < BASE.font_size,
        "Onest's x-height is taller"
    );
}

#[test]
fn greek_and_vietnamese_are_the_ui_face_s_own_in_every_weight_and_style() {
    let fs = system();
    for text in [
        "Γεια σου κόσμε, Αρχεία · Ρυθμίσεις",
        "Tiếng Việt có dấu, Đồng ý",
        "Report Αναφορά Báo cáo Größe",
    ] {
        for weight in [400, 500, 600, 700] {
            for style in [Style::Normal, Style::Italic] {
                let spans = plan_text(&fs, text, ui(weight).style(style), TextKind::Prose);
                assert_eq!(spans.len(), 1, "{text} {weight} {style:?}");
                assert_eq!(family(&spans[0].1), UI_SANS_FAMILY, "{text}");
                assert!(spans[0].1.metrics_opt.is_none(), "{text}: own size");
            }
        }
    }
    let ui_face = face_caps(&fs, UI_SANS_FAMILY, 400, Style::Italic);
    assert!("ΆΰώẪỹ₫".chars().all(|c| ui_face.covers(c)));
}

#[test]
fn italic_cyrillic_keeps_a_true_italic_from_noto_sans() {
    let fs = system();
    let attrs = ui(400).style(Style::Italic);
    let spans = plan_text(&fs, "Привет Γεια", attrs, TextKind::Prose);
    let set: Vec<(&str, &str)> = spans.iter().map(|(t, a)| (*t, family(a))).collect();
    assert_eq!(set, [("Привет", "Noto Sans"), (" Γεια", UI_SANS_FAMILY)]);
    let (_, cyrillic) = &spans[0];
    assert!(!features(cyrillic).contains(b"ss02"));
    assert!(cyrillic.metrics_opt.is_some(), "still x-height matched");
}

#[test]
fn a_face_without_vietnamese_sends_whole_words_to_noto_sans() {
    let fs = system();
    let attrs = Attrs::new().family(Family::Name(UI_HYPERLEGIBLE_FAMILY));
    let spans = plan_text(&fs, "Tiếng Việt có dấu", attrs, TextKind::Prose);
    let set: Vec<(&str, &str)> = spans.iter().map(|(t, a)| (*t, family(a))).collect();
    // A word is never split between faces; one the UI face covers stays in it.
    assert_eq!(
        set,
        [
            ("Tiếng Việt", "Noto Sans"),
            (" có ", UI_HYPERLEGIBLE_FAMILY),
            ("dấu", "Noto Sans")
        ]
    );
}

#[test]
fn hyperlegible_gets_the_same_fallbacks_matched_to_its_own_x_height() {
    let fs = system();
    let attrs = Attrs::new().family(Family::Name(UI_HYPERLEGIBLE_FAMILY));
    let spans = plan_text(&fs, "Atkinson Привет", attrs.clone(), TextKind::Prose);
    assert_eq!(spans.len(), 2);
    assert_eq!(family(&spans[0].1), UI_HYPERLEGIBLE_FAMILY);
    let (_, cyrillic) = &spans[1];
    assert_eq!(family(cyrillic), "Onest");
    let primary = face_caps(&fs, UI_HYPERLEGIBLE_FAMILY, 400, Style::Normal);
    let onest = face_caps(&fs, "Onest", 390, Style::Normal);
    let size = Metrics::from(cyrillic.metrics_opt.unwrap()).font_size;
    let expected =
        BASE.font_size * primary.x_height_em(400.0).unwrap() / onest.x_height_em(390.0).unwrap();
    assert!((size - expected).abs() < 0.01);
    // Hyperlegible has no Greek: Noto Sans sets it, at Hyperlegible's x-height.
    let spans = plan_text(&fs, "Atkinson Γεια", attrs, TextKind::Prose);
    let (_, greek) = &spans[1];
    assert_eq!(family(greek), "Noto Sans");
    let noto = face_caps(&fs, "Noto Sans", 400, Style::Normal);
    let size = Metrics::from(greek.metrics_opt.unwrap()).font_size;
    let expected =
        BASE.font_size * primary.x_height_em(400.0).unwrap() / noto.x_height_em(400.0).unwrap();
    assert!((size - expected).abs() < 0.01);
}

#[test]
fn monospace_and_unmatched_scripts_are_left_to_the_shaper() {
    let fs = system();
    let mono = Attrs::new().family(Family::Monospace);
    assert_eq!(plan_text(&fs, "Привет мир", mono, TextKind::Prose).len(), 1);
    let spans = plan_text(&fs, "مرحبا 東京 שלום", ui(400), TextKind::Prose);
    assert_eq!(spans.len(), 1);
    assert_eq!(family(&spans[0].1), UI_SANS_FAMILY);
}

#[test]
fn kinds_resolve_to_what_each_face_supports() {
    let fs = system();
    let ui_face = face_caps(&fs, UI_SANS_FAMILY, 400, Style::Normal);
    assert!(ui_face.kind_features(TextKind::Prose).is_empty());
    assert_eq!(ui_face.kind_features(TextKind::Numeric), [*b"tnum"]);
    // The shipped Tilo Sans has a slashed zero and a distinct I and l.
    assert_eq!(
        ui_face.kind_features(TextKind::Secret),
        [*b"tnum", *b"zero", *b"cv01", *b"cv02", *b"ss13"]
    );
    assert_eq!(
        ui_face.kind_features(TextKind::Identifier),
        [*b"cv01", *b"cv02", *b"ss13"]
    );
    let noto = face_caps(&fs, "Noto Sans", 400, Style::Normal);
    assert_eq!(
        noto.kind_features(TextKind::Secret),
        [*b"tnum", *b"lnum", *b"zero"],
        "Noto's titling I and J (ss04) are not a distinct-I/l set"
    );
    let onest = face_caps(&fs, "Onest", 400, Style::Normal);
    assert!(onest.kind_features(TextKind::Identifier).is_empty());

    let spans = plan_text(&fs, "4 829 113", ui(400), TextKind::Numeric);
    assert_eq!(features(&spans[0].1), [*b"tnum"]);
    let spans = plan_text(&fs, "Код 0451", ui(400), TextKind::Secret);
    assert_eq!(family(&spans[0].1), "Onest");
    assert!(features(&spans[0].1).starts_with(&[*b"ss02", *b"tnum"]));
}

#[test]
fn explicit_features_win_and_stylistic_sets_stay_with_their_face() {
    let fs = system();
    let style = TextStyle::new(14.0)
        .with_family(crate::text_render::FontChoice::Named(UI_SANS_FAMILY.into()))
        .with_font_feature(crate::text_render::OpenTypeTag::new(*b"ss03"), 1)
        .with_kerning(false);
    let attrs = style.cosmic_attrs();
    let spans = plan(
        fs.db(),
        [("y Привет", attrs)],
        BASE,
        TextKind::Numeric,
        Optical::default(),
        false,
    );
    assert_eq!(features(&spans[0].1), [*b"tnum", *b"ss03", *b"kern"]);
    let fallback = features(&spans[1].1);
    assert!(fallback.contains(b"kern") && !fallback.contains(b"ss03"));
}

#[test]
fn a_distinct_il_set_changes_only_i_l_and_their_look_alikes() {
    assert!(only_distinguishes_il(&['I', 'l', '1', '|']));
    assert!(only_distinguishes_il(&['l', 'ł', 'ĺ']));
    assert!(
        only_distinguishes_il(&['I', 'Ĳ']),
        "the Dutch IJ on the serif I"
    );
    assert!(!only_distinguishes_il(&['I', 'J']), "titling alternates");
    assert!(
        !only_distinguishes_il(&['a', 'l', 't', 'y']),
        "a single-storey a set"
    );
    assert!(!only_distinguishes_il(&['1']), "a one alone is not I or l");
    assert!(
        only_distinguishes_il(&['I', 'Ɨ', 'І', 'Ї', 'Ι', 'Ί', 'Ἰ']),
        "the same capital I in Cyrillic and Greek, as a serif I set covers it"
    );
}

#[test]
fn a_catalog_face_with_a_serif_i_variant_is_used_for_it() {
    // Adwaita Sans' cv08 serifs I and the Cyrillic and Greek capitals that
    // share its shape.
    let fs = system();
    let face = face_caps(&fs, "Adwaita Sans", 400, Style::Normal);
    assert!(
        face.features_for(TextKind::Identifier, false)
            .contains(b"cv08")
    );
    assert!(face.features_for(TextKind::Url, false).contains(b"zero"));
    assert!(face.features_for(TextKind::Prose, false).is_empty());
}

#[test]
fn planned_paragraphs_keep_their_final_empty_line() {
    let mut fs = system();
    let attrs = ui(400);
    let spans = plan(
        fs.db(),
        [("Hi Привет\n", attrs.clone())],
        BASE,
        TextKind::Prose,
        Optical::default(),
        false,
    );
    assert!(spans.len() > 1);
    let mut buffer = Buffer::new(&mut fs, BASE);
    set_spans(&mut buffer, &spans, &attrs, None);
    let mut plain = Buffer::new(&mut fs, BASE);
    plain.set_text("Hi Привет\n", &attrs, Shaping::Advanced, None);
    assert_eq!(buffer.lines.len(), plain.lines.len());
}

/// Tilo Sans' full build with its held-back distinct I and l and slashed
/// zero (`scripts/toolkit_fixture.py` in tilo/tilo-sans).
const FEATURES_FIXTURE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/TiloSansFeaturesFixture-Regular.ttf"
));
const FEATURES_FIXTURE_FAMILY: &str = "Tilo Sans Features Fixture";

fn glyph_ids(fs: &mut FontSystem, text: &str, kind: TextKind) -> Vec<u16> {
    glyph_ids_with(fs, text, kind, false)
}

fn glyph_ids_with(fs: &mut FontSystem, text: &str, kind: TextKind, distinguish: bool) -> Vec<u16> {
    let attrs = Attrs::new().family(Family::Name(FEATURES_FIXTURE_FAMILY));
    let spans = plan(
        fs.db(),
        [(text, attrs.clone())],
        BASE,
        kind,
        Optical::default(),
        distinguish,
    );
    let mut buffer = Buffer::new(fs, BASE);
    set_spans(&mut buffer, &spans, &attrs, None);
    buffer.shape_until_scroll(fs, false);
    buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
        .collect()
}

#[test]
fn tilo_sans_held_back_forms_apply_to_secrets_and_identifiers_once_present() {
    let mut db = system().into_locale_and_db().1;
    db.load_font_data(FEATURES_FIXTURE.to_vec());
    let mut fs = crate::font_catalog::system_over("en-US".into(), db);
    let face = face_caps(&fs, FEATURES_FIXTURE_FAMILY, 400, Style::Normal);
    assert!(face.kind_features(TextKind::Prose).is_empty());
    assert_eq!(face.kind_features(TextKind::Numeric), [*b"tnum"]);
    // ss13 turns both I and l, cv01 the I and cv02 the l: all three are
    // "distinct I and l" sets, and the Dutch IJ under ss13 and cv01 does not
    // disqualify them.
    assert_eq!(
        face.kind_features(TextKind::Secret),
        [*b"tnum", *b"zero", *b"cv01", *b"cv02", *b"ss13"]
    );
    assert_eq!(
        face.kind_features(TextKind::Identifier),
        [*b"cv01", *b"cv02", *b"ss13"]
    );

    let prose = glyph_ids(&mut fs, "Il0", TextKind::Prose);
    let identifier = glyph_ids(&mut fs, "Il0", TextKind::Identifier);
    let secret = glyph_ids(&mut fs, "Il0", TextKind::Secret);
    assert_eq!(prose.len(), 3);
    assert_ne!(identifier[0], prose[0], "serif I");
    assert_ne!(identifier[1], prose[1], "tailed l");
    assert_eq!(identifier[2], prose[2], "identifiers keep the plain zero");
    assert_eq!(secret[..2], identifier[..2]);
    assert_ne!(secret[2], prose[2], "slashed zero");
}

fn fixture_system() -> FontSystem {
    let mut db = system().into_locale_and_db().1;
    db.load_font_data(FEATURES_FIXTURE.to_vec());
    crate::font_catalog::system_over("en-US".into(), db)
}

fn fixture() -> Attrs<'static> {
    Attrs::new().family(Family::Name(FEATURES_FIXTURE_FAMILY))
}

const IL: [[u8; 4]; 3] = [*b"cv01", *b"cv02", *b"ss13"];
const ZERO_IL: [[u8; 4]; 4] = [*b"zero", *b"cv01", *b"cv02", *b"ss13"];

#[test]
fn urls_add_the_zero_and_distinguishing_adds_every_form_to_every_kind() {
    let fs = fixture_system();
    let face = face_caps(&fs, FEATURES_FIXTURE_FAMILY, 400, Style::Normal);
    assert_eq!(face.kind_features(TextKind::Url), ZERO_IL);
    assert_eq!(face.kind_features(TextKind::Identifier), IL);
    assert!(face.kind_features(TextKind::Prose).is_empty());
    for kind in [TextKind::Prose, TextKind::Identifier, TextKind::Url] {
        assert_eq!(face.features_for(kind, true), ZERO_IL, "{kind:?}");
    }
    assert_eq!(
        face.features_for(TextKind::Numeric, true),
        [*b"tnum", *b"zero", *b"cv01", *b"cv02", *b"ss13"]
    );
    assert_eq!(
        face.features_for(TextKind::Secret, true),
        face.kind_features(TextKind::Secret)
    );
}

#[test]
fn hyperlegible_is_distinct_by_design_so_nothing_is_turned_on() {
    let fs = system();
    let face = face_caps(&fs, UI_HYPERLEGIBLE_FAMILY, 400, Style::Normal);
    for kind in [TextKind::Prose, TextKind::Identifier, TextKind::Url] {
        assert!(face.features_for(kind, true).is_empty(), "{kind:?}");
    }
    assert_eq!(face.features_for(TextKind::Secret, true), [*b"tnum"]);
}

fn planned(spans: &[(&str, Attrs<'_>)]) -> Vec<(String, Vec<[u8; 4]>)> {
    spans
        .iter()
        .map(|(text, attrs)| ((*text).to_owned(), features(attrs)))
        .collect()
}

fn expect(spans: &[(&str, &[[u8; 4]])]) -> Vec<(String, Vec<[u8; 4]>)> {
    spans
        .iter()
        .map(|(text, features)| ((*text).to_owned(), features.to_vec()))
        .collect()
}

#[test]
fn identifiers_in_prose_are_spans_of_their_own_kind() {
    let fs = fixture_system();
    let text = "Mail Il0@example.org or open https://paypa1.com/I0l, @lena_l1.";
    let spans = plan(
        fs.db(),
        [(text, fixture())],
        BASE,
        TextKind::Prose,
        Optical::default(),
        false,
    );
    assert_eq!(
        planned(&spans),
        expect(&[
            ("Mail ", &[]),
            ("Il0@example.org", &IL),
            (" or open ", &[]),
            ("https://paypa1.com/I0l", &ZERO_IL),
            (", ", &[]),
            ("@lena_l1", &IL),
            (".", &[]),
        ])
    );
    // Text that already says what it is is not split.
    let secret = plan(
        fs.db(),
        [(text, fixture())],
        BASE,
        TextKind::Secret,
        Optical::default(),
        false,
    );
    assert_eq!(secret.len(), 1);
}

#[test]
fn a_link_across_styled_runs_is_found_in_each() {
    let fs = fixture_system();
    let bold = fixture().weight(Weight::BOLD);
    let spans = plan(
        fs.db(),
        [("see https://exa", fixture()), ("mple.org/Il now", bold)],
        BASE,
        TextKind::Prose,
        Optical::default(),
        false,
    );
    assert_eq!(
        planned(&spans),
        expect(&[
            ("see ", &[]),
            ("https://exa", &ZERO_IL),
            ("mple.org/Il", &ZERO_IL),
            (" now", &[]),
        ])
    );
}

#[test]
fn a_run_that_says_its_kind_keeps_it() {
    let fs = fixture_system();
    let spans = plan_runs(
        fs.db(),
        [
            ("Thanks ", fixture(), None),
            ("Ilya Lebedev", fixture(), Some(TextKind::Identifier)),
        ],
        BASE,
        TextKind::Prose,
        Optical::default(),
        false,
    );
    assert_eq!(
        planned(&spans),
        expect(&[("Thanks ", &[]), ("Ilya Lebedev", &IL)])
    );
}

#[test]
fn distinguishing_sets_prose_apart_and_leaves_it_alone_when_off() {
    let mut fs = fixture_system();
    let plain = glyph_ids_with(&mut fs, "Il0", TextKind::Prose, false);
    let distinct = glyph_ids_with(&mut fs, "Il0", TextKind::Prose, true);
    let identifier = glyph_ids(&mut fs, "Il0", TextKind::Identifier);
    let url = glyph_ids(&mut fs, "Il0", TextKind::Url);
    // Every one of I, l and 0 changes; prose figures stay proportional,
    // so the zero is the proportional slashed one a link has.
    assert!(plain.iter().zip(&distinct).all(|(a, b)| a != b));
    assert_eq!(distinct, url);
    assert_eq!(
        url[..2],
        identifier[..2],
        "a link's I and l are an identifier's"
    );
    assert_ne!(url[2], identifier[2], "and its zero is slashed");
}

#[test]
fn the_setting_and_security_surfaces_are_per_thread_and_nest() {
    assert!(!distinguish_everywhere());
    security_surface(true);
    security_surface(true);
    security_surface(false);
    assert!(distinguish_everywhere());
    security_surface(false);
    assert!(!distinguish_everywhere());
    assert!(set_distinguish_everywhere(true));
    assert!(!set_distinguish_everywhere(true));
    assert!(
        std::thread::spawn(distinguish_everywhere)
            .join()
            .is_ok_and(|on| !on)
    );
    assert!(distinguish_everywhere());
    assert!(set_distinguish_everywhere(false));
}
