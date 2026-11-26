//! [`DropTarget`](super::DropTarget) as a described component.

use tilo_ui_base::{component::Component, component::Described, component::Event, component::PartViews, component::ResourceId};
use tilo_ui_base::widget::Element;

use super::DropTarget;

/// A view that takes what another view dragged to it: a chat composer takes an attachment, a
/// dock takes a resource. A drag over it draws the toolkit's drop line, and letting go inside
/// it reports what the drag carried.
///
/// It takes a resource dragged out of any [`ui.DragSource`](super::DragSource), a run's item
/// ([`ui.DragItem`](super::DragItem)), and the device files a source of this same window stands
/// for — a file manager's entry let go on a folder's row, even while the desktop carries it.
/// A file dragged in from another application is not taken here, so it is never silently
/// swallowed (`ui.DropZone` and a window's own `drop` are for those).
#[derive(Component, Default)]
#[component(name = "DropTarget", role = "Group")]
pub struct DropTargetProps {
    /// What it draws.
    #[component(part(default))]
    pub content: Option<Element>,
    /// A resource was dropped on it: carries the resource the drag carried, as its address,
    /// and where inside the target it was let go — its x, then its y, in the target's own
    /// logical pixels. The view decides what to do with it — a composer attaches it to the
    /// draft, a canvas places it where it was dropped, a shell puts what it holds back there.
    #[component(event(value = ResourceId, at = Vec<i64>))]
    pub drop: Event,
    /// Device files a source of this window stood for were dropped on it: carries their paths,
    /// and where inside the target they were let go, as `drop` says it. A folder's row moves
    /// them into its folder.
    #[component(event(value = Vec<String>, at = Vec<i64>))]
    pub drop_files: Event,
}

impl Described for DropTarget {
    type Props = DropTargetProps;

    fn from_props(key: &str, props: DropTargetProps) -> Self {
        let DropTargetProps {
            content,
            drop,
            drop_files,
        } = props;
        let empty =
            || Box::new(tilo_ui_base::widget::Spacer::token(tilo_ui_base::tokens::SpacingStep::S0)) as Element;
        Self::new(key, content.unwrap_or_else(empty), drop).with_files(drop_files)
    }

    fn set_props(&mut self, props: DropTargetProps) -> bool {
        let DropTargetProps {
            content,
            drop,
            drop_files,
        } = props;
        self.set_content(content, drop, drop_files);
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "content").then_some(self as &mut dyn PartViews)
    }
}
