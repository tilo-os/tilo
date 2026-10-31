//! Rich text as a reader sees it, through the real window: a person by what the workspace calls
//! them, a task by its title now, a time in the reader's zone and language, and a task the
//! reader does not hold as only being there. Each capture is kept as a PNG at a desktop's and a
//! phone's width, light and dark, for review against the design language.

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

/// Every text the rich text in the window shows and says, as its inspection reads it, a line
/// each.
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
    fn rich(value: &serde_json::Value, out: &mut Vec<String>) {
        if value.get("widget").and_then(serde_json::Value::as_str) == Some("BlockEditor") {
            strings(value, out);
            return;
        }
        if let Some(children) = value.get("children").and_then(serde_json::Value::as_array) {
            for child in children {
                rich(child, out);
            }
        }
        if let Some(root) = value.get("root") {
            rich(root, out);
        }
    }
    let json = app
        .harness()
        .inspection_json()
        .expect("the window is inspected");
    let mut out = Vec::new();
    rich(
        &serde_json::from_str(&json).expect("inspection is JSON"),
        &mut out,
    );
    out.dedup();
    out.join("\n")
}

#[test]
fn references_and_times_read_for_the_reader_at_every_size_and_theme() {
    support::installed("mentions");
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("richtext-shots");
    for (size, viewport) in [("desktop", (1280, 800)), ("phone", (420, 860))] {
        for (theme, dark) in [("light", false), ("dark", true)] {
            let mut app = TestApp::<TlApp>::capture_at(Some("mentions"), viewport)
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
                .render_png(&out.join(format!("mentions-{size}-{theme}.png")));
            let text = said(&mut app);
            assert!(text.contains("@Grace"), "{size} {theme}: {text}");
            assert!(
                text.contains("Fix the release build"),
                "{size} {theme}: {text}"
            );
            assert!(
                text.contains("Not shared with you"),
                "{size} {theme}: {text}"
            );
            assert!(
                !text.contains("Acquisition plan"),
                "nothing of a task the reader does not hold shows: {text}"
            );
            assert!(
                !text.contains("2026-10-29T09:30"),
                "the time reads in the reader's formats, not as written: {text}"
            );
        }
    }
}
