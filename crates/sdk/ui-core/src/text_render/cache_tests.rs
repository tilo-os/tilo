use super::*;

#[test]
fn colliding_text_and_style_keep_independent_values() {
    let plain = TextStyle::new(16.0);
    let tracked = plain.clone().with_letter_spacing(8.0);
    let first = CacheQuery::new("iii", &plain);
    let mut text_collision = CacheQuery::new("WWW", &plain);
    text_collision.fingerprint = first.fingerprint;
    let mut style_collision = CacheQuery::new("iii", &tracked);
    style_collision.fingerprint = first.fingerprint;
    let mut cache = TextCache::new(4, 100_000);
    cache.insert(first.key(None, 0, 1.0), 1, 0);
    cache.insert(text_collision.key(None, 0, 1.0), 2, 0);
    cache.insert(style_collision.key(None, 0, 1.0), 3, 0);
    assert_eq!(cache.get(&first.key(None, 0, 1.0)), Some(&1));
    assert_eq!(cache.get(&text_collision.key(None, 0, 1.0)), Some(&2));
    assert_eq!(cache.get(&style_collision.key(None, 0, 1.0)), Some(&3));
    assert_eq!(cache.entries, 3);
    assert_eq!(cache.clock.len(), 1);
    assert!(cache.get(&first.key(None, 1, 1.0)).is_none());
    assert!(cache.get(&first.key(None, 0, 2.0)).is_none());
    assert!(cache.get(&first.key(Some(100.0), 0, 1.0)).is_none());
}

#[test]
fn clock_protects_reused_text_during_cold_insert_churn() {
    let style = TextStyle::new(16.0);
    let hot = CacheQuery::new("hot", &style);
    let mut cache = TextCache::new(3, 100_000);
    cache.insert(hot.key(None, 0, 1.0), 7, 0);
    for index in 0..100 {
        assert_eq!(cache.get(&hot.key(None, 0, 1.0)), Some(&7));
        let text = format!("cold {index}");
        let cold = CacheQuery::new(&text, &style);
        cache.insert(cold.key(None, 0, 1.0), index, 0);
        assert!(cache.entries <= 3);
        assert!(cache.clock.len() <= 3);
        assert!(cache.buckets.len() <= 3);
    }
}

#[test]
fn payload_budget_rejects_oversize_without_disturbing_residents() {
    let style = TextStyle::new(16.0);
    let hot = CacheQuery::new("hot", &style);
    let huge = CacheQuery::new("huge", &style);
    let mut cache = TextCache::new(10, 1024);
    cache.insert(hot.key(None, 0, 1.0), 7, 0);
    let before = cache.bytes;
    cache.insert(huge.key(None, 0, 1.0), 8, 1025);
    assert_eq!(cache.get(&hot.key(None, 0, 1.0)), Some(&7));
    assert!(cache.get(&huge.key(None, 0, 1.0)).is_none());
    assert_eq!(cache.bytes, before);
    let large_text = "a".repeat(2048);
    cache.insert(CacheQuery::new(&large_text, &style).key(None, 0, 1.0), 9, 0);
    assert_eq!(cache.bytes, before);
}

#[test]
fn byte_eviction_and_clear_remove_collision_bucket_metadata() {
    let style = TextStyle::new(16.0);
    let first = CacheQuery::new("first", &style);
    let mut sibling = CacheQuery::new("other", &style);
    sibling.fingerprint = first.fingerprint;
    let third = CacheQuery::new("third", &style);
    let mut cache = TextCache::new(2, 100_000);
    cache.insert(first.key(None, 0, 1.0), 1, 0);
    cache.insert(sibling.key(None, 0, 1.0), 2, 0);
    cache.insert(third.key(None, 0, 1.0), 3, 0);
    assert!(cache.get(&first.key(None, 0, 1.0)).is_none());
    assert!(cache.get(&sibling.key(None, 0, 1.0)).is_none());
    assert_eq!(cache.get(&third.key(None, 0, 1.0)), Some(&3));
    assert_eq!(cache.entries, 1);
    assert_eq!(cache.clock.len(), 1);
    assert_eq!(cache.buckets.len(), 1);
    cache.clear();
    assert_eq!(cache.entries, 0);
    assert_eq!(cache.bytes, 0);
    assert!(cache.clock.is_empty());
    assert!(cache.buckets.is_empty());
    // Admit by bytes, not merely by the entry limit.
    let mut cache = TextCache::new(100, 1024);
    for index in 0..50 {
        let text = format!("payload {index}");
        cache.insert(CacheQuery::new(&text, &style).key(None, 0, 1.0), index, 500);
        assert!(cache.bytes <= 1024);
        assert_eq!(cache.entries, 1);
    }
}

