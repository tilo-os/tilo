//! SVG in and out, and working from a reference picture, through the host
//! door an agent uses.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::model::Shape;
use canvas::{App, ArtworkResource};
use serde_json::Value;
use ui::prelude::{TestApp, tilo_resource::ActionRef};

const BADGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">
  <defs>
    <linearGradient id="sky" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#204080"/>
      <stop offset="1" stop-color="#80c0ff"/>
    </linearGradient>
    <clipPath id="window"><rect x="0" y="-50" width="100" height="100"/></clipPath>
  </defs>
  <rect width="200" height="100" rx="12" fill="url(#sky)"/>
  <g transform="translate(150 50)" clip-path="url(#window)" opacity="0.5">
    <circle r="40" fill="#ffcc00" stroke="#000" stroke-width="4" stroke-dasharray="6 3" stroke-linecap="round"/>
  </g>
  <text x="20" y="60" font-size="24" font-family="serif" font-weight="700" fill="#ffffff">Hello</text>
</svg>"##;

const IMPORT: ActionRef<ArtworkResource, (String, String)> =
    ActionRef::new("import-svg", &["svg", "group"]);

fn import(app: &mut TestApp<App>, svg: &str, group: &str) {
    app.invoke(IMPORT.with((svg.into(), group.into())))
        .expect("import");
}

fn operate(app: &mut TestApp<App>, id: &'static str) -> Value {
    let op: ActionRef<ArtworkResource, ()> = ActionRef::new(id, &[]);
    app.operate(op.invoke()).unwrap().unwrap().value.unwrap()
}

/// An imported SVG is one step of marks in one group: gradients, outline
/// styling, clip paths, group opacity and text come through.
#[test]
fn an_svg_imports_as_one_group_of_marks() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    import(&mut app, BADGE, "badge");
    let art = app.state();
    let marks: Vec<_> = art.members("badge").collect();
    assert!(marks.len() >= 3, "{marks:#?}");
    assert!(
        marks.iter().any(|mark| mark.style.gradient.is_some()),
        "the gradient fill survives"
    );
    let ring = marks
        .iter()
        .find(|mark| mark.style.dash == vec![6.0, 3.0])
        .expect("the dashed ring");
    assert_eq!(ring.style.cap.as_deref(), Some("round"));
    assert!(
        (ring.style.opacity() - 0.5).abs() < 0.01,
        "{:?}",
        ring.style
    );
    assert!(!ring.clip.is_empty(), "the circle is clipped");
    let clip = art.mark(&ring.clip).expect("the clip is a mark");
    assert!(
        (clip.bounds().x0 - 150.0).abs() < 0.5,
        "{:?}",
        clip.bounds()
    );
    let text = marks
        .iter()
        .find_map(|mark| match &mark.shape {
            Shape::Text {
                text, weight, font, ..
            } => Some((text.clone(), *weight, font.clone())),
            _ => None,
        })
        .expect("the lettering");
    assert_eq!(text, ("Hello".into(), Some(700), Some("serif".into())));
    assert!(app.undo(), "an import is one undo step");
    assert!(app.state().marks.is_empty());

    let refused = app
        .invoke(IMPORT.with(("<svg".into(), "x".into())))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("SVG"), "{refused}");
}

/// What `export-svg` writes imports back as the same drawing.
#[test]
fn exported_svg_imports_back_as_the_same_marks() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    import(&mut app, BADGE, "badge");
    let exported = operate(&mut app, "export-svg");
    let svg = exported["svg"].as_str().expect("small SVG answers inline");
    assert!(svg.starts_with("<svg"), "{svg}");
    assert!(svg.contains("stroke-dasharray"), "{svg}");
    assert!(svg.contains("clipPath"), "{svg}");
    assert!(svg.contains("Hello</tspan>"), "{svg}");

    let mut again = TestApp::<App>::new().expect("the app opens");
    import(&mut again, svg, "again");
    let before = app.state().marks;
    let after = again.state().marks;
    let kinds = |marks: &[canvas::model::Mark]| {
        let mut kinds: Vec<_> = marks
            .iter()
            .filter(|mark| mark.style.opacity() > 0.0)
            .map(|mark| mark.shape.kind())
            .collect();
        kinds.sort_unstable();
        kinds
    };
    assert_eq!(kinds(&after), kinds(&before));
    for (a, b) in before
        .iter()
        .filter(|mark| mark.style.opacity() > 0.0)
        .zip(after.iter().filter(|mark| mark.style.opacity() > 0.0))
    {
        let (a, b) = (a.bounds(), b.bounds());
        assert!(
            (a.x0 - b.x0).abs() < 1.5 && (a.width() - b.width()).abs() < 1.5,
            "{a:?} vs {b:?}"
        );
    }
}

/// A reference picture set from a file is compared against the artwork:
/// matching it scores near zero; the wrong colour, or paint where the
/// reference is empty, scores high.
#[test]
fn compare_scores_the_artwork_against_its_reference() {
    let dir = std::env::temp_dir().join(format!("canvas-reference-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("red.png");
    image::RgbaImage::from_fn(64, 64, |x, _| {
        if x < 32 {
            image::Rgba([200, 30, 30, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    })
    .save(&path)
    .unwrap();

    let mut app = TestApp::<App>::new().expect("the app opens");
    let refused = app.operate(ActionRef::<ArtworkResource, ()>::new("compare", &[]).invoke());
    assert!(refused.is_err(), "no reference yet");

    let set: ActionRef<ArtworkResource, (String, [f64; 4])> =
        ActionRef::new("set-reference", &["image", "frame"]);
    app.invoke(set.with((
        path.to_string_lossy().into_owned(),
        [100.0, 100.0, 200.0, 200.0],
    )))
    .expect("set the reference from a file");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(app.state().reference.is_some());

    let rect = |width: u32, fill: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="900"><rect x="100" y="100" width="{width}" height="200" fill="{fill}"/></svg>"#
        )
    };
    let clear: ActionRef<ArtworkResource, String> = ActionRef::new("remove-marks", &["layer"]);
    let score = |value: &Value| value["difference"].as_f64().unwrap();
    import(&mut app, &rect(100, "#1e40c8"), "wrong");
    let wrong = operate(&mut app, "compare");
    app.invoke(clear.with("layer-1".into())).unwrap();
    import(&mut app, &rect(200, "#c81e1e"), "spill");
    let spill = operate(&mut app, "compare");
    app.invoke(clear.with("layer-1".into())).unwrap();
    import(&mut app, &rect(100, "#c81e1e"), "right");
    let right = operate(&mut app, "compare");
    assert!(score(&right) < 0.05, "{right}");
    assert!(score(&wrong) > 0.2, "{wrong}");
    assert!(
        score(&spill) > 0.2,
        "paint where the reference is empty counts: {spill}"
    );
    assert_eq!(right["mode"], "side-by-side");
    assert_eq!(right["worst_regions"].as_array().map(Vec::len), Some(4));

    let inspected = operate(&mut app, "inspect");
    assert_eq!(
        inspected["reference"]["pixels"],
        serde_json::json!([64, 64])
    );
}
