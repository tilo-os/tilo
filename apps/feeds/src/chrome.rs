//! The reader's reading-settings panel. Its commands are the window bar's
//! slots (`app::window_actions`).

use std::rc::Rc;

use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{Column, ModelPath, NumberInput, Text, ToggleGroup, Widget};
use winit::event::WindowEvent;

use crate::app::{Request, Requests};
use crate::model::{ReadFont, ReadSettings, ReadTheme};

/// A number field that reports each change it makes as a request.
struct ScaleField {
    input: NumberInput,
    requests: Requests,
}

impl ScaleField {
    fn report(&mut self) {
        if let Some(value) = self.input.take_changed() {
            self.requests.borrow_mut().push(Request::SetScale(value));
        }
    }
}

impl ui::widget::Decorator for ScaleField {
    type Inner = NumberInput;

    fn inner(&self) -> &NumberInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut NumberInput {
        &mut self.input
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.input, event, ctx);
        self.report();
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let handled = Widget::perform_action(&mut self.input, action, &[], state, path);
        self.report();
        handled
    }

    fn name(&self) -> &'static str {
        "FeedsReadingScale"
    }
}

/// The reading settings: text size, the reading face and the paper.
pub(crate) fn reader_settings_panel(settings: ReadSettings, requests: &Requests) -> Column {
    let label = |text: &str| Box::new(Text::new(text).with_role(TextRole::Subheading));
    let scale = ScaleField {
        input: NumberInput::new(settings.percent())
            .with_range(
                f64::from(*ReadSettings::TEXT_PERCENT_RANGE.start()),
                f64::from(*ReadSettings::TEXT_PERCENT_RANGE.end()),
            )
            .with_step(10.0)
            .with_suffix("%")
            .with_id("feeds.reading-scale")
            .with_accessible_name("Text size")
            .compact(),
        requests: Rc::clone(requests),
    };
    let fonts = {
        let requests = Rc::clone(requests);
        ToggleGroup::new(
            ReadFont::ALL
                .iter()
                .map(|f| Segment::new(f.label()))
                .collect(),
            ReadFont::ALL
                .iter()
                .position(|f| *f == settings.font)
                .unwrap_or(0),
        )
        .with_id("feeds.reading-face")
        .fill()
        .on_change(move |index| {
            if let Some(font) = ReadFont::ALL.get(index) {
                requests.borrow_mut().push(Request::SetFont(*font));
            }
        })
    };
    let papers = {
        let requests = Rc::clone(requests);
        ToggleGroup::new(
            ReadTheme::ALL
                .iter()
                .map(|t| Segment::new(t.label()))
                .collect(),
            ReadTheme::ALL
                .iter()
                .position(|t| *t == settings.theme)
                .unwrap_or(0),
        )
        .with_id("feeds.reading-paper")
        .fill()
        .on_change(move |index| {
            if let Some(paper) = ReadTheme::ALL.get(index) {
                requests.borrow_mut().push(Request::SetPaper(*paper));
            }
        })
    };
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_id("feeds.reading-settings")
        .push(label("Text size"))
        .push(Box::new(scale))
        .push(label("Font"))
        .push(Box::new(fonts))
        .push(label("Paper"))
        .push(Box::new(papers))
}
