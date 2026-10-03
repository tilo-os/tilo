//! Save/Discard/Cancel confirmation, using the shared modal chrome.
//!
//! The owner must route input here before the document and consume every input
//! event while this prompt is open. Choices close the prompt; persistence and
//! the pending document replacement remain the owner's responsibility.

use crate::i18n::t;
use crate::num;
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::modal_dialog::{self, ModalAction, ModalHit, ModalLayout, ModalSpec};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

const CHOICES: [UnsavedChoice; 3] = [
    UnsavedChoice::Cancel,
    UnsavedChoice::Discard,
    UnsavedChoice::Save,
];
const TARGETS: [&str; 4] = [
    "unsaved:cancel",
    "unsaved:discard",
    "unsaved:save",
    "unsaved:close",
];
const LABELS: [&str; 4] = [
    "unsaved-cancel",
    "unsaved-discard",
    "unsaved-save",
    "unsaved-close",
];

#[derive(Default)]
pub(crate) struct UnsavedChanges {
    open: bool,
    focus: usize,
    pointer: Option<Point>,
    layout: Option<ModalLayout>,
    scroll: f64,
    max_scroll: f64,
}

impl UnsavedChanges {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.focus = 2; // Save is safe; Discard always requires a deliberate choice.
        self.pointer = None;
        self.layout = None;
        self.scroll = 0.0;
        self.max_scroll = 0.0;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.layout = None;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn rect(&self) -> Option<Rect> {
        self.layout.as_ref().filter(|_| self.open).map(|l| l.card)
    }

    fn choose(&mut self, choice: UnsavedChoice) -> UnsavedChoice {
        self.close();
        choice
    }

