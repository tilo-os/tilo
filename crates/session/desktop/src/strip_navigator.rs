//! Where you are in the workspace: the strip ribbon on the content's right
//! edge (which strip, of how many) grown into the strip switcher (who is on
//! each), and the strip rail on its bottom edge (which window of the strip,
//! which are on screen). Every desktop host builds them through
//! this, the native shell and a browser page alike.
//!
//! The parts: [`StripSwitcher`] for the labels, [`StripIndicators`] for
//! when the ribbon and rail show (on a strip or window change, then
//! fading), and [`SlidingFocus`] for their long segments sliding to what
//! took focus. The host says where they stand and what they report as a
//! [`StripFrame`], read from its own state each time, and turns the
//! returned [`StripNavigation`] into changes on its desktop.

use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId};
use ui::chrome::shell::{StripRail, StripRibbon};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};

use crate::strip_indicators::{SlidingFocus, StripIndicators, StripPosition};
use crate::strip_switcher::{
    Command, DrawCx, KeyAnswer, Press, RIBBON_REST_ALPHA, StripSwitcher, SwitcherModel,
};

/// What the host does for the navigator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StripNavigation {
    /// A strip edit or switch from the ribbon or its labels.
    Strip(Command),
    /// Focus this window of this strip, from a rail segment.
    Window { strip: StripId, group: GroupId },
}

/// Where the ribbon and rail stand and what they report, as the host reads
/// it from its own state.
#[derive(Clone, Debug)]
pub struct StripFrame {
    /// The ribbon: the active workspace's strips, the shown one focused.
    pub ribbon: StripRibbon,
    /// The rail: the shown strip's windows, which has focus and which are
    /// on screen.
    pub rail: StripRail,
    /// The active workspace's strips, in ribbon order.
    pub strips: Vec<StripId>,
    pub strip: Option<StripId>,
    /// The shown strip's windows, in rail order.
    pub windows: Vec<GroupId>,
    /// How far the shown strip is scrolled, in layout pixels.
    pub scroll_x: f64,
    /// The ribbon and its labels show and take input: off under an
    /// overview, a fullscreen window, or a panel that replaces them.
    pub ribbon_offered: bool,
    /// The rail shows and takes input: off under an overview or a
    /// fullscreen window.
    pub rail_offered: bool,
}

impl StripFrame {
    /// Nothing to show in `content`.
    pub fn empty(content: Rect) -> Self {
        Self {
            ribbon: StripRibbon {
                content,
                count: 0,
                focused: 0,
            },
            rail: StripRail {
                content,
                count: 0,
                focused: 0,
                visible: Vec::new(),
            },
            strips: Vec::new(),
            strip: None,
            windows: Vec::new(),
            scroll_x: 0.0,
            ribbon_offered: false,
            rail_offered: false,
        }
    }

    /// The frame of `tree` in `content`, for a host that draws the desktop
    /// in one area: the active workspace's strips, the shown strip's
    /// windows, and which of them are on screen (`shown`: each window's
    /// active tab and where it was laid out). Offered unless `covered`.
    pub fn of_tree(
        tree: &DesktopTree,
        content: Rect,
        (shown, scroll_x): (&[(TabId, Rect)], f64),
        covered: bool,
    ) -> Self {
        let Some(workspace) = tree.active_workspace() else {
            return Self::empty(content);
        };
        let focused_strip = tree
            .active_strip
            .get(&workspace.id)
            .and_then(|id| workspace.strips.iter().position(|s| s.id == *id))
            .unwrap_or(0);
        let strip = workspace.strips.get(focused_strip);
        let groups = strip.map_or(&[][..], |strip| strip.window_groups.as_slice());
        let focused = strip
            .and_then(|strip| tree.active_group_for(strip.id))
            .and_then(|group| groups.iter().position(|g| g.id == group))
            .unwrap_or(0);
        let visible = groups
            .iter()
            .enumerate()
            .filter(|(_, group)| {
                shown.iter().any(|(tab, rect)| {
                    *tab == group.active_tab && rect.x1 > content.x0 && rect.x0 < content.x1
                })
            })
            .map(|(i, _)| i)
            .collect();
        Self {
            ribbon: StripRibbon {
                content,
                count: workspace.strips.len(),
                focused: focused_strip,
            },
            rail: StripRail {
                content,
                count: groups.len(),
                focused,
                visible,
            },
            strips: workspace.strips.iter().map(|strip| strip.id).collect(),
            strip: strip.map(|strip| strip.id),
            windows: groups.iter().map(|group| group.id).collect(),
            scroll_x,
            ribbon_offered: !covered && !workspace.strips.is_empty(),
            rail_offered: !covered,
        }
    }

