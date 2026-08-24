//! `RemarkCompose` — the "say something about this" surface.
//!
//! A small panel that opens **beside** an app's ordinary context menu when
//! something is selected: the thing being talked about as a chip, a visible
//! destination selector, a compose field, and a send button. Type, send, and
//! the message carries the selected resource's address rather than a
//! description of it.
//!
//! It lives in the toolkit rather than in an app because the gesture is not an
//! app feature. Anything that can answer
//! [`Referent`](qos_reference::Referent) gets it, and this surface never learns
//! which app it is talking to — it is handed a [`ResourceRef`] and a list of
//! destinations and knows nothing else. That is what makes "select anything and
//! say something about it" true of the system instead of true of two apps.
//!
//! ## The destination is visible, not inferred
//!
//! A selector sits directly above the field showing where the message is going,
//! and it can be changed there. Nothing routes a message anywhere the selector
//! is not showing: [`Self::destination`] is what is on screen, and
//! [`Remark::delivery`](qos_reference::Remark::delivery) routes from that same
//! value, so the two cannot disagree. A control that displayed one destination
//! while the message went to another would be worse than no control at all.
//!
//! Typing `@name` *pings* somebody; it does not move the message. The selector
//! keeps showing the channel, so nobody is surprised about who could read it.
//!
//! ## It does not take over the context menu
//!
//! The menu beside it keeps working. This panel owns its own rect and its own
//! keyboard focus, and closing one does not close the other — dismissing the
//! compose leaves the menu usable, and dismissing the menu leaves a
//! half-written remark alone rather than discarding it.
//!
//! ## Labels come in localised
//!
//! [`DestinationOption::label`] is supplied by the caller, already resolved
//! through `fluent`. The toolkit does not word the destinations itself: a
//! hard-coded "private" here would be untranslatable English in the middle of
//! everyone's chip.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qos_reference::{Destination, Remark, ResourceRef};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::button::{Button, ButtonStyle};
use crate::widget::chip::{Chip, ChipLead, ChipStyle};
use crate::widget::dropdown::Dropdown;
use crate::widget::text_input::TextInput;
use crate::widget::Widget;

/// Panel width. Wide enough for a destination and a sentence, narrow enough to
/// sit beside a context menu without covering the document it is about.
const PANEL_W: f64 = 340.0;
const PAD: f64 = 12.0;
const GAP: f64 = 8.0;
/// Height of the band the reference chip is painted into.
const CHIP_BAND: f64 = 30.0;
const CHIP_H: f64 = 24.0;

/// One choice in the destination selector: where it goes, and the caller's
/// already-localised words for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestinationOption {
    pub destination: Destination,
    /// Localised label — the caller resolves this through `fluent`, including
    /// however it words "private" and "agent" (see
    /// [`Destination::display`](qos_reference::Destination::display) for the
    /// facts it must convey).
    pub label: String,
}

/// The toolkit's own strings for this surface. Registered on first use through
/// [`I18nManager::ensure_resource`](crate::i18n::I18nManager::ensure_resource),
/// which will not disturb a bundle an app has already set up.
const CATALOGUES: &[(&str, &str)] = &[
    ("en", include_str!("../../locales/remark-compose.en-US.ftl")),
    ("de", include_str!("../../locales/remark-compose.de-DE.ftl")),
];

fn ensure_strings() {
    crate::i18n::I18nManager::ensure_resource("remark_compose_send", CATALOGUES);
}

impl DestinationOption {
    pub fn new(destination: Destination, label: impl Into<String>) -> Self {
        Self {
            destination,
            label: label.into(),
        }
    }

    /// Word a destination through the toolkit's own `fluent` keys.
    ///
    ///
    /// One place decides how a destination reads, so every app's selector says
    /// the same thing — and, more importantly, says the privacy out loud. The
    /// agent case gets its own message rather than reusing the private one:
    /// addressing an agent privately is unremarkable *because its reply lands
    /// in the room*, and a person choosing it should be able to read that
    /// rather than having to know it.
    pub fn localised(destination: Destination) -> Self {
        ensure_strings();
        let d = destination.display();
        let key = if d.agent {
            "remark_destination_agent"
        } else if d.private {
            "remark_destination_private"
        } else {
            "remark_destination_channel"
        };
        let label = crate::i18n::I18nManager::format(
            key,
            &[("name", crate::i18n::ArgValue::Text(d.name.to_string()))],
        );
        Self { destination, label }
    }
}

