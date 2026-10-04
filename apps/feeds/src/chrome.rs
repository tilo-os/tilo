//! The reader's retained header controls and its reading-settings panel.
//!
//! Header actions are the toolkit's icon buttons in the shell's header row.
//! Each follows the library: a command with nothing to act on is disabled
//! (no feeds to refresh, no open article to star), a control whose meaning
//! flips with state is announced by what it does now, and a control that
//! opens a menu or panel records where it sits so the menu opens under it.

use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{AppModel, Button, Column, ModelPath, NumberInput, Text, ToggleGroup, Widget};
use vello::kurbo::Point;
use winit::event::WindowEvent;

use crate::app::{Core, Request, Requests, Shared};
use crate::model::{ReadFont, ReadSettings, ReadTheme};

/// A header control that anchors a menu or a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Anchor {
    ListMenu,
    ArticleMenu,
    ReaderSettings,
}

/// An icon button in the shell's header that follows the reader's state.
pub(crate) struct HeaderButton {
    button: Button,
    core: Shared,
    anchor: Option<Anchor>,
    enabled: fn(&Core) -> bool,
    named: Option<fn(&Core) -> &'static str>,
    toggled: Option<fn(&Core) -> bool>,
}

impl HeaderButton {
    pub(crate) fn new(button: Button, core: &Shared) -> Self {
        Self {
            button,
            core: Rc::clone(core),
            anchor: None,
            enabled: |_| true,
            named: None,
            toggled: None,
        }
    }

    /// Enabled only while `f` holds.
    pub(crate) fn enabled_when(mut self, f: fn(&Core) -> bool) -> Self {
        self.enabled = f;
        self
    }

    /// Announced (and titled) by what `f` says it does now.
    pub(crate) fn named_by(mut self, f: fn(&Core) -> &'static str) -> Self {
        self.named = Some(f);
        self
    }

    /// A toggle that reads as on (the selection lift) while `f` holds.
    pub(crate) fn toggled_by(mut self, f: fn(&Core) -> bool) -> Self {
        self.toggled = Some(f);
        self
    }

    /// Record where the button sits, for the menu or panel it opens.
    pub(crate) fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    fn follow(&mut self) {
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let enabled = (self.enabled)(&core);
        let name = self.named.map(|f| f(&core));
        let toggled = self.toggled.map(|f| f(&core));
        drop(core);
        if let Some(on) = toggled
            && self.button.is_toggled() != on
        {
            self.button.set_toggled(on);
        }
        if self.button.is_enabled() != enabled {
            self.button.set_enabled(enabled);
        }
        if let Some(name) = name {
            self.button.set_accessible_name(name);
            self.button.set_tooltip(Some(name.to_string()));
        }
    }
}

impl ui::widget::Decorator for HeaderButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.follow();
        Widget::sync(&mut self.button, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.follow();
        Widget::update(&mut self.button, state, path)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.button, taffy, node, origin);
        if let Some(anchor) = self.anchor
            && let Ok(mut core) = self.core.try_borrow_mut()
        {
            core.anchors.insert(anchor, Widget::get_rect(&self.button));
        }
    }

    fn name(&self) -> &'static str {
        "FeedsHeaderButton"
    }
}

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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let handled = Widget::perform_action(&mut self.input, action, state, path);
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
                f64::from(ReadSettings::SCALE_MIN) * 100.0,
                f64::from(ReadSettings::SCALE_MAX) * 100.0,
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
