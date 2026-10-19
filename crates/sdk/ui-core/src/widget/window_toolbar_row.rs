//! The app's slots in a window's bar ([`crate::window_toolbar`]): back and
//! forward, the location, and up to three action buttons, laid out across
//! one row and drawn with the window's own icon button
//! ([`super::window_controls::paint_icon_button`]).
//!
//! The location is text at rest. An editable one reads as a field, and a
//! click (or [`ToolbarRow::begin_edit`], the bar's Ctrl+L) turns it into a
//! real [`TextInput`] holding the whole value, all of it selected, with a go
//! button at its end ([`AccessoryAction::Submit`]). Enter or the go button
//! commits the typed text to the app ([`ToolbarEvent::Commit`]); Esc puts the
//! location back.
//!
//! When the window shows another tab, the row's content cross-fades in place
//! ([`crate::motion::CrossFade`]): the outgoing tab's slots fade out where
//! they stood while the incoming ones fade in.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::Widget;
use super::text_input::{AccessoryAction, FieldFace, TextInput};
use super::window_controls::{ButtonFace, CONTROL_GAP, CONTROL_PX, paint_icon_button};
use crate::icons::Icon;
use crate::motion::CrossFade;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::{MotionSource, TextRole};
use crate::window_toolbar::{ToolbarEvent, WindowToolbar};

/// Action targets of the slots, as assistive technology and automation
/// address them.
pub const BACK_TARGET: &str = "window-toolbar-back";
pub const FORWARD_TARGET: &str = "window-toolbar-forward";
pub const LOCATION_TARGET: &str = "window-toolbar-location";
pub const ACTION_TARGET_PREFIX: &str = "window-toolbar-action:";

/// Between the navigation buttons and the location, and between the
/// location and the actions.
const SECTION_GAP: f64 = 8.0;
/// The location's leading icon.
const LOCATION_ICON_PX: f64 = 16.0;
/// A location narrower than this is not shown: the bar keeps its buttons.
const MIN_LOCATION_PX: f64 = 48.0;

/// One slot of the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Back,
    Forward,
    Location,
    Action(usize),
}

/// Where each slot of one toolbar sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Slots {
    pub back: Rect,
    pub forward: Rect,
    pub location: Rect,
    pub actions: Vec<Rect>,
}

impl Slots {
    /// Lay `toolbar`'s slots out across `row`, centred on its centre line:
    /// navigation leading, actions trailing, the location filling between.
    pub(crate) fn lay_out(toolbar: &WindowToolbar, row: Rect) -> Self {
        let mut slots = Self::default();
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return slots;
        }
        let y0 = row.center().y - CONTROL_PX / 2.0;
        let square = |x0: f64| Rect::new(x0, y0, x0 + CONTROL_PX, y0 + CONTROL_PX);
        let mut x0 = row.x0;
        if toolbar.navigation.is_some() {
            slots.back = square(x0);
            slots.forward = square(slots.back.x1 + CONTROL_GAP);
            x0 = slots.forward.x1 + SECTION_GAP;
        }
        let mut x1 = row.x1;
        let mut actions = Vec::with_capacity(toolbar.actions.len());
        for _ in &toolbar.actions {
            let rect = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = rect.x0 - CONTROL_GAP;
            actions.push(rect);
        }
        actions.reverse();
        slots.actions = actions;
        if !toolbar.actions.is_empty() {
            x1 = x1 + CONTROL_GAP - SECTION_GAP;
        }
        if toolbar.location.is_some() && x1 - x0 >= MIN_LOCATION_PX {
            slots.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        slots
    }

    fn hit(&self, p: Point) -> Option<Slot> {
        if self.back.contains(p) {
            return Some(Slot::Back);
        }
        if self.forward.contains(p) {
            return Some(Slot::Forward);
        }
        if let Some(i) = self.actions.iter().position(|r| r.contains(p)) {
            return Some(Slot::Action(i));
        }
        self.location.contains(p).then_some(Slot::Location)
    }
}

/// The slots of the tab the window shows, in one row of its bar.
pub(crate) struct ToolbarRow {
    /// The tab whose slots show, and its toolbar.
    shown: Option<(u64, WindowToolbar)>,
    /// The tab whose slots show (`None`: none), cross-fading on a switch.
    fade: CrossFade<Option<u64>>,
    /// The outgoing tab's slots and where they stood, while they fade out.
    outgoing: Option<(WindowToolbar, Slots)>,
    row: Rect,
    slots: Slots,
    hover: Option<Slot>,
    pressed: Option<Slot>,
    /// The location field, while the location is edited.
    field: Option<TextInput>,
    events: Vec<(u64, ToolbarEvent)>,
}