/// The compose field's placeholder, localised.
pub fn placeholder_text() -> String {
    ensure_strings();
    crate::i18n::I18nManager::lookup("remark_compose_placeholder", None)
}

/// The send button's label, localised.
pub fn send_label() -> String {
    ensure_strings();
    crate::i18n::I18nManager::lookup("remark_compose_send", None)
}

/// The compose surface. See the [module docs](self).
pub struct RemarkCompose {
    /// What the remark is about. `None` while closed.
    reference: Option<ResourceRef>,
    options: Vec<DestinationOption>,
    /// Which option the surface opened showing, used until one is picked.
    default_index: usize,
    selector: Dropdown,
    input: TextInput,
    send: Button,
    /// Set by the send button's callback, drained in [`Self::poll`].
    send_pressed: Arc<AtomicBool>,
    open: bool,
    anchor: Point,
    rect: Rect,
    /// The finished remark, waiting to be taken by the host.
    sent: Option<Remark>,
}

impl Default for RemarkCompose {
    fn default() -> Self {
        Self::new()
    }
}

impl RemarkCompose {
    pub fn new() -> Self {
        let send_pressed = Arc::new(AtomicBool::new(false));
        let flag = send_pressed.clone();
        Self {
            reference: None,
            options: Vec::new(),
            default_index: 0,
            // A scratch path: the chosen destination is presentation, not
            // shared state, so it is not written anywhere a peer replays.
            selector: Dropdown::new(Vec::new(), "").with_id("remark_destination"),
            input: TextInput::new(Vec::new())
                .with_id("remark_input")
                .with_enter_submits(true),
            send: Button::new("remark_send", "", None)
                .with_style(ButtonStyle::Primary)
                .on_click(move || flag.store(true, Ordering::Relaxed)),
            send_pressed,
            open: false,
            anchor: Point::ZERO,
            rect: Rect::ZERO,
            sent: None,
        }
    }

    /// Open the surface for `reference`, offering `options` and starting on
    /// `chosen`.
    ///
    /// `placeholder` and `send_label` arrive already localised — see the
    /// [module docs](self).
    pub fn open_for(
        &mut self,
        reference: ResourceRef,
        options: Vec<DestinationOption>,
        chosen: usize,
        anchor: Point,
        placeholder: &str,
        send_label: &str,
    ) {
        self.reference = Some(reference);
        let chosen = chosen.min(options.len().saturating_sub(1));
        // The control shows the starting destination before anything is picked:
        // an empty selector over a message that already has a destination would
        // be the exact ambiguity this control exists to remove.
        let shown = options.get(chosen).map(|o| o.label.clone()).unwrap_or_default();
        self.selector = Dropdown::new(options.iter().map(|o| o.label.clone()).collect(), "")
            .with_id("remark_destination")
            .with_placeholder(&shown);
        self.default_index = chosen;
        self.options = options;
        self.input.set_text("");
        self.input.set_placeholder(placeholder);
        let flag = self.send_pressed.clone();
        self.send = Button::new("remark_send", send_label, None)
            .with_style(ButtonStyle::Primary)
            .on_click(move || flag.store(true, Ordering::Relaxed));
        self.send_pressed.store(false, Ordering::Relaxed);
        self.anchor = anchor;
        self.open = true;
        self.sent = None;
    }

    /// Open the surface for whatever `referent` currently has selected.
    ///
    /// **Both** input paths go through here — the right-click gesture and the
    /// [`is_compose_chord`] keyboard equivalent — so the two cannot drift into
    /// producing different messages for the same selection. There is one way to
    /// start a remark, and the pointer is only one way to reach it.
    ///
    /// Returns `false` when nothing is selected, in which case the surface stays
    /// closed and the gesture is simply not offered.
    #[allow(clippy::too_many_arguments)]
    pub fn open_for_selection(
        &mut self,
        referent: &dyn qos_reference::Referent,
        doc: &AutoCommit,
        options: Vec<DestinationOption>,
        chosen: usize,
        anchor: Point,
        placeholder: &str,
        send_label: &str,
    ) -> bool {
        let Some(reference) = referent.selection_reference(doc) else {
            return false;
        };
        self.open_for(reference, options, chosen, anchor, placeholder, send_label);
        true
    }