    /// The ribbon's pointer band: resting there opens the labels.
    pub fn zone(&self) -> Rect {
        match (
            self.ribbon.hit_rect(0),
            self.ribbon.hit_rect(self.ribbon.count.saturating_sub(1)),
        ) {
            (Some(first), Some(last)) if self.ribbon_offered => first.union(last),
            _ => Rect::ZERO,
        }
    }

    fn position(&self, hovered: bool) -> StripPosition {
        StripPosition {
            strip: self.strip,
            ribbon: (self.ribbon.count, self.ribbon.focused),
            rail: (self.rail.count, self.rail.focused),
            rail_visible: self.rail.visible.clone(),
            scroll_half_px: half_px_steps(self.scroll_x),
            hovered,
        }
    }
}

/// Who is looking: the person the desktop acts for and their id on this
/// device, with which strips they may name and remove, and whether they
/// may add one.
pub struct Viewer<'a> {
    pub local_user: &'a str,
    pub own_id: &'a str,
    pub may_rename: &'a dyn Fn(StripId) -> bool,
    pub may_add: bool,
}

/// The ribbon, the rail and the switcher between frames.
#[derive(Default)]
pub struct StripNavigator {
    switcher: StripSwitcher,
    indicators: StripIndicators,
    ribbon_focus: SlidingFocus,
    rail_focus: SlidingFocus,
    /// The position last read, which the pointer on the ribbon or rail
    /// holds up.
    position: Option<StripPosition>,
    ribbon_hover: Option<usize>,
    rail_hover: Option<usize>,
    /// A press the navigator took: its release is the navigator's too.
    owns_release: bool,
}

impl StripNavigator {
    /// Let the switcher's painted name field take a pointer event.
    pub fn field_pointer(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        self.switcher.field_pointer(event, scale_factor, modifiers)
    }

    /// The switcher, for hosts and harnesses that read its labels.
    pub fn switcher(&self) -> &StripSwitcher {
        &self.switcher
    }

    /// The strips the labels list, and who is on each.
    pub fn set_model(&mut self, model: SwitcherModel) {
        self.switcher.set_model(model);
    }

    /// The labels' model for `tree` as `viewer` sees it.
    pub fn model_of(tree: &DesktopTree, viewer: &Viewer<'_>) -> SwitcherModel {
        SwitcherModel {
            can_add: viewer.may_add,
            ..SwitcherModel::of(tree, viewer.local_user, viewer.own_id, viewer.may_rename)
        }
    }

    /// Read `frame` at `now`: a change from what was last read is
    /// navigation, and shows the ribbon and rail. Closes the labels when the
    /// ribbon is not offered; returns whether it did.
    pub fn observe(&mut self, frame: &StripFrame, now: Instant) -> bool {
        let closed = !frame.ribbon_offered && self.switcher.is_open();
        if closed {
            self.switcher.close();
        }
        self.switcher.place(&frame.ribbon);
        let position = frame.position(self.hovered());
        self.position = Some(position.clone());
        self.indicators.observe(position, now);
        closed
    }

    /// A direct pan came to rest at `frame`, on a frame that must not change
    /// the chrome: the ribbon and rail show from the next frame on.
    pub fn settle_after_pan(&mut self, frame: &StripFrame) {
        let position = frame.position(self.hovered());
        self.position = Some(position.clone());
        self.indicators.settle_after_pan(position);
    }

    fn hovered(&self) -> bool {
        self.ribbon_hover.is_some() || self.rail_hover.is_some()
    }

    /// The pointer came onto the ribbon or rail, or left them: they stay
    /// while it is on them, and the hold restarts when it leaves.
    fn hover_changed(&mut self, now: Instant) {
        if let Some(mut position) = self.position.clone() {
            position.hovered = self.hovered();
            self.position = Some(position.clone());
            self.indicators.observe(position, now);
        }
    }

    /// The ribbon mark under the pointer.
    pub fn ribbon_hover(&self) -> Option<usize> {
        self.ribbon_hover
    }

    /// The rail segment under the pointer.
    pub fn rail_hover(&self) -> Option<usize> {
        self.rail_hover
    }

    /// The pointer moved to `p`. Returns whether anything visible changed.
    pub fn pointer_moved(&mut self, frame: &StripFrame, p: Point, now: Instant) -> bool {
        let hover = (
            frame.ribbon_offered.then(|| frame.ribbon.hit(p)).flatten(),
            frame.rail_offered.then(|| frame.rail.hit(p)).flatten(),
        );
        let changed = hover != (self.ribbon_hover, self.rail_hover);
        (self.ribbon_hover, self.rail_hover) = hover;
        if changed {
            self.hover_changed(now);
        }
        let zone = frame.zone();
        if zone.area() <= 0.0 && !self.switcher.is_open() {
            return changed;
        }
        self.switcher.place(&frame.ribbon);
        self.switcher.pointer_moved(p, now, zone) || changed
    }

