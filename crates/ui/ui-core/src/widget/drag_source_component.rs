//! [`DragSource`](super::DragSource) as a described component.

use tilo_ui_base::{component::Component, component::Described, component::Event, component::PartViews};
use tilo_ui_base::widget::Element;

use super::DragSource;

/// A view a person can pick up and drag: to a drop target in the same window (a search hit, a
/// row of a list, a message's attachment), carrying `resource`, or, given `files`, out of the
/// window as a real file drag another app can take. The ghost under the pointer says `title`.
///
/// A press that does not travel is the content's own, so a dragged row still opens when clicked
/// and a dragged chip still presses.
#[derive(Component, Default)]
#[component(name = "DragSource", role = "Group", label = "title")]
pub struct DragSourceProps {
    /// The resource the drag carries, as its published address
    /// (`chat.workspace/messages/m1`): what a drop target receives. Empty for a drag of files
    /// alone, which leaves the window.
    #[component(positional, data)]
    pub resource: String,
    /// What the drag's ghost says under the pointer, and what assistive technology reads
    /// instead of the resource's address.
    pub title: Option<String>,
    /// The device files the drag offers another app, by path: a press that travels starts the
    /// desktop's own drag of them, so a person can drop them into another window. Without them
    /// the drag carries its `resource` on this window's drag bus.
    pub files: Vec<String>,
    /// What it draws.
    #[component(part(default))]
    pub content: Option<Element>,
    /// It was picked up and let go without travelling: the content's own press did not take
    /// it. Reported so a source that wraps something inert still says what happened.
    #[component(event(press))]
    pub activate: Event,
}

impl Described for DragSource {
    type Props = DragSourceProps;

    fn from_props(key: &str, props: DragSourceProps) -> Self {
        let DragSourceProps {
            resource,
            title,
            files,
            content,
            ..
        } = props;
        let empty =
            || Box::new(tilo_ui_base::widget::Spacer::token(tilo_ui_base::tokens::SpacingStep::S0)) as Element;
        Self::new(
            key,
            content.unwrap_or_else(empty),
            resource,
            title.unwrap_or_default(),
        )
        .with_files(files.into_iter().map(std::path::PathBuf::from).collect())
    }

    fn set_props(&mut self, props: DragSourceProps) -> bool {
        let DragSourceProps {
            resource,
            title,
            files,
            content,
            ..
        } = props;
        self.set_content(
            content,
            resource,
            title.unwrap_or_default(),
            files.into_iter().map(std::path::PathBuf::from).collect(),
        );
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "content").then_some(self as &mut dyn PartViews)
    }
}
