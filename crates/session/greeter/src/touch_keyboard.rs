//! A keyboard on the screen for signing in by touch.
//!
//! The session's on-screen keyboard is an input method served by the Tilo
//! compositor. The greeter runs before any session, in a kiosk compositor
//! that offers no input method, so a touch-only device would have no way to
//! enter a passcode. This keyboard is part of the greeter instead: it shows
//! while a field of the form has the focus on a touch device, and the
//! greeter hands what it types to that field's own editing (see
//! `GreeterApp::type_into_focused_field`). It never predicts, corrects or
//! remembers anything: what it types is a passcode.
//!
//! The widget is rebuilt with the form; what must outlive a rebuild (shift,
//! the symbols layer, presses not yet delivered) lives in the shared
//! [`KeyboardModel`].

use std::cell::RefCell;
use std::rc::Rc;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// What one key press asks of the focused field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Text(String),
    Backspace,
    Enter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    Shift,
    Symbols,
    Space,
    Backspace,
    Enter,
}

const LETTERS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 4] = ["1234567890", "!@#$%^&*()", "-_=+[]{};:", "'\",.<>/?\\"];
const ROWS: u16 = 5;

/// State that outlives the form's rebuilds.
#[derive(Debug, Default)]
pub struct KeyboardModel {
    shift: bool,
    symbols: bool,
    actions: Vec<KeyAction>,
}

impl KeyboardModel {
    /// What was typed since the last call, in order.
    pub fn take_actions(&mut self) -> Vec<KeyAction> {
        std::mem::take(&mut self.actions)
    }

    fn rows(&self) -> Vec<Vec<(Key, f64)>> {
        let source = if self.symbols { SYMBOLS } else { LETTERS };
        let mut rows: Vec<Vec<(Key, f64)>> = source
            .iter()
            .map(|row| row.chars().map(|c| (Key::Char(c), 1.0)).collect())
            .collect();
        if let Some(last) = rows.last_mut() {
            last.insert(0, (Key::Shift, 1.5));
            last.push((Key::Backspace, 1.5));
        }
        rows.push(vec![
            (Key::Symbols, 1.5),
            (Key::Space, 6.0),
            (Key::Enter, 2.0),
        ]);
        rows
    }

    fn press(&mut self, key: Key) {
        match key {
            Key::Char(c) => {
                let c = if self.shift {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                };
                self.actions.push(KeyAction::Text(c.to_string()));
                self.shift = false;
            }
            Key::Space => self.actions.push(KeyAction::Text(" ".into())),
            Key::Backspace => self.actions.push(KeyAction::Backspace),
            Key::Enter => self.actions.push(KeyAction::Enter),
            Key::Shift => self.shift = !self.shift,
            Key::Symbols => {
                self.symbols = !self.symbols;
                self.shift = false;
            }
        }
    }

    fn label(&self, key: Key) -> String {
        match key {
            Key::Char(c) if self.shift => c.to_uppercase().collect(),
            Key::Char(c) => c.to_string(),
            Key::Shift => "⇧".into(),
            Key::Symbols if self.symbols => "abc".into(),
            Key::Symbols => "?123".into(),
            Key::Space => String::new(),
            Key::Backspace => "⌫".into(),
            Key::Enter => "↵".into(),
        }
    }
}

pub struct TouchKeyboard {
    model: Rc<RefCell<KeyboardModel>>,
    key_height: f32,
    gap: f32,
    max_width: f32,
    rect: Rect,
    cursor: Point,
    pressed: Option<Key>,
    /// Keys whose look changed since the last paint.
    damage: Vec<Rect>,
}

impl TouchKeyboard {
    /// A keyboard of `key_height` keys `gap` apart, at most `max_width`
    /// wide, typing into `model`.
    pub fn new(
        model: Rc<RefCell<KeyboardModel>>,
        key_height: f32,
        gap: f32,
        max_width: f32,
    ) -> Self {
        Self {
            model,
            key_height,
            gap,
            max_width,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            pressed: None,
            damage: Vec::new(),
        }
    }

    fn height(&self) -> f32 {
        let rows = f32::from(ROWS);
        self.key_height * rows + self.gap * (rows - 1.0)
    }

    /// Every key and where it sits.
    pub(crate) fn layout(&self) -> Vec<(Key, Rect)> {
        let gap = f64::from(self.gap);
        let key_height = f64::from(self.key_height);
        let mut out = Vec::new();
        let mut y = self.rect.y0;
        for row in self.model.borrow().rows() {
            let units: f64 = row.iter().map(|(_, width)| width).sum();
            let gaps = gap * f64::from(u32::try_from(row.len()).unwrap_or(1).saturating_sub(1));
            let unit = ((self.rect.width() - gaps) / units).min(key_height * 1.4);
            let row_width = unit * units + gaps;
            let mut x = self.rect.x0 + (self.rect.width() - row_width) / 2.0;
            for (key, width) in row {
                let w = unit * width;
                out.push((key, Rect::new(x, y, x + w, y + key_height)));
                x += w + gap;
            }
            y += key_height + gap;
        }
        out
    }