    pub fn pointer_left(&mut self, now: Instant) -> bool {
        let hovered = self.ribbon_hover.take().is_some() | self.rail_hover.take().is_some();
        if hovered {
            self.hover_changed(now);
        }
        self.switcher.pointer_left() || hovered
    }

    /// A left press at `p` on the open labels: `None` when it is not
    /// theirs, else what to do (possibly nothing).
    pub fn press_labels(
        &mut self,
        frame: &StripFrame,
        p: Point,
        now: Instant,
    ) -> Option<Option<StripNavigation>> {
        if !frame.ribbon_offered || !self.switcher.is_open() {
            return None;
        }
        self.switcher.place(&frame.ribbon);
        let answer = match self.switcher.press(p, now) {
            Press::Outside => return None,
            Press::Consumed => None,
            Press::Command(command) => Some(StripNavigation::Strip(command)),
        };
        self.owns_release = true;
        Some(answer)
    }

    /// A tap at `p` on a ribbon mark while the labels are closed: it opens
    /// them pinned, so a finger, which cannot rest there, reaches every
    /// strip and its actions. Returns whether it did.
    pub fn open_from_ribbon(&mut self, frame: &StripFrame, p: Point, now: Instant) -> bool {
        if !frame.ribbon_offered || self.switcher.is_open() || frame.ribbon.hit(p).is_none() {
            return false;
        }
        self.switcher.place(&frame.ribbon);
        self.switcher.open_pinned(now);
        self.switcher.place(&frame.ribbon);
        true
    }

    /// A long-press, or a secondary click, at `p`: on a strip's label (or
    /// its ribbon mark, opening the labels) it puts that strip's actions
    /// beside the label. `None` when it is not the navigator's.
    pub fn secondary_press(
        &mut self,
        frame: &StripFrame,
        p: Point,
        now: Instant,
    ) -> Option<Option<StripNavigation>> {
        if !frame.ribbon_offered {
            return None;
        }
        if !self.switcher.is_open() && !self.open_from_ribbon(frame, p, now) {
            return None;
        }
        self.switcher.place(&frame.ribbon);
        let answer = self.switcher.secondary_press(p);
        self.switcher.place(&frame.ribbon);
        match answer {
            Press::Outside => None,
            Press::Consumed | Press::Command(_) => {
                self.owns_release = true;
                Some(None)
            }
        }
    }

    /// The workspace menu's New strip: the labels open pinned and a strip
    /// is made, whose name then goes in place for typing. `None` where the
    /// switcher is not offered.
    pub fn new_strip(&mut self, frame: &StripFrame, now: Instant) -> Option<StripNavigation> {
        if !frame.ribbon_offered {
            return None;
        }
        self.switcher.place(&frame.ribbon);
        let answer = self.switcher.begin_new_strip(now);
        self.switcher.place(&frame.ribbon);
        match answer {
            Press::Command(command) => Some(StripNavigation::Strip(command)),
            Press::Consumed | Press::Outside => None,
        }
    }

    /// The strip whose ribbon mark is at `p`: what a press there is about
    /// (a press itself opens the labels, [`Self::open_from_ribbon`]).
    pub fn ribbon_target(frame: &StripFrame, p: Point) -> Option<StripNavigation> {
        let i = frame
            .ribbon_offered
            .then(|| frame.ribbon.hit(p))
            .flatten()?;
        let strip = *frame.strips.get(i)?;
        Some(StripNavigation::Strip(Command::Focus(strip)))
    }

    /// The window a press at `p` on a rail segment focuses.
    pub fn rail_target(frame: &StripFrame, p: Point) -> Option<StripNavigation> {
        let i = frame.rail_offered.then(|| frame.rail.hit(p)).flatten()?;
        Some(StripNavigation::Window {
            strip: frame.strip?,
            group: *frame.windows.get(i)?,
        })
    }

    /// A left press at `p`: on the open labels, a ribbon mark (which opens
    /// them) or a rail segment. `None` when it is none of them and goes on
    /// to what is under it, else what to do (possibly nothing).
    pub fn press(
        &mut self,
        frame: &StripFrame,
        p: Point,
        now: Instant,
    ) -> Option<Option<StripNavigation>> {
        if let Some(answer) = self.press_labels(frame, p, now) {
            return Some(answer);
        }
        if self.open_from_ribbon(frame, p, now) {
            self.owns_release = true;
            return Some(None);
        }
        let target = Self::rail_target(frame, p)?;
        self.owns_release = true;
        Some(Some(target))
    }