impl Default for ToolbarRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolbarRow {
    pub(crate) fn new() -> Self {
        Self {
            shown: None,
            fade: CrossFade::new(None),
            outgoing: None,
            row: Rect::ZERO,
            slots: Slots::default(),
            hover: None,
            pressed: None,
            field: None,
            events: Vec::new(),
        }
    }

    /// Show `tab`'s slots (`toolbar`), cross-fading from another tab's.
    pub(crate) fn show(
        &mut self,
        tab: u64,
        toolbar: Option<&WindowToolbar>,
        theme_motion: crate::tokens::EffectiveMotion,
        now: Instant,
    ) {
        let next = toolbar.map(|toolbar| (tab, toolbar.clone()));
        let next_tab = next.as_ref().map(|(tab, _)| *tab);
        let previous_tab = self.shown.as_ref().map(|(tab, _)| *tab);
        if next_tab != previous_tab {
            // Another tab: what the person typed belongs to the tab they
            // typed it in, and goes with it.
            self.field = None;
            self.hover = None;
            self.pressed = None;
            self.outgoing = self
                .shown
                .take()
                .map(|(_, toolbar)| (toolbar, self.slots.clone()));
            self.fade
                .show(next_tab, theme_motion, MotionSource::Pointer, now);
        }
        self.shown = next;
    }

    /// The toolbar showing, if any.
    pub(crate) fn toolbar(&self) -> Option<&WindowToolbar> {
        self.shown.as_ref().map(|(_, toolbar)| toolbar)
    }

    /// Lay the slots out across `row`.
    pub(crate) fn lay_out(&mut self, row: Rect) {
        self.row = row;
        self.slots = self
            .toolbar()
            .map(|toolbar| Slots::lay_out(toolbar, row))
            .unwrap_or_default();
        let location = self.slots.location;
        if let Some(field) = self.field.as_mut() {
            if location.area() > 0.0 {
                field.apply_layout_external(location);
            } else {
                // No room for the field any more.
                self.field = None;
            }
        }
    }

    pub(crate) fn slots(&self) -> &Slots {
        &self.slots
    }

    pub(crate) fn row(&self) -> Rect {
        self.row
    }

    /// Whether the location is being edited.
    pub(crate) fn is_editing(&self) -> bool {
        self.field.is_some()
    }

    /// The location field's text, while it is edited.
    #[cfg(test)]
    pub(crate) fn field_text(&self) -> Option<&str> {
        self.field.as_ref().map(TextInput::value)
    }

    /// Turn an editable location into a field holding its whole value, all
    /// of it selected. Returns whether it did.
    pub(crate) fn begin_edit(&mut self) -> bool {
        if self.field.is_some() {
            return true;
        }
        let Some(location) = self
            .toolbar()
            .and_then(|toolbar| toolbar.location.as_ref())
            .filter(|location| location.editable)
        else {
            return false;
        };
        if self.slots.location.area() <= 0.0 {
            return false;
        }
        let go_name = if location.go_name.is_empty() {
            crate::i18n::I18nManager::lookup_plain("window-toolbar-go")
        } else {
            location.go_name.clone()
        };
        let mut field = TextInput::local()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_initial_value(location.edit_value())
            .with_placeholder(location.placeholder.clone())
            .with_accessible_name(location.name.clone())
            .with_enter_submits(true)
            .with_trailing_accessory(Icon::ArrowRight, &go_name, AccessoryAction::Submit);
        field.apply_layout_external(self.slots.location);
        field.set_focus(true);
        field.select_all();
        self.field = Some(field);
        true
    }

    /// Put the location back, discarding what was typed.
    pub(crate) fn cancel_edit(&mut self) -> bool {
        self.field.take().is_some()
    }

    fn shown_tab(&self) -> Option<u64> {
        self.shown.as_ref().map(|(tab, _)| *tab)
    }

    fn push(&mut self, event: ToolbarEvent) {
        if let Some(tab) = self.shown_tab() {
            self.events.push((tab, event));
        }
    }

    /// Commit the field's text to the app.
    fn commit(&mut self) {
        if let Some(field) = self.field.take() {
            let text = field.value().trim().to_owned();
            if !text.is_empty() {
                self.push(ToolbarEvent::Commit { text });
            }
        }
    }

