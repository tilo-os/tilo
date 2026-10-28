//! The notification centre's code, which a program may fetch the first
//! time the centre opens ([`NOTIFICATION_CENTRE`]).

use ui::a11y::AccessNode;
use ui::kurbo::{Affine, Point, Rect};
use ui::time::Instant;
use ui::vello::Scene;
use ui::winit::event::{MouseScrollDelta, WindowEvent};
use ui::winit::keyboard::Key;

use super::NotificationHost;
use super::center::{CenterLayout, CenterMetrics, CenterOutcome, NotificationCenter};

/// What the desktop asks of an open notification centre, laid out by
/// `layout`.
pub trait CentreApi {
    /// How tall the centre's rows ask the panel to be, `width` wide and at
    /// most `max` tall.
    fn fitted_height(
        &self,
        host: &dyn NotificationHost,
        metrics: &CenterMetrics,
        width: f64,
        max: f64,
    ) -> f64;
    fn click(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> CenterOutcome;
    fn wheel(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        delta: MouseScrollDelta,
        scale_factor: f64,
    );
    fn key(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
        backwards: bool,
    ) -> CenterOutcome;
    fn draw(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        cursor: Option<Point>,
    );
    fn reader_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool;
    fn reading_needs_frame(&self) -> bool;
    fn advance_reading(&mut self) -> bool;
    fn reading_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion>;
    fn apply_reading_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult;
    fn accessibility(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> AccessNode;
    fn accessibility_rect_for_path(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> Option<Rect>;
    fn focus_accessibility_path(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> bool;
    fn is_reading(&self) -> bool;
}

impl CentreApi for NotificationCenter {
    fn fitted_height(
        &self,
        host: &dyn NotificationHost,
        metrics: &CenterMetrics,
        width: f64,
        max: f64,
    ) -> f64 {
        Self::fitted_height(self, host, metrics.clone(), width, max)
    }
    fn click(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        p: Point,
    ) -> CenterOutcome {
        Self::click(self, host, layout, p)
    }
    fn wheel(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        delta: MouseScrollDelta,
        scale_factor: f64,
    ) {
        Self::wheel(self, host, layout, delta, scale_factor);
    }
    fn key(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        logical_key: &Key,
        backwards: bool,
    ) -> CenterOutcome {
        Self::key_with_shift(self, host, layout, logical_key, backwards)
    }
    fn draw(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        cursor: Option<Point>,
    ) {
        Self::draw(self, host, layout, scene, transform, text, theme, cursor);
    }
    fn reader_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        Self::reader_event(self, event, ctx)
    }
    fn reading_needs_frame(&self) -> bool {
        Self::reading_needs_frame(self)
    }
    fn advance_reading(&mut self) -> bool {
        Self::advance_reading(self)
    }
    fn reading_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        Self::reading_scroll(self)
    }
    fn apply_reading_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        Self::apply_reading_scroll(self, action)
    }
    fn accessibility(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        now: Instant,
    ) -> AccessNode {
        Self::accessibility(self, host, layout, now)
    }
    fn accessibility_rect_for_path(
        &self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> Option<Rect> {
        Self::accessibility_rect_for_path(self, host, layout, path)
    }
    fn focus_accessibility_path(
        &mut self,
        host: &dyn NotificationHost,
        layout: &CenterLayout,
        path: &[usize],
    ) -> bool {
        Self::focus_accessibility_path(self, host, layout, path)
    }
    fn is_reading(&self) -> bool {
        Self::is_reading(self)
    }
}

/// The notification centre's code, as a program that fetches it on first
/// use hands it over.
#[derive(Clone, Copy)]
pub struct MakeNotificationCentre(fn() -> Box<dyn CentreApi>);

impl MakeNotificationCentre {
    /// A freshly opened centre.
    #[must_use]
    pub fn make(self) -> Box<dyn CentreApi> {
        (self.0)()
    }
}

/// The notification centre this build links.
#[must_use]
pub fn builtin_notification_centre() -> MakeNotificationCentre {
    MakeNotificationCentre(|| Box::new(NotificationCenter::new()))
}

ui::lazy_code! {
    /// The notification centre's code: linked, or with
    /// `lazy-notification-centre` fetched the first time the centre opens.
    pub static NOTIFICATION_CENTRE: MakeNotificationCentre = builtin_notification_centre,
        feature = "lazy-notification-centre";
}