    /// A left release: whether its press was the navigator's.
    pub fn release(&mut self) -> bool {
        std::mem::take(&mut self.owns_release)
    }

    /// Whether a wheel at `p` is the navigator's: over the ribbon or the
    /// open labels.
    pub fn takes_wheel_at(&self, frame: &StripFrame, p: Point) -> bool {
        frame.ribbon_offered && (frame.zone().contains(p) || self.switcher.target_at(p).is_some())
    }

    /// A wheel or two-finger scroll at `p`: over the ribbon or the labels it
    /// steps through the strips. `None` when it is not the navigator's.
    pub fn wheel(
        &mut self,
        frame: &StripFrame,
        p: Point,
        delta: MouseScrollDelta,
        now: Instant,
    ) -> Option<Option<StripNavigation>> {
        if !self.takes_wheel_at(frame, p) {
            return None;
        }
        let command = self.switcher.scroll(p, delta, now, frame.zone());
        self.switcher.place(&frame.ribbon);
        Some(command.map(StripNavigation::Strip))
    }

    pub fn is_open(&self) -> bool {
        self.switcher.is_open()
    }

    /// Close the labels, dropping a name being typed.
    pub fn close(&mut self) {
        self.switcher.close();
    }

    /// Whether the keyboard is the switcher's: opened for choosing, or a
    /// name is typed.
    pub fn takes_keys(&self) -> bool {
        self.switcher.takes_keys()
    }

    /// A key while [`Self::takes_keys`].
    pub fn key(
        &mut self,
        frame: &StripFrame,
        key: &Key,
        mods: ModifiersState,
    ) -> Option<StripNavigation> {
        let answer = self.switcher.key(key, mods);
        self.switcher.place(&frame.ribbon);
        match answer {
            KeyAnswer::Command(command) => Some(StripNavigation::Strip(command)),
            KeyAnswer::Consumed | KeyAnswer::Ignored => None,
        }
    }

    /// Text an input method committed while a name is typed.
    pub fn commit_text(&mut self, text: &str) -> bool {
        self.switcher.commit_text(text)
    }

    /// Open the labels for choosing, as a menu, on the shown strip.
    pub fn open_for_choosing(&mut self, frame: &StripFrame, now: Instant) {
        if frame.ribbon_offered {
            self.switcher.place(&frame.ribbon);
            self.switcher.open_for_choosing(now);
            self.switcher.place(&frame.ribbon);
        }
    }

    /// Open the labels with the shown strip's name in place for typing.
    pub fn rename_focused(&mut self, frame: &StripFrame, now: Instant) {
        if frame.ribbon_offered {
            self.switcher.place(&frame.ribbon);
            self.switcher.rename_focused(now);
            self.switcher.place(&frame.ribbon);
        }
    }

    /// Open or close the labels for the keyboard.
    pub fn toggle_from_keyboard(&mut self, frame: &StripFrame, now: Instant) {
        if frame.ribbon_offered {
            self.switcher.place(&frame.ribbon);
            self.switcher.toggle_from_keyboard(now);
            self.switcher.place(&frame.ribbon);
        }
    }

    /// Advance to `now` without input: a pointer at rest on the ribbon
    /// opens the labels. Returns whether anything visible changed.
    pub fn settle(&mut self, frame: &StripFrame, now: Instant) -> bool {
        self.switcher.place(&frame.ribbon);
        let changed = self.switcher.settle(now, frame.zone());
        if changed {
            self.switcher.place(&frame.ribbon);
        }
        changed
    }

    /// The ribbon mark to light: the one whose label the pointer or the
    /// keyboard is on while the labels are open, else the one under the
    /// pointer.
    pub fn lit_strip(&self) -> Option<usize> {
        self.switcher.lit_strip().or(self.ribbon_hover)
    }

    /// The ribbon and rail's navigation opacity at `now`: shown on a change,
    /// then fading; nothing where the rail is not offered.
    pub fn indicator_alpha_at(&self, frame: &StripFrame, now: Instant, reduce_motion: bool) -> f32 {
        if frame.rail_offered {
            self.indicators.alpha_at(now, reduce_motion)
        } else {
            0.0
        }
    }

