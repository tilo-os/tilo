//! Type, finish and optical measure through the door an agent uses:
//! lettering in the fonts Tilo ships, tracked and set along a curve; grain,
//! paper tooth, inner shadows and feathered edges; ink measured as the eye
//! weighs it; and drawings that keep their groups and shadows through SVG.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::view_ops;
use serde_json::{Value, json};
use ui::prelude::TestApp;

fn draw(app: &mut TestApp<App>, marks: &Value) {
    app.call("draw", json!({ "marks": marks })).expect("draw");
}

fn view(app: &mut TestApp<App>, action: &str, args: Value) -> Value {
    let effect = args.as_object().and_then(|args| {
        <canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(action, args).ok()
    });
    if let Some(effect) = effect {
        return app
            .effect(&effect)
            .expect("the view effect finishes")
            .value
            .unwrap_or(Value::Null);
    }

    app.call(action, args).unwrap().unwrap()
}

/// The pixels of a PNG look at `region`.
fn pixels(app: &TestApp<App>, region: [f64; 4]) -> Vec<u8> {
    let art = app.state();
    let look = Look {
        region: Some(region),
        size: Some(256),
        format: Some("png".into()),
        ..Look::default()
    };
    let (picture, _) = view_ops::look(
        &mut Renderer::default(),
        &mut ui::TextRenderer::new(),
        &art,
        &look,
    )
    .unwrap();
    image::load_from_memory(&picture.bytes)
        .unwrap()
        .to_rgba8()
        .into_raw()
}

/// How far apart two pictures are, 0..255 per channel on average.
fn distance(a: &[u8], b: &[u8]) -> f64 {
    let total: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum();
    total / f64::from(u32::try_from(a.len().max(1)).unwrap_or(u32::MAX))
}