    fn key_at(&self, point: Point) -> Option<Key> {
        self.layout()
            .into_iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(key, _)| key)
    }

    fn key_rect(&self, wanted: Key) -> Option<Rect> {
        self.layout()
            .into_iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, rect)| rect)
    }

    /// Where `key` sits, if this layer has it.
    #[cfg(test)]
    pub(crate) fn key_center(&self, key: Key) -> Option<Point> {
        self.key_rect(key).map(|rect| rect.center())
    }
}

impl Widget for TouchKeyboard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "a leaf node with a plain style cannot fail to be created"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: length(self.height()),
                },
                max_size: Size {
                    width: length(self.max_width),
                    height: length(self.height()),
                },
                margin: taffy::Rect {
                    left: auto(),
                    right: auto(),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = taffy.layout(node) {
            let x0 = origin.x + f64::from(layout.location.x);
            let y0 = origin.y + f64::from(layout.location.y);
            self.rect = Rect::new(
                x0,
                y0,
                x0 + f64::from(layout.size.width),
                y0 + f64::from(layout.size.height),
            );
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let ground = ui::chrome::panel_ground(theme);
        let pressed_ground = c.border_focus;
        let hairline = ui::tokens::colors::Colors::hairline_on(ground, 40);
        let radius = f64::from(theme.tokens.radii.md);
        let model = self.model.borrow();
        for (key, rect) in self.layout() {
            let lit = matches!(key, Key::Shift) && model.shift
                || matches!(key, Key::Symbols) && model.symbols
                || self.pressed == Some(key);
            let shape = RoundedRect::from_rect(rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                if lit { pressed_ground } else { ground },
                None,
                &shape,
            );
            scene.stroke(&Stroke::new(1.0), transform, hairline, None, &shape);
            let label = model.label(key);
            if label.is_empty() {
                continue;
            }
            let size = 18.0_f32;
            let (w, h) = text_renderer.measure(&label, size, None);
            text_renderer.draw_text(
                scene,
                &label,
                size,
                c.text_primary,
                transform.pre_translate(Vec2::new(
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                )),
                None,
            );
        }
        drop(model);
        self.damage.clear();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                self.pressed = self.key_at(self.cursor);
                if let Some(rect) = self.pressed.and_then(|key| self.key_rect(key)) {
                    self.damage.push(rect);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.pressed.is_some() => {
                if let Some(key) = self.pressed.take() {
                    let before = self.layout();
                    let shifted = self.model.borrow().shift;
                    if self.key_at(self.cursor) == Some(key) {
                        self.model.borrow_mut().press(key);
                    }
                    // A layer or shift change relabels every key; a plain
                    // key only itself.
                    if matches!(key, Key::Shift | Key::Symbols)
                        || matches!(key, Key::Char(_)) && shifted
                    {
                        self.damage.extend(before.into_iter().map(|(_, rect)| rect));
                    } else if let Some(rect) = self.key_rect(key) {
                        self.damage.push(rect);
                    }
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        !self.damage.is_empty()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        regions.extend(self.damage.iter().map(|rect| rect.inflate(1.0, 1.0)));
    }

    fn name(&self) -> &'static str {
        "TouchKeyboard"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard() -> TouchKeyboard {
        let mut keyboard = TouchKeyboard::new(Rc::default(), 40.0, 8.0, 720.0);
        keyboard.rect = Rect::new(0.0, 400.0, 720.0, 400.0 + f64::from(keyboard.height()));
        keyboard
    }

    fn tap(keyboard: &TouchKeyboard, wanted: Key) {
        let at = keyboard
            .key_center(wanted)
            .expect("the key is on this layer");
        assert_eq!(keyboard.key_at(at), Some(wanted));
        keyboard.model.borrow_mut().press(wanted);
    }

    #[test]
    fn keys_type_text_shifted_once_and_symbols_on_their_own_layer() {
        let keyboard = keyboard();
        tap(&keyboard, Key::Char('p'));
        tap(&keyboard, Key::Shift);
        tap(&keyboard, Key::Char('a'));
        tap(&keyboard, Key::Char('s'));
        tap(&keyboard, Key::Symbols);
        tap(&keyboard, Key::Char('#'));
        tap(&keyboard, Key::Char('7'));
        tap(&keyboard, Key::Backspace);
        tap(&keyboard, Key::Enter);
        assert_eq!(
            keyboard.model.borrow_mut().take_actions(),
            [
                KeyAction::Text("p".into()),
                KeyAction::Text("A".into()),
                KeyAction::Text("s".into()),
                KeyAction::Text("#".into()),
                KeyAction::Text("7".into()),
                KeyAction::Backspace,
                KeyAction::Enter,
            ]
        );
    }

    #[test]
    fn every_key_is_a_touch_target_inside_the_keyboard_and_none_overlap() {
        let keyboard = keyboard();
        let keys = keyboard.layout();
        assert!(keys.len() > 30);
        for (i, (_, a)) in keys.iter().enumerate() {
            assert!(keyboard.rect.contains(a.center()));
            assert!(a.width() >= 30.0 && a.height() >= 30.0, "{a:?}");
            for (_, b) in keys.iter().skip(i + 1) {
                assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }
}