    /// The ribbon's opacity at `now`: whole while the labels are open, the
    /// navigation fade while moving, and the quiet resting mark otherwise.
    pub fn ribbon_alpha_at(&self, frame: &StripFrame, now: Instant, reduce_motion: bool) -> f32 {
        if !frame.ribbon_offered || frame.ribbon.count == 0 {
            return 0.0;
        }
        if self.switcher.is_open() {
            return 1.0;
        }
        // One strip is no news: a lone mark said nothing and read as a
        // stray sliver at the screen's edge. It shows while the pointer
        // reaches for it (and while moving), as a second strip does at
        // rest.
        let rest = if frame.ribbon.count > 1 {
            RIBBON_REST_ALPHA
        } else {
            0.0
        };
        self.indicators.alpha_at(now, reduce_motion).max(rest)
    }

    /// Where the ribbon's long segment is drawn at `now` while it slides to
    /// the strip that took focus; `None` at rest.
    pub fn ribbon_focus_at(&self, now: Instant) -> Option<f64> {
        self.ribbon_focus.sliding_at(now)
    }

    /// Paint the open labels; their extent, for damage, when painted.
    pub fn draw_labels(
        &mut self,
        frame: &StripFrame,
        cx: &mut DrawCx<'_>,
        now: Instant,
    ) -> Option<Rect> {
        if !frame.ribbon_offered || !self.switcher.is_open() {
            return None;
        }
        self.switcher
            .place_sliding(&frame.ribbon, self.ribbon_focus.sliding_at(now));
        self.switcher.draw(cx, now);
        Some(self.switcher.bounds())
    }

    /// Paint the ribbon; its extent, for damage, when painted. Who is on a
    /// strip shows in the open labels only: the people are the bar's faces
    /// everywhere else.
    pub fn draw_ribbon(
        &mut self,
        frame: &StripFrame,
        cx: &mut DrawCx<'_>,
        now: Instant,
    ) -> Option<Rect> {
        let ribbon = frame.ribbon;
        let focus =
            self.ribbon_focus
                .follow((ribbon.count, ribbon.focused), cx.theme.motion(), now);
        let alpha = self.ribbon_alpha_at(frame, now, cx.theme.reduce_motion);
        if alpha <= 0.0 {
            return None;
        }
        ribbon.draw_at(
            cx.scene,
            cx.transform,
            cx.theme,
            (self.lit_strip(), alpha),
            focus,
        );
        self.ribbon_bounds(frame)
    }

    /// Paint the rail; its extent, for damage, when painted.
    pub fn draw_rail(
        &mut self,
        frame: &StripFrame,
        cx: &mut DrawCx<'_>,
        now: Instant,
    ) -> Option<Rect> {
        let rail = &frame.rail;
        let focus = self
            .rail_focus
            .follow((rail.count, rail.focused), cx.theme.motion(), now);
        let alpha = self.indicator_alpha_at(frame, now, cx.theme.reduce_motion);
        if alpha <= 0.0 || !rail.is_meaningful() {
            return None;
        }
        rail.draw_at(
            cx.scene,
            cx.transform,
            cx.theme,
            (self.rail_hover, alpha),
            focus,
        );
        self.rail_bounds(frame)
    }

    /// Paint the labels, the ribbon and the rail.
    pub fn draw(
        &mut self,
        frame: &StripFrame,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        self.settle(frame, now);
        let mut cx = DrawCx {
            scene,
            transform,
            text,
            theme,
        };
        self.draw_labels(frame, &mut cx, now);
        self.draw_ribbon(frame, &mut cx, now);
        self.draw_rail(frame, &mut cx, now);
    }

    /// The open labels' extent, shadows included.
    pub fn labels_bounds(&self) -> Rect {
        self.switcher.bounds()
    }

    /// The ribbon's extent: its marks, and nothing beside them.
    pub fn ribbon_bounds(&self, frame: &StripFrame) -> Option<Rect> {
        segments_bounds([
            frame.ribbon.segment_rect(0),
            frame
                .ribbon
                .segment_rect(frame.ribbon.count.saturating_sub(1)),
        ])
    }

    /// The rail's extent, `None` when it has nothing to show.
    pub fn rail_bounds(&self, frame: &StripFrame) -> Option<Rect> {
        if !frame.rail.is_meaningful() {
            return None;
        }
        segments_bounds([
            frame.rail.segment_rect(0),
            frame.rail.segment_rect(frame.rail.count.saturating_sub(1)),
        ])
    }

    /// Whether the labels owe a frame: fading in.
    pub fn labels_need_animation_frame(&self, now: Instant) -> bool {
        self.switcher.needs_animation_frame(now)
    }

    /// Whether the ribbon or rail owe a frame: a fade or a slide.
    pub fn indicators_need_animation_frame(&self, now: Instant) -> bool {
        self.indicators.needs_animation_frame_at(now)
            || self.ribbon_focus.is_moving(now)
            || self.rail_focus.is_moving(now)
    }

