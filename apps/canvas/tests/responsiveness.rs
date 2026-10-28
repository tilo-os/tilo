use canvas::{
    App,
    render::{Options, Renderer},
};
use serde_json::json;
use std::fmt::Write;
use ui::prelude::TestApp;
use ui::vello::{Scene, kurbo::Affine};

#[test]
fn compound_pixel_paths_survive_repeated_zoom_redraws() {
    let mut app = TestApp::<App>::new().unwrap();
    let mut d = String::new();
    for n in 0..500 {
        let x = n % 32;
        let y = n / 32;
        write!(d, "M{x} {y}h1v1h-1Z").unwrap();
    }
    let marks = (0..55)
        .map(|n| json!({"id":format!("colour-{n}"),"shape":{"kind":"path","d":d},"fill":"#f7d48b"}))
        .collect::<Vec<_>>();
    app.call("draw", json!({"marks":marks})).unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let art = app.state();
    let mut samples = Vec::new();
    for n in 0..40 {
        let mut scene = Scene::new();
        let start = std::time::Instant::now();
        renderer.draw(
            &art,
            &mut scene,
            Affine::scale(1.0 + f64::from(n % 8)),
            &mut text,
            &Options::default(),
        );
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(!scene.encoding().path_data.is_empty());
    }
    samples.sort_by(f64::total_cmp);
    eprintln!(
        "vector zoom CPU scene construction: median={:.3}ms p95={:.3}ms (55 paths, 27,500 cell rectangles)",
        samples[20], samples[38]
    );
    assert_eq!(
        app.state().marks.len(),
        55,
        "redrawing never edits the artwork"
    );
}

#[test]
fn edited_svg_geometry_and_undo_never_use_a_stale_cached_outline() {
    use canvas::{resource::Look, view_ops};
    let mut app = TestApp::<App>::new().unwrap();
    app.call("draw",json!({"marks":[{"id":"outline","shape":{"kind":"path","d":"M10 10h80v80h-80Z"},"fill":"#b93f30"}]})).unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let look = Look {
        format: Some("png".into()),
        size: Some(128),
        ..Look::default()
    };
    let before = view_ops::look(&mut renderer, &mut text, &app.state(), &look)
        .unwrap()
        .0
        .bytes;
    app.call(
        "edit-marks",
        json!({"ids":["outline"],"shape":{"kind":"path","d":"M150 10h80v80h-80Z"}}),
    )
    .unwrap();
    let after = view_ops::look(&mut renderer, &mut text, &app.state(), &look)
        .unwrap()
        .0
        .bytes;
    assert_ne!(
        before, after,
        "editing existing geometry invalidates the cache"
    );
    let fresh = view_ops::look(&mut Renderer::default(), &mut text, &app.state(), &look)
        .unwrap()
        .0
        .bytes;
    assert_eq!(
        after, fresh,
        "cached and independently rendered geometry agree"
    );
    assert!(app.undo());
    let restored = view_ops::look(&mut renderer, &mut text, &app.state(), &look)
        .unwrap()
        .0
        .bytes;
    assert_eq!(
        before, restored,
        "undo restores exact pixels even with the same object id"
    );
}

#[test]
fn repeated_artwork_publication_keeps_the_same_action_contract() {
    let art = canvas::Artwork::default();
    let expected = ui::tilo_resource::publish::<canvas::Artwork>("art", "Artwork", &art);
    let mut samples = Vec::new();
    for _ in 0..40 {
        let start = std::time::Instant::now();
        let published = ui::tilo_resource::publish::<canvas::Artwork>("art", "Artwork", &art);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        let actual: Vec<_> = published
            .actions
            .iter()
            .map(|action| {
                (
                    &action.id,
                    &action.label,
                    &action.description,
                    action.params(),
                )
            })
            .collect();
        let contract: Vec<_> = expected
            .actions
            .iter()
            .map(|action| {
                (
                    &action.id,
                    &action.label,
                    &action.description,
                    action.params(),
                )
            })
            .collect();
        assert_eq!(actual, contract);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "CANVAS_PUBLICATION median_ms={:.3} p95_ms={:.3}",
        samples[19], samples[37]
    );
}
