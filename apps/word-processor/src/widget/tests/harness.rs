use std::collections::HashMap;

use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;
use winit::event::WindowEvent;

use super::super::{ChromeColors, Editor, WordProcessorWidget};

pub(in crate::widget) fn mount(
    state: ui::State,
    wp: Editor,
    w: u32,
    h: u32,
) -> Framed<WordProcessorWidget> {
    mount_themed(state, wp, w, h, &Theme::dark())
}

pub(in crate::widget) fn mount_themed(
    state: ui::State,
    wp: Editor,
    w: u32,
    h: u32,
    theme: &Theme,
) -> Framed<WordProcessorWidget> {
    Framed::new(state, WordProcessorWidget::over(wp), (w, h), theme.clone())
}

pub(in crate::widget) fn split(
    framed: &mut Framed<WordProcessorWidget>,
) -> (&mut Editor, &mut ui::State) {
    let (window, state) = Framed::split(framed);
    (window.editor_mut(), state)
}

pub(in crate::widget) fn editor(framed: &mut Framed<WordProcessorWidget>) -> &mut Editor {
    split(framed).0
}

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

pub(in crate::widget) fn on_editor<T>(
    visit: impl FnOnce(&mut Editor) -> T,
) -> impl FnOnce(&mut WordProcessorWidget) -> T {
    move |window| visit(&mut window.state())
}

pub(in crate::widget) fn pipeline_draw(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
) -> (&mut Editor, &mut ui::State) {
    pipeline_draw_themed(framed, w, h, &Theme::dark());
    split(framed)
}

pub(in crate::widget) fn pipeline_draw_themed(
    framed: &mut Framed<WordProcessorWidget>,
    w: u32,
    h: u32,
    theme: &Theme,
) -> ChromeColors {
    Framed::harness_mut(framed)
        .relayout_themed((w, h), theme.clone())
        .expect("the frame settles");
    editor(framed).chrome
}
