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
    plan(fs.db(), [(text, attrs)], BASE, kind)
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
    // Regular maps to the Onest weight whose stems match Instrument's.
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
fn greek_takes_geologica_at_the_weight_whose_stems_match_on_every_role_weight() {
    let fs = system();
    for (ui_weight, expected) in [(400, 215), (500, 348), (600, 481), (700, 614)] {
        let spans = plan_text(&fs, "Γεια σου κόσμε", ui(ui_weight), TextKind::Prose);
        assert_eq!(spans.len(), 1);
        let (_, greek) = &spans[0];
        assert_eq!(family(greek), "Geologica");
        assert_eq!(greek.weight, Weight(expected), "{ui_weight}");
        let size = Metrics::from(greek.metrics_opt.unwrap()).font_size;
        let geologica = face_caps(&fs, "Geologica", expected, Style::Normal);
        let primary = face_caps(&fs, UI_SANS_FAMILY, ui_weight, Style::Normal);
        let matched = size * geologica.x_height_em(f32::from(expected)).unwrap();
        let ui_x = BASE.font_size * primary.x_height_em(f32::from(ui_weight)).unwrap();
        assert!((matched - ui_x).abs() < 0.01, "{matched} vs {ui_x}");
    }
}

#[test]
fn italic_greek_and_cyrillic_keep_a_true_italic_from_noto_sans() {
    let fs = system();
    let attrs = ui(400).style(Style::Italic);
    let spans = plan_text(&fs, "Привет Γεια", attrs, TextKind::Prose);
    for (text, attrs) in &spans {
        assert_eq!(family(attrs), "Noto Sans", "{text}");
        assert!(!features(attrs).contains(b"ss02"));
        assert!(attrs.metrics_opt.is_some(), "still x-height matched");
    }
}

#[test]
fn vietnamese_words_the_ui_face_cannot_set_whole_go_to_noto_sans() {
    let fs = system();
    let spans = plan_text(&fs, "Tiếng Việt có dấu", ui(400), TextKind::Prose);
    let set: Vec<(&str, &str)> = spans.iter().map(|(t, a)| (*t, family(a))).collect();
    // A word is never split between faces; one the UI face covers stays in it.
    assert_eq!(
        set,
        [
            ("Tiếng Việt", "Noto Sans"),
            (" có ", UI_SANS_FAMILY),
            ("dấu", "Noto Sans")
        ]
    );
}

#[test]
fn hyperlegible_gets_the_same_fallbacks_matched_to_its_own_x_height() {
    let fs = system();
    let attrs = Attrs::new().family(Family::Name(UI_HYPERLEGIBLE_FAMILY));
    let spans = plan_text(&fs, "Atkinson Привет", attrs, TextKind::Prose);
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
    let instrument = face_caps(&fs, UI_SANS_FAMILY, 400, Style::Normal);
    assert!(instrument.kind_features(TextKind::Prose).is_empty());
    assert_eq!(instrument.kind_features(TextKind::Numeric), [*b"tnum"]);
    // No slashed zero and no distinct I/l in Instrument Sans: degrade quietly.
    assert_eq!(instrument.kind_features(TextKind::Secret), [*b"tnum"]);
    assert!(instrument.kind_features(TextKind::Identifier).is_empty());
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
    let spans = plan(fs.db(), [("y Привет", attrs)], BASE, TextKind::Numeric);
    assert_eq!(features(&spans[0].1), [*b"tnum", *b"ss03", *b"kern"]);
    let fallback = features(&spans[1].1);
    assert!(fallback.contains(b"kern") && !fallback.contains(b"ss03"));
}

#[test]
fn a_distinct_il_set_changes_only_i_l_and_their_look_alikes() {
    assert!(only_distinguishes_il(&['I', 'l', '1', '|']));
    assert!(only_distinguishes_il(&['l', 'ł', 'ĺ']));
    assert!(!only_distinguishes_il(&['I', 'J']), "titling alternates");
    assert!(
        !only_distinguishes_il(&['a', 'l', 't', 'y']),
        "a single-storey a set"
    );
    assert!(!only_distinguishes_il(&['1']), "a one alone is not I or l");
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
    );
    assert!(spans.len() > 1);
    let mut buffer = Buffer::new(&mut fs, BASE);
    set_spans(&mut buffer, &spans, &attrs, None);
    let mut plain = Buffer::new(&mut fs, BASE);
    plain.set_text("Hi Привет\n", &attrs, Shaping::Advanced, None);
    assert_eq!(buffer.lines.len(), plain.lines.len());
}