#[test]
fn text_uses_the_fonts_tilo_ships_and_names_them_when_refused() {
    let mut app = TestApp::<App>::new().unwrap();
    let media = view(&mut app, "media", json!({}));
    let families = media["fonts"]["families"].as_array().unwrap();
    assert!(
        families.iter().any(|family| family == ui::UI_SERIF_FAMILY),
        "{media}"
    );
    assert_eq!(media["fonts"]["generic"]["serif"], ui::UI_SERIF_FAMILY);

    let refused = app
        .call(
            "draw",
            json!({"marks": [{"shape": {"kind": "text", "x": 10, "y": 50, "text": "Hi", "size": 30, "font": "Comic Sans"}}]}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains(ui::UI_SANS_FAMILY),
        "the refusal lists the fonts: {refused}"
    );

    draw(
        &mut app,
        &json!([{"id": "title", "shape": {"kind": "text", "x": 40, "y": 120, "text": "MORAINE", "size": 80, "font": ui::UI_SERIF_FAMILY, "weight": 650, "tracking": 120}, "fill": "#222222"}]),
    );
    let measured = view(&mut app, "measure", json!({"ids": ["title"]}));
    let letters = measured["measured"][0]["letters"].as_array().unwrap();
    assert_eq!(letters.len(), 7, "one box per letter: {measured}");
    assert_eq!(letters[0]["t"], "M");
    let left = |index: usize| letters[index]["box"][0].as_f64().unwrap();
    let width = |index: usize| letters[index]["box"][2].as_f64().unwrap();
    // Tracking opens the gaps: the space after O is more than a tenth of
    // the size.
    assert!(left(2) - (left(1) + width(1)) > 8.0, "{measured}");
}

#[test]
fn words_follow_a_path_and_export_as_outlines() {
    let mut app = TestApp::<App>::new().unwrap();
    draw(
        &mut app,
        &json!([{"id": "arc", "shape": {"kind": "text", "x": 0, "y": 0, "text": "ALONG THE LAKE", "size": 40, "align": "center", "path": "M300,400 Q600,0 900,400"}, "fill": "#103050"}]),
    );
    let measured = view(&mut app, "measure", json!({"ids": ["arc"]}));
    let letters = measured["measured"][0]["letters"].as_array().unwrap();
    let top = |index: usize| letters[index]["box"][1].as_f64().unwrap();
    // The middle of the line rides the top of the curve; its ends sit lower.
    let middle = letters.len() / 2;
    assert!(top(middle) < top(0) - 30.0, "{measured}");
    assert!(top(middle) < top(letters.len() - 1) - 30.0, "{measured}");

    let svg = view(&mut app, "export-svg", json!({}));
    let markup = svg["svg"].as_str().unwrap();
    assert!(
        markup.contains(r#"<path id="arc""#) && !markup.contains("<text"),
        "text along a path exports as outlines: {markup}"
    );
    draw(
        &mut app,
        &json!([{"id": "plain", "shape": {"kind": "text", "x": 40, "y": 80, "text": "Rail", "size": 30, "tracking": 50}}]),
    );
    let svg = view(&mut app, "export-svg", json!({}));
    assert!(
        svg["svg"]
            .as_str()
            .unwrap()
            .contains(r#"letter-spacing="0.05em""#),
        "{svg}"
    );
    let outlined = view(&mut app, "export-svg", json!({"outline_text": true}));
    assert!(
        !outlined["svg"].as_str().unwrap().contains("<text"),
        "{outlined}"
    );
}

#[test]
fn grain_tooth_inner_shadow_and_feather_change_what_is_drawn() {
    let region = [0.0, 0.0, 400.0, 400.0];
    let plain = |app: &mut TestApp<App>| {
        draw(
            app,
            &json!([{"id": "hill", "shape": {"kind": "rect", "x": 100, "y": 100, "width": 200, "height": 200}, "fill": "#507090"}]),
        );
    };
    let mut app = TestApp::<App>::new().unwrap();
    plain(&mut app);
    let before = pixels(&app, region);
    for edit in [
        json!({"ids": ["hill"], "grain": 0.6}),
        json!({"ids": ["hill"], "inner_shadow": {"offset": [0, 12], "blur": 16, "colour": "#000000aa"}}),
        json!({"ids": ["hill"], "feather": 24}),
    ] {
        let mut app = TestApp::<App>::new().unwrap();
        plain(&mut app);
        app.call("edit-marks", edit.clone()).unwrap();
        let after = pixels(&app, region);
        assert!(distance(&before, &after) > 0.5, "{edit} shows");
    }
    let mut app = TestApp::<App>::new().unwrap();
    plain(&mut app);
    app.call("edit-layer", json!({"id": "layer-1", "texture": 1.0}))
        .unwrap();
    let after = pixels(&app, region);
    assert!(distance(&before, &after) > 0.2, "the paper's tooth shows");
    let inspect = view(&mut app, "inspect", json!({}));
    assert_eq!(inspect["layers_top_first"][0]["texture"], 1.0, "{inspect}");
}

#[test]
fn measure_finds_where_the_ink_weighs() {
    let mut app = TestApp::<App>::new().unwrap();
    draw(
        &mut app,
        &json!([
            {"id": "disc", "shape": {"kind": "ellipse", "cx": 200, "cy": 200, "rx": 60, "ry": 60}, "fill": "#000000"},
            {"id": "wedge", "shape": {"kind": "polygon", "points": [[500, 100], [500, 300], [700, 300]]}, "fill": "#000000"},
        ]),
    );
    let measured = view(&mut app, "measure", json!({"ids": ["disc", "wedge"]}));
    let disc = &measured["measured"][0];
    let wedge = &measured["measured"][1];
    let offset = |entry: &Value, axis: usize| entry["offset"][axis].as_f64().unwrap();
    assert!(
        offset(disc, 0).abs() < 1.0 && offset(disc, 1).abs() < 1.0,
        "{disc}"
    );
    // A right triangle's ink sits toward its right angle, a third of the way.
    assert!(
        offset(wedge, 0) < -25.0 && offset(wedge, 1) > 25.0,
        "{wedge}"
    );
    let area = disc["ink_area"].as_f64().unwrap();
    assert!(
        (area - std::f64::consts::PI * 3600.0).abs() < 400.0,
        "{disc}"
    );
}

#[test]
fn nested_groups_and_shadows_survive_an_svg_round_trip() {
    let mut app = TestApp::<App>::new().unwrap();
    draw(
        &mut app,
        &json!([
            {"id": "mail-body", "group": "icons/mail", "shape": {"kind": "rect", "x": 20, "y": 20, "width": 80, "height": 60}, "fill": "#3060a0",
             "shadow": {"offset": [0, 4], "blur": 6, "colour": "#00000066"}},
            {"id": "home-roof", "group": "icons/home", "shape": {"kind": "polygon", "points": [[150, 60], [190, 20], [230, 60]]}, "fill": "#a03030"},
        ]),
    );
    let inspect = view(&mut app, "inspect", json!({}));
    assert_eq!(
        inspect["groups"],
        json!(["icons", "icons/home", "icons/mail"]),
        "{inspect}"
    );
    let svg = view(&mut app, "export-svg", json!({}));
    let markup = svg["svg"].as_str().unwrap().to_owned();
    app.call("remove-marks", json!({"layer": "layer-1"}))
        .unwrap();
    app.call("import-svg", json!({"svg": markup, "group": "again"}))
        .unwrap();
    let art = app.state();
    let mail: Vec<_> = art.members("again/icons/mail").collect();
    assert_eq!(mail.len(), 1, "{:#?}", art.marks);
    assert!(
        mail[0].style.shadow.is_some(),
        "the drop shadow comes back: {:?}",
        mail[0].style
    );
    assert_eq!(art.members("again/icons").count(), 2);
    // A group inside a group moves with its outer group.
    app.call(
        "edit-marks",
        json!({"ids": ["again/icons"], "move_by": [10, 0]}),
    )
    .unwrap();
}

#[test]
fn diff_says_what_changed_about_a_mark() {
    let mut app = TestApp::<App>::new().unwrap();
    draw(
        &mut app,
        &json!([
            {"id": "a", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 50, "height": 50}, "fill": "#ff0000"},
            {"id": "b", "shape": {"kind": "rect", "x": 20, "y": 20, "width": 50, "height": 50}, "fill": "#00ff00"},
        ]),
    );
    app.call("checkpoint", json!({"name": "drawn"})).unwrap();
    app.call("arrange", json!({"ids": ["a"], "to": "front"}))
        .unwrap();
    app.call("edit-marks", json!({"ids": ["b"], "opacity": 0.5}))
        .unwrap();
    let diff = view(&mut app, "compare-versions", json!({"from": "drawn"}));
    assert_eq!(
        diff["marks"]["changed"],
        json!({"a": ["stacking"], "b": ["opacity"]}),
        "{diff}"
    );
}

#[test]
fn a_reference_picture_can_be_sent_inline() {
    use base64::Engine as _;
    let mut app = TestApp::<App>::new().unwrap();
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(8, 4, image::Rgba([200, 40, 40, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let inline = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    app.call("set-reference", json!({"image": inline})).unwrap();
    let reference = app.state().reference.expect("a reference");
    assert!(
        ui::tilo_actions::content_hash(&reference.image).is_some(),
        "{reference:?}"
    );
    let refused = app
        .call(
            "set-reference",
            json!({"image": "data:image/png;base64,bm90IGEgcGljdHVyZQ=="}),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("not a PNG, JPEG or WebP"), "{refused}");
}

/// Tracking follows every letter, the last too; a centred line still
/// centres its letters, and `measure` sees all of their ink.
#[test]
fn tracked_centred_text_centres_its_letters() {
    let mut app = TestApp::<App>::new().unwrap();
    draw(
        &mut app,
        &json!([{"id": "t", "shape": {"kind": "text", "x": 600, "y": 200, "text": "RAIL", "size": 80, "tracking": 400, "align": "center"}}]),
    );
    let measured = view(&mut app, "measure", json!({"ids": ["t"]}));
    let entry = &measured["measured"][0];
    let letters = entry["letters"].as_array().unwrap();
    let left = letters[0]["box"][0].as_f64().unwrap();
    let last = &letters[letters.len() - 1]["box"];
    let right = last[0].as_f64().unwrap() + last[2].as_f64().unwrap();
    // Within the letters' own side bearings; the trailing space alone is 16.
    assert!((f64::midpoint(left, right) - 600.0).abs() < 4.0, "{entry}");
    let ink = entry["ink"].as_array().unwrap();
    let ink_left = ink[0].as_f64().unwrap();
    assert!(
        (ink_left - left).abs() < 2.0,
        "the ink box holds every letter: {entry}"
    );
}
