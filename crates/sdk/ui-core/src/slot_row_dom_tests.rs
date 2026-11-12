//! The window bar's location label as the page gets it: the words a person
//! reads in the bar reach the page as text runs, not only as the outlines a
//! canvas draws — including where the run was shaped by a frame drawn before
//! the page asked for text.

use std::sync::OnceLock;

use crate::widget::Decorator;
use crate::window_toolbar::WindowToolbar;

/// The book's title, as an app of its own names the place its bar shows.
const TITLE: &str = "The Lantern-Keeper's Almanac by Wren Halloway";

/// An app whose window bar names one place and whose frame holds one control,
/// so the bar's label is the only word the frame sets.
struct Titled {
    content: crate::widget::Column,
}

impl Decorator for Titled {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        crate::window_toolbar::declare(Some(WindowToolbar {
            location: Some(crate::window_toolbar::ToolbarLocation {
                label: TITLE.into(),
                ..crate::window_toolbar::ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }));
        crate::widget::Widget::sync(&mut self.content, state, path);
    }
}

/// The operations one frame of a window showing `TITLE` in its bar becomes,
/// as the HTML presenter makes them: a frame drawn before the page asks for
/// text, then the frame after it does.
fn ops() -> Vec<serde_json::Value> {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        crate::i18n::I18nManager::init("en-US");
    });
    let app = Titled {
        content: crate::widget::Column::new().push(Box::new(crate::widget::Button::new(
            "page",
            "The Lantern-Keeper's Almanac",
        ))),
    };
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        app,
        (1200, 800),
        crate::theme::Theme::dark().with_reduce_motion(true),
    );
    harness.host_window_slots();
    harness.track_scene_parts(true);
    crate::scene_parts::key_widgets(Some(crate::scene_parts::keyer()));
    harness.relayout();
    // A window draws before any page asks for text, as one does before a page
    // mounts it as HTML.
    let _ = harness.poll_frame();
    crate::text_render::present_text(true);
    let _ = harness.poll_frame();
    let frame = tilo_cpu_raster::dom::DomEncoder::new().encode(harness.scene().encoding());
    crate::scene_parts::key_widgets(None);
    // The offer outlives this test in its process: other tests of this binary
    // shape text as a GPU host does.
    crate::text_render::present_text(false);
    serde_json::from_str(&frame.ops).expect("the encoder writes JSON")
}

#[test]
fn the_window_bars_title_is_text_in_the_page() {
    let ops = ops();
    let runs: Vec<&str> = ops
        .iter()
        .filter(|op| op[0] == "t")
        .map(|op| op[1].as_str().unwrap_or_default())
        .collect();
    assert!(
        runs.iter().any(|run| run.contains("Lantern")),
        "the bar's title is a text run the page can select and read aloud: {runs:?}"
    );
}