    /// What the person did since the last call, by tab.
    pub(crate) fn take_events(&mut self) -> Vec<(u64, ToolbarEvent)> {
        std::mem::take(&mut self.events)
    }

    fn enabled(&self, slot: &Slot) -> bool {
        let Some(toolbar) = self.toolbar() else {
            return false;
        };
        match slot {
            Slot::Back => toolbar.navigation.is_some_and(|nav| nav.back),
            Slot::Forward => toolbar.navigation.is_some_and(|nav| nav.forward),
            Slot::Location => toolbar.location.is_some(),
            Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|a| a.enabled),
        }
    }

    /// Run `slot` as a press on it does. Returns whether it acted.
    fn activate(&mut self, slot: &Slot) -> bool {
        if !self.enabled(slot) {
            return false;
        }
        match slot {
            Slot::Back => self.push(ToolbarEvent::Back),
            Slot::Forward => self.push(ToolbarEvent::Forward),
            Slot::Location => return self.begin_edit(),
            Slot::Action(i) => {
                let Some(id) = self
                    .toolbar()
                    .and_then(|toolbar| toolbar.actions.get(*i))
                    .map(|action| action.id.clone())
                else {
                    return false;
                };
                self.push(ToolbarEvent::Action { id });
            }
        }
        true
    }

    /// The pointer moved to `p`. Returns whether what shows changed.
    pub(crate) fn pointer_moved(&mut self, p: Point) -> bool {
        let hover = self.slots.hit(p);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    pub(crate) fn pointer_left(&mut self) -> bool {
        let changed = self.hover.is_some() || self.pressed.is_some();
        self.hover = None;
        self.pressed = None;
        changed
    }

    /// Whether `p` is on one of the slots.
    pub(crate) fn hit(&self, p: Point) -> bool {
        self.slots.hit(p).is_some()
    }

    /// A left press at `p`. Returns whether a slot took it. A press
    /// outside the field while it is edited puts the location back.
    pub(crate) fn press(&mut self, p: Point) -> bool {
        let slot = self.slots.hit(p);
        if self.field.is_some() && slot != Some(Slot::Location) {
            self.field = None;
        }
        match slot {
            Some(Slot::Location) if self.field.is_none() => {
                // A click on the location starts editing at once: the field
                // takes the press's place, with everything selected.
                self.begin_edit()
            }
            Some(Slot::Location) => false,
            Some(slot) => {
                self.pressed = Some(slot);
                true
            }
            None => false,
        }
    }

    /// The left button came up at `p`: a press released on the slot it
    /// started on runs it.
    pub(crate) fn release(&mut self, p: Point) -> bool {
        let Some(pressed) = self.pressed.take() else {
            return false;
        };
        if self.slots.hit(p).as_ref() == Some(&pressed) {
            self.activate(&pressed);
        }
        true
    }

    /// Hand an event to the location field while it is edited. Enter and
    /// the go button commit, Esc puts the location back. Returns whether the
    /// field took it.
    pub(crate) fn field_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let Some(field) = self.field.as_mut() else {
            return false;
        };
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && key.logical_key == Key::Named(NamedKey::Escape)
        {
            self.field = None;
            *ctx.handled = true;
            return true;
        }
        let mut handled = false;
        field.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if field.take_submit() {
            self.commit();
            handled = true;
        }
        // The field keeps the keyboard while it is edited: no key reaches
        // the tabs behind it.
        let key = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        if handled || key {
            *ctx.handled = true;
        }
        handled || key
    }

    /// Whether a frame must follow: the cross-fade, or the field's caret.
    pub(crate) fn needs_animation_frame(&self, now: Instant) -> bool {
        self.fade.is_moving(now)
            || self
                .field
                .as_ref()
                .is_some_and(Widget::needs_animation_frame)
    }

    /// Paint the row.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = crate::time::now();
        if let Some((outgoing, opacity)) = self.fade.outgoing(now)
            && outgoing.is_some()
            && let Some((toolbar, slots)) = self.outgoing.as_ref()
        {
            layered(scene, transform, self.row, opacity, |scene| {
                paint_slots(scene, transform, tr, theme, toolbar, slots, None);
            });
        } else if !self.fade.is_moving(now) {
            self.outgoing = None;
        }
        let incoming = self.fade.incoming(now);
        let Some((_, toolbar)) = self.shown.as_ref() else {
            return;
        };
        let interaction = Interaction {
            hover: self.hover.clone(),
            pressed: self.pressed.clone(),
            editing: self.field.is_some(),
        };
        let row = self.row;
        let slots = &self.slots;
        let field = self.field.as_mut();
        layered(scene, transform, row, incoming, |scene| {
            paint_slots(
                scene,
                transform,
                tr,
                theme,
                toolbar,
                slots,
                Some(&interaction),
            );
            if let Some(field) = field {
                Widget::draw(field, scene, transform, tr, theme, presence);
            }
        });
    }

    /// The slots as assistive technology and automation reach them.
    pub(crate) fn accessibility(&self) -> Vec<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let Some(toolbar) = self.toolbar() else {
            return Vec::new();
        };
        let button = |name: String, target: String, rect: Rect, enabled: bool| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(rect)
                .with_state(AccessState {
                    focusable: enabled,
                    disabled: !enabled,
                    ..Default::default()
                })
        };
        let mut nodes = Vec::new();
        if let Some(nav) = toolbar.navigation {
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-back"),
                BACK_TARGET.into(),
                self.slots.back,
                nav.back,
            ));
            nodes.push(button(
                crate::i18n::I18nManager::lookup_plain("window-toolbar-forward"),
                FORWARD_TARGET.into(),
                self.slots.forward,
                nav.forward,
            ));
        }
        if let Some(location) = toolbar.location.as_ref()
            && self.slots.location.area() > 0.0
        {
            if let Some(field) = self.field.as_ref().and_then(Widget::accessibility) {
                nodes.push(field);
            } else {
                let name = if location.name.is_empty() {
                    location.label.clone()
                } else {
                    location.name.clone()
                };
                let mut node = AccessNode::new(if location.editable {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(name)
                .with_optional_value(Some(location.edit_value().to_owned()))
                .with_bounds(self.slots.location);
                if location.editable {
                    node = node
                        .with_action_target(LOCATION_TARGET)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        });
                }
                nodes.push(node);
            }
        }
        for (action, rect) in toolbar.actions.iter().zip(&self.slots.actions) {
            let mut node = button(
                action.name.clone(),
                format!("{ACTION_TARGET_PREFIX}{}", action.id),
                *rect,
                action.enabled,
            );
            if action.on {
                node.role = Role::ToggleButton;
                node.state.checked = Some(true);
            }
            nodes.push(node);
        }
        nodes
    }

    /// Run the slot `target` names, as assistive technology activates it.
    pub(crate) fn perform(&mut self, target: &str) -> bool {
        let slot = match target {
            BACK_TARGET => Slot::Back,
            FORWARD_TARGET => Slot::Forward,
            LOCATION_TARGET => Slot::Location,
            _ => {
                let Some(id) = target.strip_prefix(ACTION_TARGET_PREFIX) else {
                    return false;
                };
                let Some(i) = self
                    .toolbar()
                    .and_then(|toolbar| toolbar.actions.iter().position(|a| a.id == id))
                else {
                    return false;
                };
                Slot::Action(i)
            }
        };
        self.activate(&slot)
    }
}

