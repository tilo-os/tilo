//! [`Labelled`](super::Labelled) as a described component.

use tilo_ui_base::widget::Element;
use tilo_ui_base::{component::Component, component::Described, component::PartViews};

use super::Labelled;

/// A control a view composed of its own parts (a chooser, a field holding parts of its own),
/// named by the caption the view places beside it: a screen reader and an agent reach it by the
/// words the person reads there. A control that names itself keeps its own name.
#[derive(Component, Default)]
#[component(name = "Labelled", role = "Group", label = "label")]
pub struct LabelledProps {
    /// The caption the view shows beside the control, which names it.
    #[component(positional)]
    pub label: String,
    /// A line that explains the control, read after its name.
    pub description: Option<String>,
    /// The control it names.
    #[component(part(default))]
    pub content: Option<Element>,
}

fn nothing() -> Element {
    Box::new(tilo_ui_base::widget::Spacer::token(
        tilo_ui_base::tokens::SpacingStep::S0,
    ))
}

impl Described for Labelled {
    type Props = LabelledProps;

    fn from_props(_key: &str, props: LabelledProps) -> Self {
        let LabelledProps {
            label,
            description,
            content,
        } = props;
        Self::new(label, content.unwrap_or_else(nothing)).described(description.unwrap_or_default())
    }

    /// The caption and its line change in place; the control is the part's, which the host
    /// changes through [`Described::part_views`].
    fn set_props(&mut self, props: LabelledProps) -> bool {
        let LabelledProps {
            label, description, ..
        } = props;
        self.label = label;
        self.description = description.unwrap_or_default();
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "content").then_some(self as &mut dyn PartViews)
    }
}

/// The one control it names.
impl PartViews for Labelled {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.content)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.content = view;
    }

    fn remove(&mut self, _index: usize) {
        self.content = nothing();
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.content = view;
    }
}