    fn move_focus(&mut self, backwards: bool) {
        // Shared frame platform ordering rotates this semantic action order;
        // the cycle therefore matches both primary-first and primary-last hosts.
        self.focus = if backwards {
            self.focus.checked_sub(1).unwrap_or(2).min(2)
        } else {
            self.focus.saturating_add(1) % CHOICES.len()
        };
    }

    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
    ) -> Option<UnsavedChoice> {
        if !self.open {
            return None;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(position.x / scale, position.y / scale));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self
                    .pointer
                    .and_then(|point| self.layout.as_ref()?.hit(point))
                {
                    match hit {
                        ModalHit::Action(index) => {
                            if let Some(choice) = CHOICES.get(index).copied() {
                                return Some(self.choose(choice));
                            }
                        }
                        ModalHit::Close | ModalHit::Scrim => {
                            return Some(self.choose(UnsavedChoice::Cancel));
                        }
                        ModalHit::Card => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 32.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale,
                };
                self.scroll = (self.scroll - delta).clamp(0.0, self.max_scroll);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        return Some(self.choose(UnsavedChoice::Cancel));
                    }
                    Key::Named(NamedKey::Tab) => self.move_focus(modifiers.state().shift_key()),
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => self.move_focus(true),
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                        self.move_focus(false);
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        return Some(
                            self.choose(
                                CHOICES
                                    .get(self.focus)
                                    .copied()
                                    .unwrap_or(UnsavedChoice::Cancel),
                            ),
                        );
                    }
                    Key::Character(text) if text == " " => {
                        return Some(
                            self.choose(
                                CHOICES
                                    .get(self.focus)
                                    .copied()
                                    .unwrap_or(UnsavedChoice::Cancel),
                            ),
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        if !self.open {
            return;
        }
        let title = t("unsaved-title");
        let body = t("unsaved-body");
        let labels = LABELS.map(t);
        let mut actions = [
            ModalAction::secondary(&labels[0]),
            ModalAction::danger(&labels[1]),
            ModalAction::primary(&labels[2]),
        ];
        let style = theme.text(TextRole::Body);
        let spec = ModalSpec::new(&title).actions(&actions);
        let preliminary = modal_dialog::layout(viewport, &spec, tr, theme);
        let width = num::f32_of_f64(preliminary.body.width().max(1.0));
        let (_, measured_height) = tr.measure_style(&body, Some(width), &style);
        let body_height = f64::from(measured_height);
        let spec = spec.body_height(body_height);
        let mut layout = modal_dialog::layout(viewport, &spec, tr, theme);
        // The shared frame leaves multi-action wrapping to its owner. Preserve
        // its measured button height, but stack actions when their row is wider
        // than the card. Painting and hit testing still use the same frame data.
        if layout.actions.iter().any(|r| r.x0 < layout.body.x0) {
            let gap = f64::from(theme.tokens.spacing.s2);
            let height = modal_dialog::action_height(theme);
            let extra = (height + gap) * 2.0;
            layout =
                modal_dialog::layout(viewport, &spec.body_height(body_height + extra), tr, theme);
            let bottom = layout.actions.first().map_or(layout.card.y1, |r| r.y1);
            let available = (bottom - layout.body.y0 - gap * 2.0).max(0.0);
            let height = height.min(available / 3.0);
            let top = bottom - height * 3.0 - gap * 2.0;
            layout.body.y1 = (top - f64::from(theme.tokens.spacing.s3)).max(layout.body.y0);
            // A vertical row uses authoring order, with Save last.
            for (index, rect) in layout.actions.iter_mut().enumerate() {
                let y = top + num::f64_of_usize(index) * (height + gap);
                *rect = Rect::new(layout.body.x0, y, layout.body.x1, y + height);
            }
        }
        let hovered = self.pointer.and_then(|point| layout.hit(point));
        for (index, action) in actions.iter_mut().enumerate() {
            action.focused = self.focus == index;
            action.hovered = hovered == Some(ModalHit::Action(index));
        }
        let spec = ModalSpec::new(&title)
            .actions(&actions)
            .close_focused(self.focus == 3)
            .close_hovered(hovered == Some(ModalHit::Close));
        modal_dialog::paint_backdrop(scene, transform, tr, theme, &layout, &spec);
        self.max_scroll = (f64::from(measured_height) - layout.body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        if layout.body.area() > 0.0 {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &layout.body,
            );
            tr.draw_style(
                scene,
                &body,
                modal_dialog::ink(theme).0,
                transform * Affine::translate((layout.body.x0, layout.body.y0 - self.scroll)),
                Some(width),
                &style,
            );
            scene.pop_layer();
        }
        modal_dialog::paint_actions(scene, transform, tr, theme, &layout, &spec);
        self.layout = Some(layout);
    }

    pub(crate) fn accessibility(&self) -> Option<AccessNode> {
        if !self.open {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let mut children = Vec::new();
        for (index, (target, label)) in TARGETS.iter().zip(LABELS).enumerate() {
            let rect = if index == 3 {
                layout.close
            } else {
                layout.actions.get(index).copied()
            };
            if let Some(rect) = rect {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(t(label))
                        .with_action_target(*target)
                        .with_bounds(rect)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.focus == index,
                            ..Default::default()
                        }),
                );
            }
        }
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t("unsaved-title"))
                .with_description(t("unsaved-body"))
                .with_bounds(layout.card)
                .with_state(AccessState {
                    modal: true,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    pub(crate) fn accessible_action(
        &mut self,
        target: u64,
        action: &Action,
    ) -> Option<UnsavedChoice> {
        if !self.open || !matches!(action, Action::Activate) {
            return None;
        }
        let index = TARGETS
            .iter()
            .position(|id| ui::a11y::hash_key(id) == target)?;
        Some(self.choose(CHOICES.get(index).copied().unwrap_or(UnsavedChoice::Cancel)))
    }

    pub(crate) fn focus_accessible(&mut self, target: u64) -> bool {
        if !self.open {
            return false;
        }
        if let Some(index) = TARGETS
            .iter()
            .position(|id| ui::a11y::hash_key(id) == target)
        {
            self.focus = index;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;
    use winit::event::{DeviceId, ElementState, KeyEvent, MouseButton, RawKeyEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

    fn key(named: NamedKey) -> WindowEvent {
        let key = Key::Named(named);
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event: KeyEvent::synthetic(
                RawKeyEvent {
                    physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    state: ElementState::Pressed,
                },
                key.clone(),
                None,
                KeyLocation::Standard,
                false,
                None,
                key,
            ),
            is_synthetic: true,
        }
    }

    fn press(dialog: &mut UnsavedChanges, named: NamedKey) -> Option<UnsavedChoice> {
        dialog.handle_event(&key(named), 1.0, Modifiers::default())
    }

    fn draw(dialog: &mut UnsavedChanges, theme: &Theme, viewport: Rect) {
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(theme.text_scale);
        dialog.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut renderer,
            theme,
            viewport,
        );
    }

    fn click(dialog: &mut UnsavedChanges, point: Point) -> Option<UnsavedChoice> {
        dialog.handle_event(
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(point.x * 2.0, point.y * 2.0),
            },
            2.0,
            Modifiers::default(),
        );
        dialog.handle_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            2.0,
            Modifiers::default(),
        )
    }

    /// Keyboard choices are safe and reset on reopen, and Tab and Shift-Tab reach every choice.
    #[test]
    fn the_unsaved_prompt_keyboard_reaches_every_safe_choice() {
        // Phase: unsaved_prompt_keyboard_choices_are_safe_and_reset_on_reopen.
        {
            let mut dialog = UnsavedChanges::default();
            assert!(!dialog.is_open());
            assert_eq!(press(&mut dialog, NamedKey::Enter), None);
            dialog.open();
            assert!(dialog.is_open());
            assert_eq!(
                press(&mut dialog, NamedKey::Enter),
                Some(UnsavedChoice::Save)
            );
            assert!(!dialog.is_open());
            dialog.open();
            assert_eq!(
                press(&mut dialog, NamedKey::Escape),
                Some(UnsavedChoice::Cancel)
            );
            dialog.open();
            press(&mut dialog, NamedKey::ArrowLeft);
            assert_eq!(
                press(&mut dialog, NamedKey::Space),
                Some(UnsavedChoice::Discard)
            );
            dialog.open();
            assert_eq!(
                press(&mut dialog, NamedKey::Enter),
                Some(UnsavedChoice::Save)
            );
        }

        // Phase: unsaved_prompt_tab_and_shift_tab_reach_every_choice.
        {
            let mut dialog = UnsavedChanges::default();
            dialog.open();
            press(&mut dialog, NamedKey::Tab);
            assert_eq!(
                press(&mut dialog, NamedKey::Enter),
                Some(UnsavedChoice::Cancel)
            );
            dialog.open();
            dialog.handle_event(&key(NamedKey::Tab), 1.0, ModifiersState::SHIFT.into());
            assert_eq!(
                press(&mut dialog, NamedKey::Enter),
                Some(UnsavedChoice::Discard)
            );
        }
    }

    #[test]
    fn unsaved_prompt_actions_are_visible_and_clickable_in_both_themes_and_scales() {
        for theme in [Theme::dark(), Theme::light()] {
            for scale in [1.0, 2.0] {
                for viewport in [
                    Rect::new(0.0, 0.0, 1000.0, 800.0),
                    Rect::new(0.0, 0.0, 600.0, 600.0),
                ] {
                    let theme = theme.clone().with_text_scale(scale);
                    for (index, choice) in [
                        UnsavedChoice::Cancel,
                        UnsavedChoice::Discard,
                        UnsavedChoice::Save,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let mut dialog = UnsavedChanges::default();
                        dialog.open();
                        draw(&mut dialog, &theme, viewport);
                        let layout = dialog.layout.as_ref().unwrap();
                        assert_eq!(layout.card.intersect(viewport), layout.card);
                        for action in &layout.actions {
                            assert!(action.area() > 0.0);
                            assert_eq!(action.intersect(layout.card), *action);
                        }
                        let point = layout.actions[index].center();
                        assert_eq!(click(&mut dialog, point), Some(choice));
                    }
                }
            }
        }
    }

    #[test]
    fn unsaved_prompt_scrim_and_close_cancel_and_accessible_buttons_activate() {
        let mut dialog = UnsavedChanges::default();
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        dialog.open();
        draw(&mut dialog, &Theme::dark(), viewport);
        assert_eq!(
            click(&mut dialog, Point::new(1.0, 1.0)),
            Some(UnsavedChoice::Cancel)
        );
        dialog.open();
        draw(&mut dialog, &Theme::light(), viewport);
        let close = dialog.layout.as_ref().unwrap().close.unwrap().center();
        assert_eq!(click(&mut dialog, close), Some(UnsavedChoice::Cancel));
        for (target, choice) in [
            ("unsaved:save", UnsavedChoice::Save),
            ("unsaved:discard", UnsavedChoice::Discard),
            ("unsaved:cancel", UnsavedChoice::Cancel),
        ] {
            dialog.open();
            draw(&mut dialog, &Theme::dark(), viewport);
            let tree = dialog.accessibility().unwrap();
            assert!(tree.state.modal);
            assert_eq!(tree.children.len(), 4);
            assert!(dialog.focus_accessible(ui::a11y::hash_key(&target)));
            assert_eq!(
                dialog.accessible_action(ui::a11y::hash_key(&target), &Action::Activate),
                Some(choice)
            );
        }
        assert!(dialog.accessibility().is_none());
    }
}
