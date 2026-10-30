//! Showing a setting asked for from search, the palette or an agent: once
//! its pane is built, its control takes the keyboard and is scrolled into
//! sight, a little below the top so its card's heading shows too; it stays
//! in sight through relayouts until the person acts, and its row is marked
//! for a moment as a found match is. A capture puts the keyboard on a
//! control the same way, and a pane rebuilt under the keyboard keeps it on
//! the control it was on.
//!
//! This is a piece of Settings' own around its shell ([`Reveal`]): the
//! toolkit can put the keyboard on a widget and scroll a scroller, but it
//! has no way to say "the control of this setting, once the part that shows
//! it is built and read". A setting of the index names several controls
//! ([`settings_store::index::Setting::is_control`]), and whether its pane
//! is built as it will show depends on Settings' own reads (an area's first
//! reading of the device): neither is anything the toolkit knows of.

use std::cell::Cell;

use settings_store::index::Setting;
use ui::widget::{Element, Scrollable, Widget};

/// How long a shown setting's row stays marked, in seconds.
const HIGHLIGHT_HOLD: f32 = 1.5;

/// What the window asks [`Reveal`] to show, shared with it.
#[derive(Default)]
pub(crate) struct RevealAsks {
    /// The setting asked to be shown, until its pane is built.
    pub(crate) setting: Cell<Option<&'static Setting>>,
    /// The setting whose control took the keyboard, to scroll to once laid
    /// out.
    scroll: Cell<Option<&'static Setting>>,
    /// The setting shown, kept in sight until the person acts.
    pub(crate) kept: Cell<Option<&'static Setting>>,
    /// The control a capture puts the keyboard on, once its pane is built.
    pub(crate) control: Cell<Option<&'static str>>,
}

impl RevealAsks {
    /// The setting asked to be shown that is not in sight yet.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> Option<&'static str> {
        self.setting
            .get()
            .or_else(|| self.scroll.get())
            .map(|setting| setting.id)
    }
}

/// The window's content, showing what [`RevealAsks`] asks once `settled`
/// says the pane is built as it will show. See the [module docs](self).
pub(crate) struct Reveal {
    asks: std::rc::Rc<RevealAsks>,
    settled: Box<dyn Fn() -> bool>,
    /// The row of the setting just shown, marked since then.
    highlight: Option<(&'static Setting, ui::time::Instant)>,
    content: Element,
}

impl Reveal {
    pub(crate) fn new(
        asks: std::rc::Rc<RevealAsks>,
        settled: impl Fn() -> bool + 'static,
        content: Element,
    ) -> Self {
        Self {
            asks,
            settled: Box::new(settled),
            highlight: None,
            content,
        }
    }

