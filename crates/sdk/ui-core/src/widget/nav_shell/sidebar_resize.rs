//! The docked sidebar's resize edge: the separator assistive technology
//! and the keyboard step, and the drag that sets the sidebar's width or
//! hides it past its minimum.

use vello::kurbo::Rect;

use super::{NavShell, Resize};
use crate::num::f32_of_f64;
use crate::widget::flap::SplitLayout;
use crate::widget::{SplitHandle, SplitOrientation, SplitRatio};

impl NavShell {
    pub(super) fn sidebar_resize_target(&self) -> String {
        format!(
            "{}.sidebar-resize",
            self.id.as_deref().unwrap_or("nav-shell")
        )
    }

    /// The docked sidebar's resize edge as the shared split separator, its
    /// value the sidebar's share of the window, while the edge is draggable.
    pub(super) fn sidebar_resize_handle(&self) -> Option<SplitHandle> {
        let edge = self.geometry.resize_handle;
        let extent = self.rect.width();
        if self.resize.is_some() || self.animating || edge.width() <= 0.0 || extent <= 0.0 {
            return None;
        }
        let inputs = self.inputs.borrow();
        let overview = if inputs.has_overview {
            inputs.metrics.overview
        } else {
            0.0
        };
        let widest = inputs
            .metrics
            .sidebar_max
            .min((extent - overview - inputs.content_min()).max(0.0));
        let mut ratio = SplitRatio::default().with_bounds(0.0, f32_of_f64(widest / extent));
        ratio.set(crate::num::f32_of_f64(
            self.geometry.sidebar_column.width() / extent,
        ));
        let mut handle = SplitHandle::new(SplitOrientation::Horizontal, ratio);
        handle.set_bounds(Rect::new(self.rect.x0, edge.y0, self.rect.x1, edge.y1));
        handle.set_focused(self.resize_focused);
        Some(handle)
    }

    /// Resize the sidebar by a separator step or value, as a drag would.
    pub(super) fn step_sidebar_resize(&mut self, action: &crate::a11y::Action) -> bool {
        let Some(mut handle) = self.sidebar_resize_handle() else {
            return false;
        };
        if !handle.perform_action(action) {
            return false;
        }
        let width = handle.ratio().first_extent(self.rect.width());
        self.drag_sidebar_to(
            Resize {
                start_x: 0.0,
                start_width: 0.0,
            },
            width,
        );
        true
    }

    pub(super) fn drag_sidebar_to(&mut self, resize: Resize, x: f64) {
        let width = resize.start_width + (x - resize.start_x);
        let (min, scale) = {
            let inputs = self.inputs.borrow();
            (inputs.metrics.sidebar_min, inputs.metrics.scale)
        };
        if width < min * 0.75 {
            // Dragged past its minimum: the sidebar hides.
            if self.is_sidebar_open() {
                let mut inputs = self.inputs.borrow_mut();
                inputs.sidebar.snap(false);
            }
        } else {
            let mut inputs = self.inputs.borrow_mut();
            if !inputs.sidebar.is_open(SplitLayout::Docked) {
                inputs.sidebar.snap(true);
            }
            inputs.user_sidebar = Some(width.max(min) / scale.max(f64::EPSILON));
        }
        self.update_toggle_names();
        self.publish_open_states();
        self.structure_dirty = true;
        self.invalidate();
    }
}
