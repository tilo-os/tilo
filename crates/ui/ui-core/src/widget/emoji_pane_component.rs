//! [`EmojiPicker`] as a pane: the whole picker a view places, not the button that opens one.
//!
//! The shell opens the picker over the windows with a chord, at the caret of the field the person
//! types into; a view that places this component draws the same card, with the same search,
//! groups, tones and recents every app's picker has, and hears each pick or its dismissal.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, Value};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{DrawChild, EventCtx, TaffyTreeExt, Widget};

use super::emoji_picker::{EmojiPicker, PickerEvent};

/// The emoji picker as a card a view places: search, groups, tones and recents, writing each pick
/// into the field the person was typing in. It is modal while it is open.
#[derive(Component, Default)]
#[component(name = "EmojiPane", role = "Dialog", label = "label")]
pub struct EmojiPaneProps {
    /// Whether it is open; a closed pane paints nothing and takes no input.
    pub open: bool,
    /// What assistive technology calls it; the picker's own card name unless said.
    pub label: Option<String>,
    /// An emoji was picked: carries it.
    #[component(event(value = String))]
    pub pick: Event,
    /// It was closed without a pick (Escape, or a press outside it).
    #[component(event)]
    pub dismiss: Event,
}

/// The picker card a view places.
pub struct EmojiPane {
    picker: EmojiPicker,
    open: bool,
    rect: Rect,
    pick: Event,
    dismiss: Event,
}

impl EmojiPane {
    /// The picker card, open as `open` says, reporting picks and its closing to `pick` and
    /// `dismiss`.
    #[must_use]
    pub fn new(open: bool, pick: Event, dismiss: Event) -> Self {
        let mut picker = EmojiPicker::sheet();
        if open {
            picker.reopen();
        } else {
            picker.close();
        }
        Self {
            picker,
            open,
            rect: Rect::ZERO,
            pick,
            dismiss,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open && self.picker.is_open()
    }

    /// Report what the person did to the card: a pick, or its closing.
    fn take_events(&mut self) {
        for happened in self.picker.take_events() {
            match happened {
                PickerEvent::Picked(text) => self.pick.fire(Some(Value::Text(text))),
                PickerEvent::Dismissed => self.dismiss.fire(None),
            }
        }
    }
}

impl Described for EmojiPane {
    type Props = EmojiPaneProps;

    fn from_props(_key: &str, props: EmojiPaneProps) -> Self {
        Self::new(props.open, props.pick, props.dismiss)
    }

    /// Opening or closing keeps what was typed and chosen where the card stays open; the events a
    /// pick or a closing reports change where the pane stands.
    fn set_props(&mut self, props: EmojiPaneProps) -> bool {
        if props.open != self.open {
            if props.open {
                self.picker.reopen();
            } else {
                self.picker.close();
            }
            self.open = props.open;
        }
        self.pick = props.pick;
        self.dismiss = props.dismiss;
        true
    }
}

impl Widget for EmojiPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The card sizes itself; the slot it stands in is the room it may take, so the card is
        // centred inside it as the chrome places the same picker. The slot fills the parent, so
        // the percentage resolves against the display the shell stands on.
        let style = Style {
            size: taffy::Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::style_helpers::percent(1.0_f32),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        if !self.open {
            return;
        }
        // The slot is the card's room: place the card inside it (centred when it has no caret)
        // before painting it.
        self.picker.place_at(None, self.rect, theme);
        self.picker.draw_child(scene, transform, text, theme, &env);
        self.take_events();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if !self.open {
            return;
        }
        self.picker.handle_event(event, ctx);
        self.take_events();
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.is_open().then_some(self.picker.rect())
    }

    fn dismiss_presented_modal(&mut self) -> bool {
        if !self.is_open() {
            return false;
        }
        self.picker.close();
        self.dismiss.fire(None);
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        if self.is_open() {
            self.picker.accessibility()
        } else {
            None
        }
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.picker.ime_cursor_rect()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.picker.cursor_shape(point)
    }

    fn needs_animation_frame(&self) -> bool {
        self.picker.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        (self.open && self.picker.needs_redraw()) || self.picker.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.picker.mark_drawn();
    }

    fn name(&self) -> &str {
        "EmojiPane"
    }

    crate::described!();
}
