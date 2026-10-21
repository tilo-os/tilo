//! The reading menu: one menu for every reader, setting the person's shared
//! [`ReadingPreferences`] (size, face, line length, paper, alignment).
//!
//! A reader opens it in a popover under its reading button and applies each
//! [`ReadingChange`] the menu reports to the shared preferences, so a
//! change made in Feeds sets the Browser's reader mode and Mail too.

use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::Point;
use winit::event::WindowEvent;

use super::palette::Paper;
use super::preferences::{Measure, ReadingFont, ReadingPreferences};
use crate::tokens::TextRole;
use crate::widget::toggle_group::Segment;
use crate::widget::{AppModel, Column, ModelPath, NumberInput, Text, ToggleGroup, Widget};

/// One change the reading menu asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadingChange {
    /// The reading size as a whole percentage (`110` for 1.1).
    Size(f64),
    Font(ReadingFont),
    Measure(Measure),
    Paper(Paper),
    /// Justified (`true`) or ragged paragraphs.
    Justify(bool),
}

impl ReadingChange {
    /// Apply the change to `prefs`, keeping every value in range.
    pub fn apply(self, prefs: &mut ReadingPreferences) {
        match self {
            Self::Size(percent) => prefs.size = crate::num::f32_of_f64(percent / 100.0),
            Self::Font(font) => prefs.font = font,
            Self::Measure(measure) => prefs.measure = measure,
            Self::Paper(paper) => prefs.paper = paper,
            Self::Justify(justify) => prefs.justify = justify,
        }
        *prefs = prefs.clamped();
    }
}

/// What the menu calls for each change.
pub type OnReadingChange = Rc<dyn Fn(ReadingChange)>;

/// The size field, reporting each value the person commits.
struct SizeField {
    input: NumberInput,
    changed: OnReadingChange,
}

impl SizeField {
    fn report(&mut self) {
        if let Some(value) = self.input.take_changed() {
            (self.changed)(ReadingChange::Size(value));
        }
    }
}

impl crate::widget::Decorator for SizeField {
    type Inner = NumberInput;

    fn inner(&self) -> &NumberInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut NumberInput {
        &mut self.input
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.input, event, ctx);
        self.report();
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let handled = Widget::perform_action(&mut self.input, action, &[], state, path);
        self.report();
        handled
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.input, taffy, node, origin);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        Widget::sync(&mut self.input, state, path);
    }

    fn name(&self) -> &'static str {
        "ReadingSize"
    }
}

/// One choice among `options`, reporting the chosen one.
fn choice<T: Copy + PartialEq + 'static>(
    id: &str,
    options: &'static [T],
    label: fn(T) -> &'static str,
    selected: T,
    change: fn(T) -> ReadingChange,
    changed: &OnReadingChange,
) -> ToggleGroup {
    let changed = Rc::clone(changed);
    ToggleGroup::new(
        options
            .iter()
            .map(|option| Segment::new(label(*option)))
            .collect(),
        options
            .iter()
            .position(|option| *option == selected)
            .unwrap_or(0),
    )
    .with_id(id)
    .fill()
    .on_change(move |index| {
        if let Some(option) = options.get(index) {
            changed(change(*option));
        }
    })
}

/// The reading menu for `prefs`; `id` prefixes its controls' ids
/// (`feeds.reading`).
#[must_use]
pub fn reading_menu(prefs: ReadingPreferences, id: &str, changed: &OnReadingChange) -> Column {
    let label = |text: &str| Box::new(Text::new(text).with_role(TextRole::Subheading));
    let size = SizeField {
        input: NumberInput::new(f64::from(prefs.percent()))
            .with_range(
                f64::from(*ReadingPreferences::SIZE_RANGE.start()) * 100.0,
                f64::from(*ReadingPreferences::SIZE_RANGE.end()) * 100.0,
            )
            .with_step(f64::from(ReadingPreferences::SIZE_STEP) * 100.0)
            .with_suffix("%")
            .with_id(&format!("{id}-size"))
            .with_accessible_name("Text size")
            .compact(),
        changed: Rc::clone(changed),
    };
    const FONTS: &[ReadingFont] = &ReadingFont::ALL;
    const MEASURES: &[Measure] = &Measure::ALL;
    const PAPERS: &[Paper] = &Paper::ALL;
    const ALIGNMENTS: &[bool] = &[false, true];
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_id(&format!("{id}-menu"))
        .push(label("Text size"))
        .push(Box::new(size))
        .push(label("Font"))
        .push(Box::new(choice(
            &format!("{id}-face"),
            FONTS,
            ReadingFont::label,
            prefs.font,
            ReadingChange::Font,
            changed,
        )))
        .push(label("Line length"))
        .push(Box::new(choice(
            &format!("{id}-measure"),
            MEASURES,
            Measure::label,
            prefs.measure,
            ReadingChange::Measure,
            changed,
        )))
        .push(label("Page"))
        .push(Box::new(choice(
            &format!("{id}-paper"),
            PAPERS,
            Paper::label,
            prefs.paper,
            ReadingChange::Paper,
            changed,
        )))
        .push(label("Alignment"))
        .push(Box::new(choice(
            &format!("{id}-alignment"),
            ALIGNMENTS,
            |justify| if justify { "Justified" } else { "Left" },
            prefs.justify,
            ReadingChange::Justify,
            changed,
        )))
}
