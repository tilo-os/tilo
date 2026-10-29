//! Views, the app an agent's answer opens in: the answer is a view block drawn live through
//! `embed()` with the agent's task as its code author, over the resources of the apps Views
//! mounts. Each capture is kept as a PNG at a desktop's and a phone's width, light and dark, for
//! review against the design language.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;

/// Every text the window shows, as its inspection reads it, a line each.
fn said(app: &mut TestApp<TlApp>) -> String {
    fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) if !text.is_empty() => out.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
            serde_json::Value::Object(fields) => fields
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "path" | "widget" | "test_id"))
                .for_each(|(_, item)| strings(item, out)),
            _ => {}
        }
    }
    let json = app
        .harness()
        .inspection_json()
        .expect("the window is inspected");
    let mut out = Vec::new();
    strings(
        &serde_json::from_str(&json).expect("inspection is JSON"),
        &mut out,
    );
    out.dedup();
    out.join("\n")
}

#[test]
fn an_answer_is_a_live_view_over_other_apps_at_every_size_and_theme() {
    support::installed("views");
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("views-shots");
    for (size, viewport) in [("desktop", (1280, 800)), ("phone", (420, 860))] {
        for (theme, dark) in [("light", false), ("dark", true)] {
            let mut app = TestApp::<TlApp>::capture_at(Some("a release answer"), viewport)
                .expect("the scenario stages");
            app.harness().set_theme(if dark {
                ui::Theme::dark()
            } else {
                ui::Theme::light()
            });
            app.harness()
                .settle_for_capture(ui::time::Duration::from_millis(600));
            app.harness().relayout();
            app.harness()
                .render_png(&out.join(format!("views-answer-{size}-{theme}.png")));
            let text = said(&mut app);
            // The answer's title, and the tasks' card drawn by Tasks' own view: the block
            // rendered, as a view, not an error. (Its rows over live tasks are the runtime's
            // test, `an_answer_draws_its_view_block_live_over_other_apps_as_its_task_wrote_it`.)
            for shown in ["Left for the release", "Tasks"] {
                assert!(text.contains(shown), "{size} {theme}: {shown:?} in {text}");
            }
            assert!(!text.contains("E0"), "the block compiled: {text}");
        }
    }
}

/// The view the recorded model answers with in the agent's end-to-end check compiles as a view
/// over the apps Views mounts. (Its programs are a task's session, each after the last, which
/// the door runs; only the view stands alone.)
#[test]
fn the_recorded_answer_compiles_as_a_view_over_the_apps_views_mounts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let views = root.join("../tl/views");
    let dirs = tilolang::source::dependency_order(&views, &[]).expect("the packages resolve");
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .expect("the packages compile");
    let metas: Vec<tilolang::PackageMeta> = built.iter().map(|b| b.compiled.meta.clone()).collect();
    let script: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("../../scripts/headless-examples/agent-tilolang/release.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let views: Vec<&str> = script["turns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|turn| turn["blocks"].as_array().unwrap())
        .filter_map(|block| block["input"]["view"]["source"].as_str())
        .collect();
    assert_eq!(views.len(), 1, "the script answers with one view");
    for view in views {
        let compiled =
            tilolang::compile_block(view, &metas).unwrap_or_else(|d| panic!("{view}: {d:#?}"));
        assert_eq!(compiled.kind, tilolang::BlockKind::View, "{view}");
    }
}