fn reset_work() {
    HASHED_BYTES.with(|count| count.set(0));
    COMPARED_BYTES.with(|count| count.set(0));
}

fn work() -> (usize, usize) {
    (
        HASHED_BYTES.with(std::cell::Cell::get),
        COMPARED_BYTES.with(std::cell::Cell::get),
    )
}

#[test]
fn prepared_warm_measure_and_draw_do_not_scan_payload() {
    let mut renderer = super::super::TextRenderer::new();
    let style = TextStyle::new(16.0)
        .with_kerning(false)
        .with_font_variation(super::super::OpenTypeTag::WIDTH, 90.0);
    let input = PreparedText::new("Hello Καλημέρα مرحبا 👩‍💻", style);
    let size = renderer.measure_prepared(&input, None);
    let mut scene = vello::Scene::new();
    reset_work();
    for index in 0..10 {
        assert_eq!(
            renderer.measure_prepared(&input.clone(), Some(1000.0)),
            size
        );
        renderer.draw_prepared(
            &mut scene,
            &input,
            vello::peniko::Color::WHITE,
            vello::kurbo::Affine::translate((f64::from(index), 20.0)),
            Some(1000.0),
        );
    }
    assert_eq!(
        work(),
        (0, 0),
        "retained hits must not hash or compare payloads"
    );
}

#[test]
fn borrowed_operation_hashes_once_and_adopts_the_first_exact_match() {
    let mut renderer = super::super::TextRenderer::new();
    let style = TextStyle::new(16.0);
    let text = "Repeated label";
    renderer.measure_style(text, Some(1000.0), &style);
    reset_work();
    renderer.measure_style(text, Some(1000.0), &style);
    assert_eq!(work(), (text.len(), text.len()));
}

#[test]
fn prepared_adopts_an_existing_borrowed_entry_for_future_calls() {
    let mut renderer = super::super::TextRenderer::new();
    let style = TextStyle::new(16.0);
    renderer.measure_style("borrowed first", None, &style);
    let input = PreparedText::new("borrowed first", style);
    renderer.measure_prepared(&input, None);
    reset_work();
    renderer.measure_prepared(&input, None);
    assert_eq!(
        work(),
        (0, 0),
        "a prepared handle must retain its exact-match resolution"
    );
}