    /// The row of the setting just shown, while it is marked.
    fn highlighted_row(&self) -> Option<vello::kurbo::Rect> {
        let (setting, since) = self.highlight?;
        let elapsed = ui::time::now()
            .saturating_duration_since(since)
            .as_secs_f32();
        if elapsed >= HIGHLIGHT_HOLD {
            return None;
        }
        row_of(&*self.content, setting)
    }
}

impl ui::widget::Decorator for Reveal {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn name(&self) -> &'static str {
        "SettingsReveal"
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        self.content.take_structure_change()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let settled = (self.settled)();
        // Composed panes recreate their controls on layout, including the
        // follow-up layout that scrolling a revealed control can request.
        // Keep the current control's identity, not the old focus ordinal or
        // the last reveal request: either could steal focus after a move.
        let focused = focused_control_id(&*self.content).map(str::to_owned);
        let node = self.content.build_layout(taffy, theme, node_map);
        let content = &mut *self.content;
        // The pane asked to show a setting is built: its control takes the
        // keyboard, which the rebuild keeps on it. A pane still being read
        // (or showing its first reading as on its way) shows its control
        // once it is built; a control the pane does not show (no such
        // device) leaves the page open at its top.
        if let Some(setting) = self.asks.setting.get().filter(|_| settled) {
            self.asks.setting.set(None);
            clear_control_focus(content);
            if focus_control(content, &|id| setting.is_control(id)) {
                self.asks.scroll.set(Some(setting));
            }
        } else if let Some(id) = self.asks.control.get().filter(|_| settled) {
            if focus_control(content, &|control| control == id) {
                self.asks.control.set(None);
            }
        } else if let Some(id) = focused {
            focus_control(content, &|control| control == id);
        }
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.content.apply_layout(taffy, node, origin);
        if let Some(setting) = self.asks.scroll.take() {
            scroll_to_control(&mut *self.content, setting);
            self.highlight = Some((setting, ui::time::now()));
            self.asks.kept.set(Some(setting));
        } else if let Some(setting) = self.asks.kept.get() {
            // Scrolls only once the control has left the view.
            scroll_to_control(&mut *self.content, setting);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        // The row of a setting just shown is marked as a found match is,
        // for a moment.
        match self.highlighted_row() {
            Some(row) => ui::widget::paint_find_match(scene, transform, row, true, theme),
            None => self.highlight = None,
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // The person acts: what they do decides what is in sight now.
        if matches!(
            event,
            winit::event::WindowEvent::KeyboardInput { .. }
                | winit::event::WindowEvent::MouseInput { .. }
                | winit::event::WindowEvent::MouseWheel { .. }
                | winit::event::WindowEvent::Touch(_)
        ) {
            self.asks.kept.set(None);
        }
        self.content.handle_event(event, ctx);
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        self.content.collect_damage(regions);
        // The fading wash over a shown setting's row.
        if let Some((setting, _)) = self.highlight
            && let Some(row) = row_of(&*self.content, setting)
        {
            regions.push(row.inflate(2.0, 2.0));
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.highlight.is_some() || self.content.needs_animation_frame()
    }
}

/// Scroll the innermost scroller holding `setting`'s control under `root`
/// so the control is in sight, a little below the top so its card's heading
/// shows too.
fn scroll_to_control(root: &mut dyn Widget, setting: &'static Setting) {
    fn control_rect(widget: &dyn Widget, setting: &Setting) -> Option<vello::kurbo::Rect> {
        if widget.test_id().is_some_and(|id| setting.is_control(id)) {
            return Some(widget.get_rect());
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| control_rect(child, setting))
    }
    /// Scroll the innermost scroller holding the control; whether one did.
    fn reveal(widget: &mut dyn Widget, setting: &Setting) -> bool {
        if widget
            .children_mut()
            .into_iter()
            .any(|child| reveal(child, setting))
        {
            return true;
        }
        let Some(control) = control_rect(&*widget, setting) else {
            return false;
        };
        let Some(scroll) = widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Scrollable>())
        else {
            return false;
        };
        let viewport = scroll.content_viewport_rect();
        if control.y0 >= viewport.y0 && control.y1 <= viewport.y1 {
            return true;
        }
        let context = (viewport.height() * 0.2).clamp(24.0, 120.0);
        let top = control.y0 - viewport.y0 + f64::from(scroll.offset_y()) - context;
        scroll.scroll_into_view(
            ui::num::f32_of_f64(control.x0 - viewport.x0 + f64::from(scroll.offset_x())),
            ui::num::f32_of_f64(top.max(0.0)),
            1.0,
            ui::num::f32_of_f64(viewport.height()),
        );
        true
    }
    reveal(root, setting);
}

/// The row of `setting`'s control under `root`, within the scroller that
/// holds it: the narrowest ancestor of the control at least as wide as most
/// of that scroller, which is the settings row.
fn row_of(root: &dyn Widget, setting: &Setting) -> Option<vello::kurbo::Rect> {
    fn walk(
        widget: &dyn Widget,
        setting: &Setting,
        trail: &mut Vec<(vello::kurbo::Rect, bool)>,
    ) -> Option<vello::kurbo::Rect> {
        let rect = widget.get_rect();
        if widget.test_id().is_some_and(|id| setting.is_control(id)) {
            let (viewport, at) = trail
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (_, scroller))| *scroller)
                .map(|(at, (viewport, _))| (*viewport, at))?;
            let row = trail
                .get(at..)
                .unwrap_or_default()
                .iter()
                .rev()
                .map(|(rect, _)| *rect)
                .find(|candidate| candidate.width() >= viewport.width() * 0.6)
                .unwrap_or(rect);
            return Some(row.intersect(viewport));
        }
        trail.push((rect, widget.name() == "Scrollable"));
        let found = widget
            .children()
            .into_iter()
            .find_map(|child| walk(child, setting, trail));
        trail.pop();
        found
    }
    walk(root, setting, &mut Vec::new())
}

/// The identity of the current keyboard control, before a pane rebuilds it.
fn focused_control_id(widget: &dyn Widget) -> Option<&str> {
    if widget.is_focusable()
        && widget.is_focused()
        && let Some(id) = widget.test_id()
    {
        return Some(id);
    }
    widget.children().into_iter().find_map(focused_control_id)
}

/// A reveal transfers the keyboard rather than leaving two focused controls.
fn clear_control_focus(widget: &mut dyn Widget) {
    if widget.is_focusable() && widget.is_focused() {
        widget.set_focus(false);
    }
    for child in widget.children_mut() {
        clear_control_focus(child);
    }
}

/// Put the keyboard on the first matching control under `widget`;
/// whether there was one.
fn focus_control(widget: &mut dyn Widget, matches: &dyn Fn(&str) -> bool) -> bool {
    fn first_focusable(widget: &mut dyn Widget) -> Option<&mut dyn Widget> {
        if widget.is_focusable() {
            return Some(widget);
        }
        widget.children_mut().into_iter().find_map(first_focusable)
    }
    if widget.test_id().is_some_and(matches) {
        return match first_focusable(widget) {
            Some(control) => {
                control.set_focus(true);
                true
            }
            None => false,
        };
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| focus_control(child, matches))
}