/// What the person is doing with the row, for its paint.
struct Interaction {
    hover: Option<Slot>,
    pressed: Option<Slot>,
    editing: bool,
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
fn layered(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut Scene),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(scene);
    if part {
        scene.pop_layer();
    }
}

fn paint_slots(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    toolbar: &WindowToolbar,
    slots: &Slots,
    interaction: Option<&Interaction>,
) {
    let face = |slot: Slot, enabled: bool, on: bool| ButtonFace {
        hovered: interaction.is_some_and(|i| i.hover.as_ref() == Some(&slot)),
        pressed: interaction.is_some_and(|i| i.pressed.as_ref() == Some(&slot)),
        on,
        enabled,
    };
    if let Some(nav) = toolbar.navigation {
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.back, Icon::ArrowLeft),
            face(Slot::Back, nav.back, false),
        );
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (slots.forward, Icon::ArrowRight),
            face(Slot::Forward, nav.forward, false),
        );
    }
    for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        paint_icon_button(
            scene,
            transform,
            tr,
            theme,
            (*rect, icon),
            face(Slot::Action(i), action.enabled, action.on),
        );
    }
    let editing = interaction.is_some_and(|i| i.editing);
    if let Some(location) = toolbar.location.as_ref()
        && slots.location.area() > 0.0
        && !editing
    {
        paint_location(scene, transform, tr, theme, location, slots.location, {
            interaction.is_some_and(|i| i.hover == Some(Slot::Location))
        });
    }
}

