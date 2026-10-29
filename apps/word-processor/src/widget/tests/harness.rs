//! Mounting the editor in its window for a test, and reaching the editor
//! the window shows.

use std::collections::HashMap;

use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use winit::event::WindowEvent;

use super::super::{ChromeColors, Editor, WordProcessorWidget};

/// Mount `wp` in its window over `state` in a
/// [`FrameHarness`](ui::headless::FrameHarness) and run its first frame at
/// `w`×`h` under the dark theme.
pub(in crate::widget) fn mount(
    state: ui::State,
    wp: Editor,
    w: u32,
    h: u32,
) -> Framed<WordProcessorWidget> {
    mount_themed(state, wp, w, h, &Theme::dark())
}

/// Like [`mount`] but runs the first frame under `theme`.
pub(in crate::widget) fn mount_themed(
    state: ui::State,
    wp: Editor,
    w: u32,
    h: u32,
    theme: &Theme,
) -> Framed<WordProcessorWidget> {
    Framed::new(state, WordProcessorWidget::over(wp), (w, h), theme.clone())
}

/// The mounted window's editor and its document at once; the next frame
/// recomposes from scratch.
pub(in crate::widget) fn split(
    framed: &mut Framed<WordProcessorWidget>,
) -> (&mut Editor, &mut ui::State) {
    let (window, state) = Framed::split(framed);
    (window.editor_mut(), state)
}

/// The editor the window shows to the test that mounted it.
pub(in crate::widget) fn editor(framed: &mut Framed<WordProcessorWidget>) -> &mut Editor {
    split(framed).0
}

/// The editor lent to [`Dispatch::send`] for one event, as the widget its
/// window is.
pub(in crate::widget) struct Lent<'a>(pub(in crate::widget) &'a mut Editor);

impl Widget for Lent<'_> {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        _node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy
            .new_leaf(taffy::Style::default())
            .expect("a scratch leaf")
    }

    fn name(&self) -> &'static str {
        "WordProcessor"
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.0.handle_event(event, ctx);
    }
}

/// `visit` of the editor a framework app's window shows, for
/// [`TestApp::with_surface`](ui::framework::testing::TestApp::with_surface).
pub(in crate::widget) fn on_editor<T>(
    visit: impl FnOnce(&mut Editor) -> T,
) -> impl FnOnce(&mut WordProcessorWidget) -> T {
    move |window| visit(&mut window.state())
}

/// Run one frame at `w`×`h` under the dark theme that recomposes the whole
/// layout tree, and hand back the editor and its document for what follows.
pub(in crate::widget) fn pipeline_draw(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
) -> (&mut Editor, &mut ui::State) {
    pipeline_draw_themed(framed, w, h, &Theme::dark());
    split(framed)
}

/// Like [`pipeline_draw`] but renders under an explicit [`Theme`] so tests
/// can exercise the real five-pass draw path under light/dark/high-contrast
/// palettes. Returns the widget's resolved chrome palette for inspection.
pub(in crate::widget) fn pipeline_draw_themed(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    theme: &Theme,
) -> ChromeColors {
    Framed::harness_mut(framed)
        .relayout_themed((w, h), theme.clone())
        .expect("the frame settles");
    // `draw` resolves the chrome palette from `theme` into `wp.chrome`.
    editor(framed).chrome
}
