//! [`FloatingPanel`] as a described component: the body a palette, a popover, the notification
//! centre or the agent composer stands on.

use crate::component::{Component, Described, SlotViews};
use crate::widget::Element;
use crate::widget::arrange::{Fit, Slot};

use super::FloatingPanel;

/// The one body of a floating surface: a palette, a popover, the notification centre, the
/// agent composer. It is the floating level's ground, the panel radius, the inner hairline and
/// the two-layer shadow (`docs/design-language.md`, "Surfaces"); where it stands over the
/// desk's windows its ground is the blurred material, which the host draws.
#[derive(Component, Default)]
#[component(name = "FloatingPanel", role = "Group")]
pub struct FloatingPanelProps {
    /// A line over the body: a title, or the field a palette is typed into. A field here
    /// renders embedded in the panel's own ground.
    #[component(slot)]
    pub header: Option<Element>,
    /// What the panel holds, in order: the rows, the sections, the answer.
    #[component(slot(default))]
    pub content: Vec<Element>,
    /// What stands under the body, separated by space, never boxed: a composer's Send and its
    /// keys.
    #[component(slot)]
    pub footer: Option<Element>,
}

impl Described for FloatingPanel {
    type Props = FloatingPanelProps;

    fn from_props(_key: &str, props: FloatingPanelProps) -> Self {
        let FloatingPanelProps {
            header,
            content,
            footer,
        } = props;
        let mut stack = crate::widget::Stack::stack()
            .gap(crate::tokens::Rhythm::Related)
            .with_id("floating-body");
        for view in content {
            stack = stack.push_slot(Slot::new(view));
        }
        let mut panel = Self::new(Box::new(stack));
        if let Some(header) = header {
            panel = panel.header(header);
        }
        if let Some(footer) = footer {
            panel = panel.footer(footer);
        }
        panel
    }
}