/// The location at rest: its icon, then its label, elided at the end. An
/// editable one stands in a field's frame, so it reads as one to click.
fn paint_location(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    location: &crate::window_toolbar::ToolbarLocation,
    rect: Rect,
    hovered: bool,
) {
    let mut text_rect = rect;
    if location.editable {
        let face = FieldFace::new(rect, "").hovered(hovered);
        face.paint_chrome(scene, transform, theme);
        text_rect = face.text_rect(theme);
    }
    if let Some(icon) = location.icon.as_deref().and_then(Icon::from_name) {
        let c = Point::new(text_rect.x0 + LOCATION_ICON_PX / 2.0, rect.center().y);
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(c, (LOCATION_ICON_PX, LOCATION_ICON_PX)),
            icon,
            theme.tokens.colors.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_rect.x0 += LOCATION_ICON_PX + f64::from(theme.tokens.spacing.s2);
    }
    let (shown, ink) = if location.label.is_empty() {
        (
            location.placeholder.as_str(),
            theme.tokens.colors.text_tertiary,
        )
    } else {
        (location.label.as_str(), theme.tokens.colors.text_primary)
    };
    if shown.is_empty() || text_rect.width() <= 0.0 {
        return;
    }
    let style = theme.text(TextRole::Body);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a location's width is a bounded on-screen size, far inside f32"
    )]
    let width = text_rect.width() as f32;
    let line = tr.elide_to_width_style(shown, width, &style);
    let (_, h) = tr.measure_style(&line, None, &style);
    let y = rect.center().y - f64::from(h) / 2.0;
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine transform composition over f64 screen coordinates"
    )]
    let origin = transform * Affine::translate((text_rect.x0, y));
    tr.draw_style(scene, &line, ink, origin, None, &style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNavigation};

    fn browser() -> WindowToolbar {
        WindowToolbar {
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: false,
            }),
            location: Some(ToolbarLocation {
                label: "duckduckgo.com".into(),
                value: "https://duckduckgo.com/".into(),
                editable: true,
                name: "Address and search".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction {
                id: "reload".into(),
                icon: "refresh".into(),
                name: "Reload".into(),
                enabled: true,
                on: false,
            }],
        }
    }

    fn row() -> ToolbarRow {
        let mut row = ToolbarRow::new();
        row.show(
            7,
            Some(&browser()),
            Theme::dark().motion(),
            crate::time::now(),
        );
        row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
        row
    }

    #[test]
    fn navigation_leads_actions_trail_and_the_location_fills_between() {
        let row = row();
        let slots = row.slots();
        assert!(slots.back.x1 < slots.forward.x0);
        assert!(slots.forward.x1 < slots.location.x0);
        assert!(slots.location.x1 < slots.actions[0].x0);
        assert!((slots.actions[0].x1 - 640.0).abs() < f64::EPSILON);
        assert!(slots.location.width() > 400.0);
    }

    #[test]
    fn back_runs_only_where_it_can_and_forward_not_at_the_end() {
        let mut row = row();
        let back = row.slots().back.center();
        assert!(row.press(back));
        assert!(row.release(back));
        let forward = row.slots().forward.center();
        assert!(row.press(forward));
        assert!(row.release(forward));
        let reload = row.slots().actions[0].center();
        assert!(row.press(reload));
        assert!(row.release(reload));
        assert_eq!(
            row.take_events(),
            vec![
                (7, ToolbarEvent::Back),
                (
                    7,
                    ToolbarEvent::Action {
                        id: "reload".into()
                    }
                )
            ],
            "forward cannot go"
        );
    }

    #[test]
    fn a_click_on_the_location_edits_the_whole_address_and_esc_puts_it_back() {
        let mut row = row();
        let location = row.slots().location.center();
        assert!(row.press(location));
        assert!(row.is_editing());
        assert_eq!(row.field_text(), Some("https://duckduckgo.com/"));
        assert!(row.cancel_edit());
        assert!(!row.is_editing());
        assert!(row.take_events().is_empty(), "nothing was committed");
    }

    #[test]
    fn another_tab_cross_fades_and_drops_what_was_typed_in_this_one() {
        let mut row = row();
        assert!(row.begin_edit());
        let now = crate::time::now();
        row.show(8, None, Theme::dark().motion(), now);
        assert!(!row.is_editing());
        assert!(row.toolbar().is_none());
        assert!(row.needs_animation_frame(now), "the old slots fade out");
    }
}