    /// Whether the next frame is owed now.
    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.labels_need_animation_frame(now) || self.indicators_need_animation_frame(now)
    }

    /// When the ribbon and rail next change with no input: the fade's start.
    pub fn indicators_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.indicators.next_wake_deadline_at(now)
    }

    /// When the labels next change with no input: a resting pointer
    /// opening them or lighting one.
    pub fn labels_wake_deadline(&self, frame: &StripFrame, now: Instant) -> Option<Instant> {
        self.switcher.deadline(now, frame.zone())
    }

    /// When the navigator next changes with no input.
    pub fn next_wake_deadline(&self, frame: &StripFrame, now: Instant) -> Option<Instant> {
        [
            self.indicators_wake_deadline(now),
            self.labels_wake_deadline(frame, now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Everything the open labels would take beside the ribbon: where
    /// nothing else may wait at that edge, open or not.
    pub fn labels_extent(&self, frame: &StripFrame) -> Rect {
        self.switcher.extent_beside(&frame.ribbon)
    }

    /// The labels for assistive technology, while open.
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.switcher.accessibility()
    }

    /// Named targets for harnesses: the ribbon, and each label when open.
    pub fn test_rects(&self, frame: &StripFrame) -> Vec<(String, Rect)> {
        self.switcher.test_rects(frame.zone())
    }

    /// Activate the switcher's semantic target `target` as a press would.
    pub fn activate_target(
        &mut self,
        frame: &StripFrame,
        target: &str,
    ) -> Option<Option<StripNavigation>> {
        self.switcher.place(&frame.ribbon);
        let answer = self.switcher.activate_target(target)?;
        self.switcher.place(&frame.ribbon);
        Some(match answer {
            Press::Command(command) => Some(StripNavigation::Strip(command)),
            Press::Consumed | Press::Outside => None,
        })
    }

    /// The switcher laid out beside `frame`'s ribbon, for tests.
    pub fn switcher_for_tests(&mut self, frame: &StripFrame) -> &mut StripSwitcher {
        self.switcher.place(&frame.ribbon);
        &mut self.switcher
    }
}

/// A scroll offset in half-pixel steps: finer than a person sees, coarse
/// enough that layout noise is no navigation.
pub fn half_px_steps(scroll_x: f64) -> i64 {
    let steps = (scroll_x * 2.0).round();
    if steps.is_finite() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a strip offset is far inside i64; non-finite values are excluded above"
        )]
        let steps = steps as i64;
        steps
    } else {
        0
    }
}