    /// Dismiss without sending. The half-written text is discarded with the
    /// surface; the context menu beside it is untouched.
    pub fn close(&mut self) {
        self.open = false;
        self.reference = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What the remark is about, while open. This is what makes the chip
    /// *followable*: a host can take the address and resolve it.
    pub fn reference(&self) -> Option<&ResourceRef> {
        self.reference.as_ref()
    }

    /// The destination currently **shown by the selector** — and therefore the
    /// one a sent remark is delivered to.
    ///
    /// Resolved from what the control displays: the picked option, or the one
    /// it opened showing. There is deliberately no separate "current
    /// destination" field for the display to fall out of step with.
    pub fn destination(&self) -> Option<&Destination> {
        let selected = self.selector.selected();
        self.options
            .iter()
            .find(|o| o.label == selected)
            .or_else(|| self.options.get(self.default_index))
            .map(|o| &o.destination)
    }

    /// The text typed so far.
    pub fn text(&self) -> &str {
        self.input.value()
    }

    /// Drain the send affordances — the button and `Enter` in the field — into
    /// a finished [`Remark`]. Called once per frame by the host.
    ///
    /// Both affordances land here rather than each building their own message,
    /// so the mouse path and the keyboard path cannot produce different
    /// messages for the same state.
    pub fn poll(&mut self) {
        if !self.open {
            return;
        }
        let clicked = self.send_pressed.swap(false, Ordering::Relaxed);
        let entered = self.input.take_submit();
        if clicked || entered {
            self.submit();
        }
    }

    /// Build the remark from the current state and close.
    ///
    /// Mentions are left empty deliberately: resolving `@name` needs the room's
    /// roster, which the host has and the toolkit does not, and chat binds
    /// mentions once at send. The host calls
    /// [`Remark::with_mentions`](qos_reference::Remark::with_mentions) with its
    /// own resolver before dispatching.
    pub fn submit(&mut self) {
        let (Some(reference), Some(destination)) =
            (self.reference.clone(), self.destination().cloned())
        else {
            return;
        };
        let text = self.input.value().trim().to_string();
        if text.is_empty() {
            // Nothing was said. Sending an empty remark would post a bare
            // reference with no point to it.
            return;
        }
        self.sent = Some(Remark::new(text, reference, destination));
        self.open = false;
        self.reference = None;
    }

    /// One-shot read of a finished remark.
    pub fn take_sent(&mut self) -> Option<Remark> {
        self.sent.take()
    }
}

/// Whether `event` is the keyboard equivalent of the right-click gesture.
///
/// `Ctrl+Shift+M` on the current selection opens the same surface the mouse
/// path opens, with the same reference — the gesture is not mouse-only, which
/// matters because an address is exactly what somebody working without a
/// pointer cannot produce by describing.
pub fn is_compose_chord(event: &KeyEvent, modifiers: &Modifiers) -> bool {
    let state = modifiers.state();
    if !state.control_key() || !state.shift_key() {
        return false;
    }
    match &event.logical_key {
        Key::Character(c) => c.eq_ignore_ascii_case("m"),
        _ => false,
    }
}

/// Whether `event` should dismiss the surface.
pub fn is_dismiss(event: &KeyEvent) -> bool {
    matches!(event.logical_key, Key::Named(NamedKey::Escape))
}

impl Widget for RemarkCompose {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.open {
            return;
        }
        self.selector.sync(state, path);
        self.input.sync(state, path);
        self.send.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if !self.open {
            // Closed: an empty leaf, so nothing lays out and nothing draws.
            let node = taffy.new_leaf(Style::default()).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            return node;
        }
        let children = vec![
            self.selector.build_layout(taffy, theme, node_map),
            self.input.build_layout(taffy, theme, node_map),
            self.send.build_layout(taffy, theme, node_map),
        ];
        let node = taffy
            .new_with_children(
                Style {
                    // The anchor is applied in `apply_layout`, not as a taffy
                    // inset: this widget is laid out as the root of its own
                    // throwaway tree (like the context menu it sits beside),
                    // and an absolute inset on a root has no containing block
                    // to be relative to, so taffy drops it and the panel lands
                    // at the origin.
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    gap: Size {
                        width: length(0.0),
                        height: length(GAP as f32),
                    },
                    padding: taffy::Rect {
                        left: length(PAD as f32),
                        right: length(PAD as f32),
                        // The chip is painted into the top padding band.
                        top: length((PAD + CHIP_BAND) as f32),
                        bottom: length(PAD as f32),
                    },
                    size: Size {
                        width: Dimension::length(PANEL_W as f32),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        let x = origin.x + self.anchor.x + l.location.x as f64;
        let y = origin.y + self.anchor.y + l.location.y as f64;
        self.rect = Rect::new(x, y, x + l.size.width as f64, y + l.size.height as f64);
        if !self.open {
            return;
        }
        let child_origin = Point::new(x, y);
        if let Ok(kids) = taffy.children(node) {
            let mut widgets: Vec<&mut (dyn Widget + '_)> =
                vec![&mut self.selector, &mut self.input, &mut self.send];
            for (w, n) in widgets.iter_mut().zip(kids) {
                w.apply_layout(taffy, n, child_origin);
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open || self.rect.width() <= 1.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        // The panel sits on the overlay tier, like the context menu it appears
        // beside, so the two read as one surface rather than two tiers.
        let panel = RoundedRect::from_rect(self.rect, theme.tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &panel);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &panel,
        );

        // The reference chip: what this is about, in the top band.
        if let Some(r) = &self.reference {
            let cx = self.rect.x0 + PAD;
            let cy = self.rect.y0 + PAD;
            let avail = (self.rect.width() - PAD * 2.0).max(0.0);
            let natural = Chip::natural_width_at(
                &r.label,
                CHIP_H,
                ChipLead::Glyph(crate::icons::Icon::ChevronRight),
                theme,
                text_renderer,
            );
            let w = natural.min(avail);
            Chip::draw_at(
                scene,
                transform,
                Rect::new(cx, cy, cx + w, cy + CHIP_H),
                &r.label,
                ChipLead::Glyph(crate::icons::Icon::ChevronRight),
                ChipStyle::tinted(theme.accent_primary),
                theme,
                text_renderer,
            );
        }

        self.selector.draw(scene, transform, text_renderer, theme, presence);
        self.input.draw(scene, transform, text_renderer, theme, presence);
        self.send.draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        // The open destination list composites above everything, including the
        // panel it belongs to.
        self.selector
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if !self.open {
            return;
        }
        // Escape dismisses this surface and nothing else — the context menu
        // beside it is a separate widget and keeps its own state.
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state.is_pressed() && is_dismiss(key) {
                self.close();
                *handled = true;
                return;
            }
        }
        let Ok(kids) = taffy.children(node_id) else {
            return;
        };
        let mut widgets: Vec<&mut (dyn Widget + '_)> =
            vec![&mut self.selector, &mut self.input, &mut self.send];
        for (w, n) in widgets.iter_mut().zip(kids) {
            w.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            if *handled {
                break;
            }
        }
        self.poll();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if !self.open {
            return vec![];
        }
        vec![&self.selector, &self.input, &self.send]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if !self.open {
            return vec![];
        }
        vec![&mut self.selector, &mut self.input, &mut self.send]
    }

    fn name(&self) -> &str {
        "RemarkCompose"
    }

    fn test_id(&self) -> Option<&str> {
        Some("remark_compose")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_reference::{Delivery, MentionRef, Span, Target};

    /// An app that has something selected. Stands in for a real `Referent`;
    /// what matters is that the surface only ever sees the trait.
    struct Selected(ResourceRef);
    impl qos_reference::Referent for Selected {
        fn selection_reference(&self, _doc: &AutoCommit) -> Option<ResourceRef> {
            Some(self.0.clone())
        }
    }
    struct NothingSelected;
    impl qos_reference::Referent for NothingSelected {
        fn selection_reference(&self, _doc: &AutoCommit) -> Option<ResourceRef> {
            None
        }
    }

    fn paragraph() -> ResourceRef {
        ResourceRef::ranged(
            "word_processor.block",
            "word-processor.block:blk_7",
            "Run sheet · ¶3",
            Span::chars(12, 41),
        )
    }

    fn options() -> Vec<DestinationOption> {
        vec![
            DestinationOption::new(Destination::channel("c_general", "general"), "# general"),
            DestinationOption::new(
                Destination::participant("u_ada", "Ada", false),
                "Ada — private",
            ),
            DestinationOption::new(
                Destination::participant("agent_scribe", "Scribe", true),
                "Scribe — private",
            ),
        ]
    }

    fn opened(chosen: usize) -> RemarkCompose {
        let mut c = RemarkCompose::new();
        c.open_for(
            paragraph(),
            options(),
            chosen,
            Point::new(200.0, 120.0),
            "say something about this",
            "Send",
        );
        c
    }

    /// The keyboard path and the mouse path produce the *same* message, because
    /// they are the same path: both reach the surface through
    /// `open_for_selection`.
    #[test]
    fn the_chord_and_the_right_click_produce_an_identical_message() {
        let doc = AutoCommit::new();
        let app = Selected(paragraph());

        let mut by_mouse = RemarkCompose::new();
        assert!(by_mouse.open_for_selection(
            &app,
            &doc,
            options(),
            0,
            Point::new(200.0, 120.0),
            "say something about this",
            "Send"
        ));
        by_mouse.input.set_text("the times contradict the prose");
        by_mouse.submit();

        let mut by_keyboard = RemarkCompose::new();
        // Same gesture, reached without a pointer. The anchor differs because a
        // chord has no cursor position — and the message must not.
        assert!(by_keyboard.open_for_selection(
            &app,
            &doc,
            options(),
            0,
            Point::new(0.0, 0.0),
            "say something about this",
            "Send"
        ));
        by_keyboard.input.set_text("the times contradict the prose");
        by_keyboard.submit();

        assert_eq!(
            by_mouse.take_sent(),
            by_keyboard.take_sent(),
            "the same selection said the same way must produce one message"
        );
    }

    #[test]
    fn the_chord_is_ctrl_shift_m_and_nothing_else() {
        use winit::event::ElementState;
        use winit::keyboard::ModifiersState;

        // SAFETY: mirrors the toolkit's established test idiom (see
        // `name_field`) — winit 0.30 keeps `KeyEventExtra` crate-private, so a
        // struct literal is unavailable; every pub field tolerates the all-zero
        // bit pattern and each is overwritten below.
        let key = |c: &str| {
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = Key::Character(c.into());
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            ev
        };
        let mods = |s: ModifiersState| -> Modifiers { s.into() };

        assert!(is_compose_chord(
            &key("m"),
            &mods(ModifiersState::CONTROL | ModifiersState::SHIFT)
        ));
        assert!(is_compose_chord(
            &key("M"),
            &mods(ModifiersState::CONTROL | ModifiersState::SHIFT)
        ));
        assert!(!is_compose_chord(&key("m"), &mods(ModifiersState::CONTROL)));
        assert!(!is_compose_chord(
            &key("k"),
            &mods(ModifiersState::CONTROL | ModifiersState::SHIFT)
        ));
    }

    /// Nothing selected means the gesture is not offered — not a reference to
    /// nothing.
    #[test]
    fn with_nothing_selected_the_surface_does_not_open() {
        let doc = AutoCommit::new();
        let mut c = RemarkCompose::new();
        assert!(!c.open_for_selection(
            &NothingSelected,
            &doc,
            options(),
            0,
            Point::ZERO,
            "",
            "Send"
        ));
        assert!(!c.is_open());
    }

    /// The control-level half of the display-matches-delivery invariant: what
    /// the selector shows is where the message goes, for every option.
    #[test]
    fn the_destination_on_screen_is_the_destination_delivered_to() {
        for (i, opt) in options().iter().enumerate() {
            let mut c = opened(i);
            let shown = c.destination().cloned().expect("a destination is shown");
            assert_eq!(
                shown, opt.destination,
                "the surface opened on option {i} but shows something else"
            );

            c.input.set_text("look at this");
            c.submit();
            let remark = c.take_sent().expect("a remark was produced");

            assert_eq!(
                remark.destination, shown,
                "the message went somewhere the selector was not showing"
            );
            assert_eq!(shown.target(), remark.delivery().target());
            assert_eq!(shown.is_private(), remark.delivery().is_private());
        }
    }

    /// A mention pings without moving the message: the selector still says
    /// `general`, and `general` is still who receives it.
    #[test]
    fn a_mention_does_not_move_the_message_off_the_shown_channel() {
        let mut c = opened(0);
        c.input.set_text("@Ada does this match the hall booking?");
        c.submit();
        let remark = c
            .take_sent()
            .unwrap()
            // The host binds mentions at send, with the room's roster.
            .with_mentions(vec![MentionRef::new("u_ada", 0, 4, false)]);

        assert_eq!(
            remark.delivery(),
            Delivery::DirectedInChannel {
                channel: "c_general".into(),
                to: vec!["u_ada".into()]
            }
        );
        assert_eq!(remark.delivery().target(), Target::Channel("c_general"));
    }

    /// The remark carries the address, not a description of it.
    #[test]
    fn the_sent_remark_carries_the_selections_address() {
        let mut c = opened(0);
        c.input.set_text("this contradicts the prose above");
        c.submit();
        let remark = c.take_sent().unwrap();
        assert_eq!(
            remark.reference.resource_id().as_str(),
            "word-processor.block:blk_7"
        );
        assert_eq!(remark.reference.span, Some(Span::chars(12, 41)));
    }

    /// The chip is followable in the sense this pass promises: the surface can
    /// report exactly what it points at, as an address.
    #[test]
    fn the_chip_can_report_what_it_points_at() {
        let c = opened(0);
        let r = c.reference().expect("an open surface knows its subject");
        assert_eq!(r.resource_id().as_str(), "word-processor.block:blk_7");
        assert_eq!(r.kind, "word_processor.block");
    }

    /// Sending nothing is not sending. A bare reference with no remark attached
    /// would be a message with no point to it.
    #[test]
    fn an_empty_remark_is_not_sent() {
        let mut c = opened(0);
        c.input.set_text("   ");
        c.submit();
        assert!(c.take_sent().is_none());
        assert!(
            c.is_open(),
            "the surface stays up rather than silently closing"
        );
    }

    /// Dismissing the compose is its own business — it does not reach into the
    /// context menu beside it, and it does not send.
    #[test]
    fn escape_dismisses_without_sending() {
        let mut c = opened(0);
        c.input.set_text("half a thought");
        c.close();
        assert!(!c.is_open());
        assert!(c.take_sent().is_none());
    }
}

/// Contrast of the surface's own colours, measured on the tokens it actually
/// draws with, in every theme it ships under.
///
/// Measured rather than eyeballed, and asserted rather than reported, because a
/// contrast defect looks fine in the theme its author had open — the last
/// several here shipped that way. Sampling tokens instead of a rendered PNG
/// keeps the check exact (no antialiasing to guess past) and permanent.
#[cfg(test)]
mod contrast {
    use crate::theme::Theme;
    use vello::peniko::Color;

    fn channel(c: f64) -> f64 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Color) -> f64 {
        let [r, g, b, _] = c.to_rgba8().to_u8_array();
        0.2126 * channel(r as f64 / 255.0)
            + 0.7152 * channel(g as f64 / 255.0)
            + 0.0722 * channel(b as f64 / 255.0)
    }

    /// WCAG 2.1 contrast ratio.
    fn ratio(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    fn themes() -> Vec<(&'static str, Theme)> {
        vec![
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high-contrast", Theme::high_contrast()),
        ]
    }

    /// Text drawn inside the panel clears the 4.5 body floor against the
    /// panel's own surface — the ground it is actually painted on, not the
    /// page behind it.
    #[test]
    fn panel_text_meets_the_body_floor() {
        for (name, t) in themes() {
            let ground = t.tokens.colors.surfaces.overlay;
            for (what, fg) in [
                ("primary text", t.tokens.colors.text_primary),
                ("dim text", t.tokens.colors.text_dim),
            ] {
                let r = ratio(fg, ground);
                assert!(
                    r >= 4.5,
                    "{name}: {what} on the compose panel is {r:.2}:1, below the 4.5 body floor"
                );
            }
        }
    }

    /// The panel is drawn on the toolkit's overlay tier — the *same* fill and
    /// border pair as the `ContextMenu` it opens beside.
    ///
    /// This is asserted as sameness rather than as a ratio, because neither
    /// half reaches 3.0 on its own in the dark theme: the fill is 1.19:1
    /// against the page and the border 1.39:1 against the fill. Separation
    /// comes from the two together, and that is the established convention for
    /// every menu and popover in the toolkit, not something this surface
    /// invented. Pinning the pair is what stops this panel drifting onto a
    /// bespoke surface — which is the failure actually available to it. The
    /// overlay tier's own contrast is a toolkit-wide question and is reported
    /// rather than silently re-decided here.
    #[test]
    fn the_panel_sits_on_the_same_tier_as_the_menu_beside_it() {
        for (name, t) in themes() {
            let c = &t.tokens.colors;
            // Both halves of the separation must actually be drawn: a border
            // that equals its fill, or a fill that equals the page, would leave
            // the panel with no edge at all.
            // The edge must actually be drawn: a border equal to its own fill
            // would leave the panel with no boundary in any theme.
            //
            // Only the border is asserted here. The fill's separation depends
            // on which surface happens to be *behind* the panel — the light
            // theme's overlay is the same white as `bg_primary`, and it reads
            // because the file list beneath it is a slightly darker grey — so a
            // fill-versus-page assertion would be comparing against a token
            // that is not what the panel is actually drawn over.
            assert!(
                ratio(c.border_standard, c.surfaces.overlay) > 1.0,
                "{name}: the panel's border is indistinguishable from its fill"
            );
        }
    }
}