#[test]
fn custom_hyphenator_identity_and_version_are_independent() {
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Policy(AtomicU64);
    impl super::super::Hyphenator for Policy {
        fn breaks(&self, _: &str) -> Vec<usize> {
            vec![]
        }
        fn cache_key(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }
    let policy = Arc::new(Policy(AtomicU64::new(7)));
    let input = PreparedText::new(
        "custom",
        TextStyle::new(16.0).with_hyphenator(policy.clone()),
    );
    let other = PreparedText::new(
        "custom",
        TextStyle::new(16.0).with_hyphenator(Arc::new(Policy(AtomicU64::new(7)))),
    );
    let mut cache = TextCache::new(10, 100_000);
    cache.insert(CacheQuery::prepared(&input).key(None, 0, 1.0), 1, 0);
    assert!(
        cache
            .get(&CacheQuery::prepared(&other).key(None, 0, 1.0))
            .is_none(),
        "same numeric version from a different algorithm must not alias"
    );
    policy.0.store(8, Ordering::Relaxed);
    assert!(
        cache
            .get(&CacheQuery::prepared(&input).key(None, 0, 1.0))
            .is_none(),
        "the cached Arc observes mutation but its captured version must not change"
    );
    cache.insert(CacheQuery::prepared(&input).key(None, 0, 1.0), 2, 0);
    assert_eq!(
        cache.get(&CacheQuery::prepared(&input).key(None, 0, 1.0)),
        Some(&2)
    );
}

#[test]
fn mutated_borrowed_storage_and_styles_cannot_reuse_stale_values() {
    let mut renderer = super::super::TextRenderer::new();
    let mut text = String::with_capacity(100);
    text.push_str("iii");
    let mut style = TextStyle::new(16.0);
    let narrow = renderer.measure_style(&text, None, &style);
    text.clear();
    text.push_str("WWW");
    let wide = renderer.measure_style(&text, None, &style);
    assert!(wide.0 > narrow.0);
    style.size = 32.0;
    let large = renderer.measure_style(&text, None, &style);
    assert!(large.0 > wide.0);
    let prepared = PreparedText::new(text.clone(), style.clone());
    assert_eq!(renderer.measure_prepared(&prepared, None), large);
    renderer.set_text_scale(2.0);
    assert!(renderer.measure_prepared(&prepared, None).0 > large.0);
    renderer.take_shape_stats();
    renderer.bump_generation();
    renderer.measure_prepared(&prepared, None);
    assert!(
        renderer.take_shape_stats().1 > 0,
        "font generation must reshape retained input"
    );
}

#[test]
fn glyph_capacity_budget_and_eviction_preserve_live_run_owners() {
    use super::super::{ShapedText, TextRenderer, glyph_run_bytes};
    let mut renderer = TextRenderer::new();
    let style = TextStyle::new(16.0);
    let live = renderer.shaped_runs("retained glyphs", None, &style);
    let first_glyph = live[0].glyphs[0].id;
    let mut roomy = (*live).clone();
    roomy[0].glyphs.reserve_exact(4096);
    roomy[0].normalized_coords.reserve_exact(4096);
    let roomy = Arc::new(roomy);
    let cost = glyph_run_bytes(&roomy);
    assert!(
        cost > 4096 * std::mem::size_of::<vello::Glyph>(),
        "charge capacity, including unused slots"
    );
    renderer.shape_cache = TextCache::new(1, 1024);
    let key = CacheQuery::new("roomy", &style);
    renderer.shape_cache.insert(
        key.key(None, 0, 1.0),
        ShapedText {
            runs: roomy.clone(),
            size: (1.0, 1.0),
        },
        cost,
    );
    assert_eq!(
        renderer.shape_cache.entries, 0,
        "oversized glyph vectors are not retained"
    );
    assert_eq!(Arc::strong_count(&roomy), 1);
    assert_eq!(live[0].glyphs[0].id, first_glyph);
    let small = renderer.shaped_runs("a", None, &style);
    assert_eq!(renderer.shape_cache.entries, 1);
    let second = renderer.shaped_runs("b", None, &style);
    assert_eq!(renderer.shape_cache.entries, 1);
    assert_eq!(
        Arc::strong_count(&small),
        1,
        "eviction drops only the cache's Arc"
    );
    assert!(renderer.shape_cache.bytes <= 1024);
    renderer.bump_generation();
    assert_eq!(renderer.shape_cache.bytes, 0);
    assert_eq!(Arc::strong_count(&second), 1);
    assert!(!small[0].glyphs.is_empty());
}

#[test]
fn exact_style_comparison_covers_every_field_even_with_one_fingerprint() {
    use super::super::{
        FontVariation, Fragmentation, HangingPunctuation, OpenTypeFeature, OpenTypeTag,
        ParagraphAlign, TextWeight, TextWrapMode,
    };
    let plain = TextStyle::new(16.0);
    let first = CacheQuery::new("style", &plain);
    let mut cache = TextCache::new(100, 100_000);
    cache.insert(first.key(None, 0, 1.0), 1, 0);
    let mut variants = Vec::new();
    macro_rules! changed {
        ($field:ident, $value:expr) => {{
            let mut style = plain.clone();
            style.$field = $value;
            variants.push(style);
        }};
    }
    changed!(size, 17.0);
    changed!(line_height, 1.5);
    changed!(weight, TextWeight::Bold);
    changed!(italic, true);
    changed!(family, FontChoice::Named("named".into()));
    changed!(hyphenation, Hyphenation::Auto("en".into()));
    changed!(letter_spacing, -0.0);
    changed!(
        font_features,
        vec![OpenTypeFeature {
            tag: OpenTypeTag::KERNING,
            value: 0
        }]
    );
    changed!(
        font_variations,
        vec![FontVariation {
            tag: OpenTypeTag::WIDTH,
            value: 90.0
        }]
    );
    changed!(optical_sizing, OpticalSizing::Value(16.0));
    changed!(align, ParagraphAlign::End);
    changed!(wrap_mode, TextWrapMode::WordOrGlyph);
    changed!(hanging_punctuation, HangingPunctuation::BOTH);
    changed!(
        fragmentation,
        Fragmentation {
            orphans: 3,
            widows: 4
        }
    );
    for style in &variants {
        let mut candidate = CacheQuery::new("style", style);
        candidate.fingerprint = first.fingerprint;
        assert!(
            cache.get(&candidate.key(None, 0, 1.0)).is_none(),
            "style must differ: {style:?}"
        );
    }
}

#[test]
fn promotion_accounts_for_caller_spare_capacity_without_evicting() {
    let plain = TextStyle::new(16.0).with_kerning(false);
    let first = CacheQuery::new("capacity", &plain);
    let mut roomy = plain.clone();
    roomy.font_features.reserve_exact(4096);
    let prepared = PreparedText::new("capacity", roomy);
    let mut cache = TextCache::new(4, 1024);
    cache.insert(first.key(None, 0, 1.0), 7, 0);
    let before = cache.bytes;
    assert_eq!(
        cache.get(&CacheQuery::prepared(&prepared).key(None, 0, 1.0)),
        Some(&7)
    );
    assert_eq!(
        cache.bytes, before,
        "oversized identity promotion must be declined"
    );
    assert_eq!(cache.entries, 1);
    cache.byte_limit = 100_000;
    assert_eq!(
        cache.get(&CacheQuery::prepared(&prepared).key(None, 0, 1.0)),
        Some(&7)
    );
    assert!(cache.bytes >= before + 4096 * std::mem::size_of::<super::super::OpenTypeFeature>());
    cache.clear();
    assert_eq!(cache.bytes, 0);
}

#[test]
fn prepared_and_borrowed_unicode_geometry_match_at_scaled_wrapped_widths() {
    use super::super::{GlyphRun, TextRenderer};
    fn geometry(runs: &[GlyphRun]) -> Vec<(u32, u32, u32)> {
        runs.iter()
            .flat_map(|run| {
                run.glyphs
                    .iter()
                    .map(|glyph| (glyph.id, glyph.x.to_bits(), glyph.y.to_bits()))
            })
            .collect()
    }
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(1.5);
    let text = "office e\u{301} Ελληνικά العربية 👩‍💻\nline two";
    let style = TextStyle::new(18.0)
        .with_letter_spacing(0.4)
        .with_kerning(false);
    let input = PreparedText::new(text, style.clone());
    let prepared_size = renderer.measure_prepared(&input, Some(100.0));
    let scaled = renderer.scaled_style(&style).into_owned();
    let prepared_runs = renderer.shaped_query(&CacheQuery::prepared(&input), Some(100.0), &scaled);
    renderer.bump_generation();
    let borrowed_size = renderer.measure_style(text, Some(100.0), &style);
    let borrowed_runs = renderer.shaped_runs(text, Some(100.0), &scaled);
    assert_eq!(prepared_size, borrowed_size);
    assert!(!prepared_runs.is_empty());
    assert_eq!(geometry(&prepared_runs), geometry(&borrowed_runs));
}

#[test]
fn over_budget_shape_is_built_only_once_per_public_operation() {
    let mut renderer = super::super::TextRenderer::new();
    renderer.shape_cache = TextCache::new(4096, 0);
    let input = PreparedText::new("uncached geometry", TextStyle::new(16.0));
    renderer.measure_prepared(&input, None);
    renderer.take_shape_stats();
    renderer.draw_prepared(
        &mut vello::Scene::new(),
        &input,
        vello::peniko::Color::WHITE,
        vello::kurbo::Affine::IDENTITY,
        Some(1000.0),
    );
    assert_eq!(
        renderer.take_shape_stats().1,
        1,
        "draw must carry rejected geometry through its width probe"
    );
    assert_eq!(renderer.shape_cache.entries, 0);
    renderer.measure_prepared(&input, Some(1000.0));
    assert_eq!(
        renderer.take_shape_stats().1,
        1,
        "measurement must not repeat a rejected warm within one call"
    );
    assert_eq!(renderer.shape_cache.entries, 0);
}

// Actual public Text callers, rather than helper-only cache identity assertions.
fn actual_text_measure(
    text: &mut crate::widget::Text,
    renderer: &mut super::super::TextRenderer,
) -> taffy::Size<f32> {
    crate::widget::Widget::measure(
        text,
        taffy::Size {
            width: None,
            height: None,
        },
        taffy::Size {
            width: taffy::AvailableSpace::MaxContent,
            height: taffy::AvailableSpace::MaxContent,
        },
        renderer,
    )
}
fn actual_text_draw(
    text: &mut crate::widget::Text,
    renderer: &mut super::super::TextRenderer,
    width: f64,
) -> vello::Scene {
    text.apply_layout_external(vello::kurbo::Rect::new(10.0, 20.0, 10.0 + width, 120.0));
    let mut scene = vello::Scene::new();
    crate::widget::Widget::draw(
        text,
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        renderer,
        &crate::theme::Theme::dark(),
        &crate::presence::PresenceCtx::empty(),
    );
    crate::widget::Widget::mark_drawn(text);
    scene
}
#[test]
fn text_widget_retained_input_actual_measure_and_repaint_do_not_scan() {
    let mut renderer = super::super::TextRenderer::new();
    let mut text = crate::widget::Text::new("Shared Καλημέρα مرحبا 👩‍💻 input").with_kerning(false);
    let expected = actual_text_measure(&mut text, &mut renderer);
    let first = actual_text_draw(&mut text, &mut renderer, 800.0);
    assert!(!first.encoding().resources.glyph_runs.is_empty());
    reset_work();
    for width in [801.0, 802.0, 803.0, 804.0] {
        assert_eq!(actual_text_measure(&mut text, &mut renderer), expected);
        let scene = actual_text_draw(&mut text, &mut renderer, width);
        assert!(
            exact_scene_encoding_eq(&first, &scene),
            "same admitted input fits both boxes with exactly the original geometry"
        );
    }
    assert_eq!(
        work(),
        (0, 0),
        "actual retained Text measure and repainted draw must share one immutable input; helper-only PreparedText reuse is insufficient"
    );
}
#[test]
fn text_widget_retained_input_actual_font_epoch_invalidates_cached_scene() {
    let mut renderer = super::super::TextRenderer::new();
    let mut text = crate::widget::Text::new("Fresh font epoch");
    let first = actual_text_draw(&mut text, &mut renderer, 800.0);
    renderer.take_shape_stats();
    renderer.bump_generation();
    let current = actual_text_draw(&mut text, &mut renderer, 800.0);
    let (_, misses, _) = renderer.take_shape_stats();
    assert!(
        misses > 0,
        "existing Text Scene must not hide a fresh actual renderer/font epoch"
    );
    assert!(exact_scene_encoding_eq(&first, &current));
    let mut replacement = super::super::TextRenderer::new();
    actual_text_draw(&mut text, &mut replacement, 800.0);
    assert!(
        replacement.take_shape_stats().1 > 0,
        "renderer replacement has a distinct owned font epoch even at equal numeric generation"
    );
}
#[test]
fn text_widget_retained_input_actual_typography_changes_match_cold_widget() {
    fn modify(text: crate::widget::Text, index: usize) -> crate::widget::Text {
        match index {
            0 => text.with_content_size(21.0),
            1 => text.with_line_height(1.7),
            2 => text.with_role(crate::tokens::TextRole::Label),
            3 => text.with_weight(super::super::TextWeight::Bold),
            4 => text.with_italic(true),
            5 => text.with_font_family(super::super::FontChoice::Monospace),
            6 => text.with_role(crate::tokens::TextRole::Caption),
            7 => text.with_kerning(false),
            8 => text.with_discretionary_ligatures(true),
            9 => text.with_font_feature(super::super::OpenTypeTag::SMALL_CAPS, 1),
            10 => text.with_numeric_style(super::super::NumericStyle::TabularOldstyle),
            11 => text.with_font_feature(super::super::OpenTypeTag::KERNING, 0),
            12 => text.with_font_variation(super::super::OpenTypeTag::WIDTH, 90.0),
            13 => text.with_optical_sizing(super::super::OpticalSizing::None),
            14 => text.with_align(super::super::ParagraphAlign::Center),
            15 => text.with_balanced_wrap(),
            16 => text.with_auto_hyphenation("en-US"),
            17 => text.with_fragmentation(3, 4),
            _ => text.with_hanging_punctuation(super::super::HangingPunctuation::BOTH),
        }
    }
    for index in 0..19 {
        let mut renderer = super::super::TextRenderer::new();
        let mut text = crate::widget::Text::new("Current 123456 typography");
        actual_text_measure(&mut text, &mut renderer);
        actual_text_draw(&mut text, &mut renderer, 800.0);
        let mut text = modify(text, index);
        reset_work();
        let current = actual_text_measure(&mut text, &mut renderer);
        assert!(
            work().0 > 0,
            "changed typography must construct current immutable input {index}"
        );
        let mut cold = modify(crate::widget::Text::new("Current 123456 typography"), index);
        assert_eq!(
            current,
            actual_text_measure(&mut cold, &mut super::super::TextRenderer::new())
        );
    }
}
#[test]
fn text_widget_retained_input_actual_oversize_and_local_change_remain_current() {
    let large = "large label ".repeat(1600);
    let mut renderer = super::super::TextRenderer::new();
    let mut oversized = crate::widget::Text::new(&large);
    actual_text_measure(&mut oversized, &mut renderer);
    reset_work();
    actual_text_measure(&mut oversized, &mut renderer);
    assert!(
        work().0 >= large.len(),
        "oversize caller must not retain duplicate payload beyond short-label bound"
    );
    let state = crate::LocalState::new(String::from("short"));
    let mut text = crate::widget::Text::from_state(&state, String::clone);
    let before = actual_text_measure(&mut text, &mut renderer);
    state.set(String::from("changed substantially longer"));
    crate::widget::Widget::sync(&mut text, &automerge::AutoCommit::new(), &automerge::ROOT);
    let after = actual_text_measure(&mut text, &mut renderer);
    assert!(after.width > before.width);
    let mut cold = crate::widget::Text::new("changed substantially longer");
    assert_eq!(after, actual_text_measure(&mut cold, &mut renderer));
}

// Main already exposes the original full structural comparator. Retain every
// stream and late resource comparison; no count/hash-only replacement.
fn exact_scene_encoding_eq(left: &vello::Scene, right: &vello::Scene) -> bool {
    left.encoding_eq(right)
}

// Main exposes role changes instead of arbitrary Text spacing modifiers. Keep
// the original low-level spacing/tracking values covered through the renderer,
// together with each newer role/style field, under deliberately colliding hashes.
#[test]
fn current_style_fields_with_colliding_prepared_hashes_match_cold_rendering() {
    use super::super::{TextGround, TextKind, TextScaling};
    let style = TextStyle::new(20.0);
    let plain = PreparedText::new("Current Il10 typography", style.clone());
    let mut content = style.clone();
    content.scaling = TextScaling::Content;
    let mut numeric = style.clone();
    numeric.kind = TextKind::Numeric;
    let mut dark = style.clone();
    dark.ground = TextGround::Dark;
    let mut distinguish = style.clone();
    distinguish.distinguish = true;
    for changed in [
        content,
        numeric,
        dark,
        distinguish,
        style.clone().with_letter_spacing(2.0),
        style.with_tracking(0.1),
    ] {
        let input = PreparedText(Arc::new(TextIdentity {
            text: plain.0.text.clone(),
            style: changed,
            fingerprint: plain.0.fingerprint,
        }));
        let mut hot = crate::font_catalog::fixture_renderer();
        hot.set_text_scale(1.5);
        hot.measure_prepared(&plain, None);
        hot.take_shape_stats();
        let current = hot.measure_prepared(&input, None);
        assert!(
            hot.take_shape_stats().1 > 0,
            "an exact style change must not reuse a colliding identity"
        );
        let mut cold = crate::font_catalog::fixture_renderer();
        cold.set_text_scale(1.5);
        assert_eq!(current, cold.measure_prepared(&input, None));
        let mut actual = vello::Scene::new();
        let mut expected = vello::Scene::new();
        hot.draw_prepared(
            &mut actual,
            &input,
            vello::peniko::Color::WHITE,
            vello::kurbo::Affine::IDENTITY,
            None,
        );
        // Compare a cold geometry lookup on the same immutable font resources;
        // independent fixture renderers intentionally own distinct resource ids.
        hot.measure_cache.clear();
        hot.shape_cache.clear();
        hot.draw_prepared(
            &mut expected,
            &input,
            vello::peniko::Color::WHITE,
            vello::kurbo::Affine::IDENTITY,
            None,
        );
        assert!(exact_scene_encoding_eq(&actual, &expected));
        assert!(!actual.encoding().resources.glyph_runs.is_empty());
    }
}

#[test]
fn retained_geometry_separates_current_distinction_scope_and_restores_plain_hits() {
    let input = PreparedText::new("Current Il10 typography", TextStyle::new(20.0));
    let mut hot = crate::font_catalog::fixture_renderer();
    let plain = hot.measure_prepared(&input, None);
    hot.take_shape_stats();
    let scoped = hot.security_surface(|renderer| renderer.measure_prepared(&input, None));
    assert!(
        hot.take_shape_stats().1 > 0,
        "the scope needs its own geometry"
    );
    let mut cold = crate::font_catalog::fixture_renderer();
    assert_eq!(
        scoped,
        cold.security_surface(|renderer| renderer.measure_prepared(&input, None))
    );
    hot.take_shape_stats();
    assert_eq!(hot.measure_prepared(&input, None), plain);
    assert_eq!(
        hot.take_shape_stats().1,
        0,
        "leaving the scope restores its original cache hit"
    );
}