/// The extent of a run's first and last segments, a pixel around.
fn segments_bounds(segments: [Option<Rect>; 2]) -> Option<Rect> {
    segments
        .into_iter()
        .flatten()
        .map(|rect| Rect::new(rect.x0 - 1.0, rect.y0 - 1.0, rect.x1 + 1.0, rect.y1 + 1.0))
        .reduce(|current, rect| current.union(rect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{
        Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };
    use ui::time::Duration;

    const CONTENT: Rect = Rect::new(0.0, 40.0, 1280.0, 800.0);

    fn group(id: u64) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: id.saturating_add(1000),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: vec![Tab {
                id: id.saturating_add(1000),
                title: "Notes".into(),
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
                app_title: None,
            }],
        }
    }

    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
        let mut main = Strip::new(StripId(10), "Main");
        main.window_groups = vec![group(1), group(2), group(3)];
        workspace.strips = vec![main, Strip::new(StripId(11), "Later")];
        let mut tree = DesktopTree {
            workspaces: vec![workspace],
            active: WorkspaceId(1),
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceId(1), StripId(10));
        tree
    }

    fn viewer(may: &dyn Fn(StripId) -> bool) -> Viewer<'_> {
        Viewer {
            local_user: "me",
            own_id: "me",
            may_rename: may,
            may_add: true,
        }
    }

    fn frame(tree: &DesktopTree, shown: &[(TabId, Rect)], scroll: f64) -> StripFrame {
        StripFrame::of_tree(tree, CONTENT, (shown, scroll), false)
    }

    fn navigator(tree: &DesktopTree, t: Instant) -> (StripNavigator, StripFrame) {
        let mut nav = StripNavigator::default();
        nav.set_model(StripNavigator::model_of(tree, &viewer(&|_| true)));
        let frame = frame(tree, &[(1001, Rect::new(40.0, 60.0, 600.0, 780.0))], 0.0);
        nav.observe(&frame, t);
        (nav, frame)
    }

    /// The ribbon says which strip of how many, the rail which window of the
    /// strip and which are on screen.
    #[test]
    fn it_reads_the_strips_and_the_shown_strips_windows() {
        let (nav, frame) = navigator(&tree(), Instant::now());
        assert_eq!((frame.ribbon.count, frame.ribbon.focused), (2, 0));
        assert_eq!(frame.rail.count, 3);
        assert_eq!(frame.rail.visible, vec![0]);
        let removable: Vec<bool> = nav
            .switcher()
            .model()
            .strips
            .iter()
            .map(|s| s.removable)
            .collect();
        assert_eq!(removable, vec![false, true], "only an empty strip may go");
    }

    /// Hidden at rest; a strip or window change shows the ribbon and rail,
    /// which fade once it stops; the pointer on them holds them up.
    #[test]
    fn a_strip_or_window_change_shows_where_you_are() {
        let t0 = Instant::now();
        let mut tree = tree();
        let (mut nav, frame0) = navigator(&tree, t0);
        assert_eq!(
            nav.indicator_alpha_at(&frame0, t0, false),
            0.0,
            "not at rest"
        );

        let t1 = t0 + Duration::from_millis(30);
        tree.active_group.insert(StripId(10), 2);
        let shown = [(1002, Rect::new(40.0, 60.0, 600.0, 780.0))];
        let moved = frame(&tree, &shown, 400.0);
        nav.observe(&moved, t1);
        assert_eq!(
            nav.indicator_alpha_at(&moved, t1, false),
            1.0,
            "a window change shows it"
        );
        assert_eq!(moved.rail.focused, 1);

        let later = t1 + Duration::from_secs(10);
        nav.observe(&moved, later);
        assert_eq!(
            nav.indicator_alpha_at(&moved, later, false),
            0.0,
            "and it fades"
        );

        let mark = moved.ribbon.segment_rect(0).unwrap().center();
        nav.pointer_moved(&moved, mark, later);
        let much_later = later + Duration::from_secs(10);
        assert_eq!(
            nav.indicator_alpha_at(&moved, much_later, false),
            1.0,
            "the pointer on the ribbon holds it up"
        );
        nav.pointer_left(much_later);

        let t2 = much_later + Duration::from_secs(10);
        tree.active_strip.insert(WorkspaceId(1), StripId(11));
        let other = frame(&tree, &[], 0.0);
        nav.observe(&other, t2);
        assert_eq!(
            nav.indicator_alpha_at(&other, t2, false),
            1.0,
            "a strip change shows it"
        );
        assert_eq!(other.ribbon.focused, 1);
        let covered = StripFrame::of_tree(&tree, CONTENT, (&[], 0.0), true);
        assert_eq!(
            nav.indicator_alpha_at(&covered, t2, false),
            0.0,
            "nothing while covered"
        );
    }

    /// A press on a ribbon mark opens the labels, pinned, and one on a
    /// label then goes to its strip; one on a rail segment goes to its
    /// window; elsewhere the press is not the navigator's.
    #[test]
    fn the_ribbon_and_rail_are_controls() {
        let t0 = Instant::now();
        let tree = tree();
        let (mut nav, frame) = navigator(&tree, t0);
        let mark = frame.ribbon.segment_rect(1).unwrap().center();
        assert_eq!(nav.press(&frame, mark, t0), Some(None));
        assert!(nav.release());
        assert!(nav.is_open(), "a tap on the ribbon opens the labels");
        let label = nav
            .switcher()
            .layout()
            .label_rect(crate::strip_switcher::Row::Strip(1))
            .unwrap()
            .center();
        assert_eq!(
            nav.press(&frame, label, t0),
            Some(Some(StripNavigation::Strip(Command::Focus(StripId(11)))))
        );
        assert!(nav.release());
        assert!(!nav.is_open(), "the choice closes labels a tap opened");
        let segment = frame.rail.segment_rect(2).unwrap().center();
        assert_eq!(
            nav.press(&frame, segment, t0),
            Some(Some(StripNavigation::Window {
                strip: StripId(10),
                group: 3
            }))
        );
        assert_eq!(nav.press(&frame, Point::new(640.0, 400.0), t0), None);
        let covered = StripFrame::of_tree(&tree, CONTENT, (&[], 0.0), true);
        assert_eq!(
            nav.press(&covered, mark, t0),
            None,
            "nothing is taken while covered"
        );
    }

    /// The labels open for choosing; Delete removes an empty strip, through
    /// the command the host applies.
    #[test]
    fn the_labels_list_switch_and_remove_strips() {
        let t0 = Instant::now();
        let (mut nav, frame) = navigator(&tree(), t0);
        nav.open_for_choosing(&frame, t0);
        assert!(nav.takes_keys());
        let none = ModifiersState::empty();
        let down = Key::Named(winit::keyboard::NamedKey::ArrowDown);
        assert_eq!(nav.key(&frame, &down, none), None);
        let delete = Key::Named(winit::keyboard::NamedKey::Delete);
        assert_eq!(
            nav.key(&frame, &delete, none),
            Some(StripNavigation::Strip(Command::Remove(StripId(11))))
        );
        // A strip with windows is not offered for removal.
        let up = Key::Named(winit::keyboard::NamedKey::ArrowUp);
        nav.key(&frame, &up, none);
        assert_eq!(nav.key(&frame, &delete, none), None);
        let enter = Key::Named(winit::keyboard::NamedKey::Enter);
        assert_eq!(
            nav.key(&frame, &enter, none),
            Some(StripNavigation::Strip(Command::Focus(StripId(10))))
        );
    }

    #[test]
    fn long_press_reaches_the_strip_actions_and_tap_takes_them() {
        use crate::strip_switcher::Row;
        let t0 = Instant::now();
        let (mut nav, frame) = navigator(&tree(), t0);
        let mark = frame.ribbon.segment_rect(1).unwrap().center();
        // No pointer ever rests anywhere: a long-press on the ribbon opens
        // the labels and puts that strip's actions beside its label.
        assert_eq!(nav.secondary_press(&frame, mark, t0), Some(None));
        assert!(nav.release());
        assert!(nav.is_open() && nav.takes_keys());
        assert_eq!(nav.switcher().armed(), Some(1));
        let rects = nav.test_rects(&frame);
        let named = |name: &str| {
            rects
                .iter()
                .find(|(n, _)| n == name)
                .map_or_else(|| panic!("{name} in {rects:?}"), |(_, r)| *r)
        };
        let rename = named("strip_switcher.rename.2");
        let remove = named("strip_switcher.remove.2");
        let label = nav.switcher().layout().label_rect(Row::Strip(1)).unwrap();
        for action in [rename, remove] {
            assert!(action.x1 <= label.x0, "beside the label: {action:?}");
            assert!((action.center().y - label.center().y).abs() < 0.5);
        }
        // A tap takes Remove: the empty strip goes.
        assert_eq!(
            nav.press(&frame, remove.center(), t0),
            Some(Some(StripNavigation::Strip(Command::Remove(StripId(11)))))
        );
        assert!(nav.release());
        // A long-press on a label, then a tap on Rename: its name is in
        // place for typing.
        nav.secondary_press(&frame, label.center(), t0);
        let rename = nav
            .test_rects(&frame)
            .into_iter()
            .find(|(n, _)| n == "strip_switcher.rename.2")
            .map(|(_, r)| r)
            .unwrap();
        assert_eq!(nav.press(&frame, rename.center(), t0), Some(None));
        assert!(nav.switcher().is_naming());
        // A strip with windows offers Rename only: nothing is lost.
        let main = nav.switcher().layout().label_rect(Row::Strip(0)).unwrap();
        nav.secondary_press(&frame, main.center(), t0);
        let offered: Vec<String> = nav.test_rects(&frame).into_iter().map(|(n, _)| n).collect();
        assert!(offered.iter().any(|n| n == "strip_switcher.rename.1"));
        assert!(!offered.iter().any(|n| n == "strip_switcher.remove.1"));
        // Escape puts the actions away first, then the labels.
        let escape = Key::Named(winit::keyboard::NamedKey::Escape);
        nav.key(&frame, &escape, ModifiersState::empty());
        assert_eq!(nav.switcher().armed(), None);
        assert!(nav.is_open());
        nav.key(&frame, &escape, ModifiersState::empty());
        assert!(!nav.is_open());
        // New strip, from the workspace menu or a tap on its label, makes
        // the strip and puts its name in place.
        assert_eq!(
            nav.new_strip(&frame, t0),
            Some(StripNavigation::Strip(Command::Create("Strip 3".into())))
        );
        assert!(nav.is_open() && nav.takes_keys());
    }

    /// Who is on a strip shows in its label only: the ribbon at rest paints
    /// its marks and nothing beside them.
    #[test]
    fn the_ribbon_at_rest_paints_no_faces() {
        let t0 = Instant::now();
        let (nav, frame) = navigator(&tree(), t0);
        let ribbon = nav.ribbon_bounds(&frame).unwrap();
        let first = frame.ribbon.segment_rect(0).unwrap();
        assert!(
            ribbon.x0 >= first.x0 - 1.0 - 1e-6,
            "nothing reaches left of the marks: {ribbon:?} vs {first:?}"
        );
    }
}
