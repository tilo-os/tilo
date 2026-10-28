//! The pairing prompt, drawn as OS chrome.
//!
//! Every variant of this dialog is the same question in different
//! clothes: *is the thing in front of you the thing that is asking?*
//! The passkey comparison is the strongest form — two screens showing
//! the same six digits is what makes a man-in-the-middle detectable —
//! so the number is the largest thing on the surface, with the device's name right next to it.
//!
//! Built from shared `ui` widgets, so it inherits the system theme and focus handling. Nothing here is hand-drawn.
//!
//! [`PairingOverlay`] is how the shell hosts it: it follows the session's
//! [`PairingPromptHandle`], keeps the view modal above the desktop, and
//! hands the person's answer back to the agent. Accepting exists only as
//! this dialog's own controls, pressed by the person at the machine; an
//! agent or script can decline a pairing through the shell's actions, but
//! nothing outside this dialog can confirm one.
//!
//! What is typed into the passkey or PIN field stays in process memory
//! owned by the view, never in a document.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use crate::bluetooth::PairingPromptHandle;
use crate::shell::i18n::{t, t_args};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use tilo_bluez::{MAX_PIN_CODE_BYTES, PairingPrompt, PairingReply, PairingRequest};
use tilo_lock::MutexRecover as _;
use ui::TextRole;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonRole, Card, Column, Row, Spacer, Text, TextInput, TextInputKind, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Test id of the passkey / PIN field of the two entry prompts.
pub const ENTRY_FIELD_ID: &str = "bluetooth.pair.entry.field";

/// Width of the dialog in logical px.
const DIALOG_WIDTH: f32 = 460.0;
/// The largest passkey Bluetooth defines: six decimal digits.
const MAX_PASSKEY: u32 = 999_999;
/// Opacity of the scrim that separates the modal dialog from the desktop.
const SCRIM_ALPHA: f32 = 0.45;

/// The prompt widget. Holds the question and, once the user acts, the answer for the host to drain.
pub struct PairingPromptView {
    request: PairingRequest,
    /// What the person typed into the entry field, outside any document.
    entry: Arc<Mutex<String>>,
    answer: Option<PairingReply>,
    /// Which of the dialog's buttons were clicked since `sync` last looked:
    /// view state, never written to a document.
    clicked: Rc<Clicked>,
    composed: ui::widget::Composed,
}

impl PairingPromptView {
    pub fn new(request: PairingRequest) -> Self {
        Self {
            request,
            entry: Arc::new(Mutex::new(String::new())),
            answer: None,
            clicked: Rc::default(),
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn request(&self) -> &PairingRequest {
        &self.request
    }

    /// Show `request` in place — a displayed code whose typed-digit count
    /// moved on. What was typed stays only while the question is the same.
    pub fn set_request(&mut self, request: PairingRequest) {
        if request.device_path != self.request.device_path
            || std::mem::discriminant(&request.prompt)
                != std::mem::discriminant(&self.request.prompt)
        {
            self.entry.lock_recover().clear();
        }
        self.request = request;
        self.composed.invalidate();
    }

    /// Take the user's answer, if they have given one.
    pub fn take_answer(&mut self) -> Option<PairingReply> {
        self.answer.take()
    }

    /// Where the dialog card is on screen: the area a capture withholds.
    pub fn card_rect(&self) -> Option<Rect> {
        self.composed
            .element()
            .map(Widget::get_rect)
            .filter(|rect| rect.area() > 0.0)
    }

    /// Answer as though the accept button had been clicked.
    pub fn press_accept(&self) {
        self.clicked.accept.set(true);
    }

    /// Answer as though the reject (or cancel) button had been clicked.
    pub fn press_reject(&self) {
        self.clicked.reject.set(true);
    }

    /// Headline. Names the device, because "Pair with a device?" tells the user nothing they can check.
    fn title(&self) -> String {
        let device = [(
            "device",
            ui::i18n::ArgValue::from(self.request.device_name.as_str()),
        )];
        match &self.request.prompt {
            PairingPrompt::ConfirmPasskey { .. } => {
                t_args("bluetooth-prompt-title-confirm-passkey", &device)
            }
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
                t_args("bluetooth-prompt-title-display", &device)
            }
            PairingPrompt::RequestPasskey => {
                t_args("bluetooth-prompt-title-request-passkey", &device)
            }
            PairingPrompt::RequestPinCode => t_args("bluetooth-prompt-title-request-pin", &device),
            PairingPrompt::ConfirmPairing => {
                t_args("bluetooth-prompt-title-confirm-pairing", &device)
            }
            PairingPrompt::AuthorizeService { .. } => {
                t_args("bluetooth-prompt-title-authorize-service", &device)
            }
        }
    }

    /// The sentence under the headline. Says what accepting actually grants, in terms of what the device will be able to do.
    fn explanation(&self) -> String {
        match &self.request.prompt {
            PairingPrompt::ConfirmPasskey { .. } => t("bluetooth-prompt-explain-confirm-passkey"),
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
                t("bluetooth-prompt-explain-display")
            }
            PairingPrompt::RequestPasskey | PairingPrompt::RequestPinCode => {
                t("bluetooth-prompt-explain-request")
            }
            PairingPrompt::ConfirmPairing => t("bluetooth-prompt-explain-confirm-pairing"),
            PairingPrompt::AuthorizeService { .. } => {
                t("bluetooth-prompt-explain-authorize-service")
            }
        }
    }

    /// The code the dialog shows, spoken digit by digit: a screen reader
    /// reading "424 242" as two numbers is no help for comparing.
    fn spoken_code(&self) -> Option<String> {
        self.request.prompt.passkey_text().map(|code| {
            let digits = code.chars().map(String::from).collect::<Vec<_>>().join(" ");
            t_args(
                "bluetooth-prompt-a11y-code",
                &[("code", digits.as_str().into())],
            )
        })
    }

    /// What pressing the primary action answers. The typed prompts answer
    /// with what was typed — a blank or unparseable entry is a refusal, not
    /// a zero passkey, which the device would reject anyway and read as
    /// "pairing mysteriously failed".
    fn accepted(&self) -> PairingReply {
        let entry = self.entry.lock_recover().trim().to_string();
        match self.request.prompt {
            PairingPrompt::RequestPasskey => match entry.parse::<u32>() {
                Ok(n) if n <= MAX_PASSKEY && !entry.is_empty() => PairingReply::Passkey(n),
                _ => PairingReply::Reject,
            },
            PairingPrompt::RequestPinCode => {
                if entry.is_empty() || entry.len() > MAX_PIN_CODE_BYTES {
                    PairingReply::Reject
                } else {
                    PairingReply::PinCode(entry)
                }
            }
            // A displayed code has nothing to accept.
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
                PairingReply::Reject
            }
            _ => PairingReply::Accept,
        }
    }

    fn entry_field(&self) -> TextInput {
        let typed = self.entry.lock_recover().clone();
        let pin = matches!(self.request.prompt, PairingPrompt::RequestPinCode);
        let field = TextInput::local()
            .with_initial_value(&typed)
            .with_id(ENTRY_FIELD_ID)
            .with_external_buffer(Arc::clone(&self.entry))
            .with_spellcheck(false)
            .with_autocorrect(false);
        if pin {
            field
                .with_kind(TextInputKind::Code)
                .with_char_filter(|c| c.is_ascii_graphic())
                .with_accessible_name(t("bluetooth-prompt-pin-label"))
                .with_placeholder(t("bluetooth-prompt-pin-placeholder"))
        } else {
            field
                .with_kind(TextInputKind::Pin)
                .with_char_filter(|c| c.is_ascii_digit())
                .with_accessible_name(t("bluetooth-prompt-code-label"))
                .with_placeholder(t("bluetooth-prompt-code-placeholder"))
        }
    }

    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_gap(18.0)
            .push(Box::new(
                Text::new(&self.title()).with_role(TextRole::Title),
            ))
            .push(Box::new(
                Text::new(&self.explanation())
                    .with_role(TextRole::Body)
                    .with_color(theme.text_dim),
            ));

        // The number, when there is one. Large and monospaced: the whole
        // security value of this dialog is comparing two of these, and proportional digits at body size make that harder than it needs to be.
        if let Some(code) = self.request.prompt.passkey_text() {
            body = body.push(Box::new(Card::new(Box::new(
                Row::new()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&spaced(&code))
                            .with_role(TextRole::Display)
                            .with_font_family(ui::text_render::FontChoice::Monospace),
                    ))
                    .push(Box::new(Spacer::new())),
            ))));
        }

        if let PairingPrompt::DisplayPasskey { entered, .. } = &self.request.prompt
            && *entered > 0
        {
            body = body.push(Box::new(
                Text::new(&t_args(
                    "bluetooth-prompt-digits-entered",
                    &[("entered", (*entered).into())],
                ))
                .with_role(TextRole::Caption)
                .with_color(theme.text_dim),
            ));
        }

        // A service people would recognise is named; an unknown one is left
        // to the explanation above rather than shown as its UUID.
        if let PairingPrompt::AuthorizeService { uuid } = &self.request.prompt
            && let Some(service) = bluetooth_service_key(uuid)
        {
            body = body.push(Box::new(
                Text::new(&t_args(
                    "bluetooth-prompt-service",
                    &[("service", t(service).into())],
                ))
                .with_role(TextRole::Caption)
                .with_color(theme.text_dim),
            ));
        }

        if self.request.prompt.expects_entry() {
            body = body.push(Box::new(self.entry_field()));
        }

        // A display-only prompt has nothing to accept — an OK button would
        // suggest pressing it completes the pairing, when the other device does.
        let actions: Vec<Box<dyn Widget>> = if self.request.prompt.expects_answer() {
            vec![
                Box::new(self.reject_button(&t("bluetooth-prompt-reject"))),
                Box::new(
                    self.accept_button(&accept_label(&self.request.prompt))
                        .role(ButtonRole::Primary),
                ),
            ]
        } else {
            vec![Box::new(self.reject_button(&t("bluetooth-prompt-cancel")))]
        };
        let mut row = Row::new().push(Box::new(Spacer::new()));
        for a in actions {
            row = row.push(a);
        }
        body = body.push(Box::new(row));

        Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(Card::new(Box::new(body)))),
        )
    }
}

impl PairingPromptView {
    fn accept_button(&self, label: &str) -> Button {
        let clicked = Rc::clone(&self.clicked);
        Button::new("bluetooth.pair.accept", label).on_click(move || clicked.accept.set(true))
    }

    fn reject_button(&self, label: &str) -> Button {
        let clicked = Rc::clone(&self.clicked);
        Button::new("bluetooth.pair.reject", label).on_click(move || clicked.reject.set(true))
    }
}

/// The dialog's buttons clicked since the view last looked.
#[derive(Default)]
struct Clicked {
    accept: Cell<bool>,
    reject: Cell<bool>,
}

fn accept_label(prompt: &PairingPrompt) -> String {
    match prompt {
        PairingPrompt::ConfirmPasskey { .. } => t("bluetooth-prompt-accept-match"),
        PairingPrompt::AuthorizeService { .. } => t("bluetooth-prompt-accept-allow"),
        _ => t("bluetooth-prompt-accept-pair"),
    }
}

/// `123456` → `123 456`. Six digits in one run is read as a number; split in half it's read as two triples, how people actually compare them.
fn spaced(code: &str) -> String {
    if code.len() == 6
        && code.chars().all(|c| c.is_ascii_digit())
        && let (Some(first), Some(second)) = (code.get(..3), code.get(3..))
    {
        format!("{first} {second}")
    } else {
        code.to_string()
    }
}

fn pressed(event: &WindowEvent, key: NamedKey) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { event, .. }
            if event.state == ElementState::Pressed && event.logical_key == Key::Named(key)
    )
}

impl Widget for PairingPromptView {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document_ref(state, root);
        // Reject wins over a simultaneous accept: the safe reading of an
        // ambiguous answer is "no".
        let accepted = self.clicked.accept.take();
        if self.clicked.reject.take() {
            self.answer = Some(PairingReply::Reject);
        } else if accepted {
            self.answer = Some(self.accepted());
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition(theme);
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::length(DIALOG_WIDTH);
            cs.size.height = taffy::Dimension::auto();
            cs.flex_grow = 0.0;
            cs.flex_shrink = 0.0;
            let _ = taffy.set_style(child, cs);
        }
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.composed.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // A modal question: dim what it sits above, so the answer is
        // plainly about this dialog and not the window behind it.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.scrim.with_alpha(SCRIM_ALPHA),
            None,
            &self.composed.rect(),
        );
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if pressed(event, NamedKey::Escape) {
            self.answer = Some(PairingReply::Reject);
            *ctx.handled = true;
            return;
        }
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        // Enter in the entry field submits what was typed, the way the
        // device's own keyboard finishes the code.
        if !*ctx.handled && self.request.prompt.expects_entry() && pressed(event, NamedKey::Enter) {
            self.answer = Some(self.accepted());
            *ctx.handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    fn name(&self) -> &'static str {
        "PairingPromptView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("bluetooth-pairing-prompt")
    }

    fn get_rect(&self) -> Rect {
        self.composed.rect()
    }

    /// Tab stays inside the question until it is answered.
    fn traps_focus(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let description = match self.spoken_code() {
            Some(code) => format!("{} {code}", self.explanation()),
            None => self.explanation(),
        };
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(self.title())
                .with_description(description)
                .with_bounds(self.card_rect().unwrap_or_else(|| self.composed.rect()))
                .with_state(AccessState::new().modal(true).protected(true)),
        )
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.composed.take_structure_dirty()
    }
}

/// The shell's host for the pairing prompt: one question at a time, from
/// the session's [`PairingPromptHandle`], modal above the desktop.
pub struct PairingOverlay {
    handle: PairingPromptHandle,
    view: Option<PairingPromptView>,
    node: Option<NodeId>,
    focus_reset: bool,
    structure_dirty: bool,
}

impl Default for PairingOverlay {
    /// An overlay on a private handle nothing posts to.
    fn default() -> Self {
        Self::new(PairingPromptHandle::new())
    }
}

impl PairingOverlay {
    pub fn new(handle: PairingPromptHandle) -> Self {
        Self {
            handle,
            view: None,
            node: None,
            focus_reset: false,
            structure_dirty: false,
        }
    }

    pub fn handle(&self) -> &PairingPromptHandle {
        &self.handle
    }

    pub fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub fn view(&self) -> Option<&PairingPromptView> {
        self.view.as_ref()
    }

    pub fn view_mut(&mut self) -> Option<&mut PairingPromptView> {
        self.view.as_mut()
    }

    /// Where the dialog is on screen; a capture draws it as the placeholder.
    pub fn secret_rect(&self) -> Option<Rect> {
        self.view.as_ref().and_then(PairingPromptView::card_rect)
    }

    /// Deliver an answer the person gave, then follow the handle: open,
    /// update or close the dialog. Returns whether anything on screen
    /// changed.
    ///
    /// The view is only ever dropped here, between frames, and a dropped
    /// view raises a focus reset: the frame rebuilds its tree before the
    /// next event reaches a widget pointer that no longer exists.
    pub fn pump(&mut self, _state: &AutoCommit, _root: &ObjId) -> bool {
        let answer = self.view.as_mut().and_then(PairingPromptView::take_answer);
        let mut changed = answer.is_some();
        if let Some(answer) = answer {
            match answer {
                PairingReply::Reject => {
                    self.handle.dismiss();
                }
                reply => {
                    self.handle.answer(reply);
                }
            }
            self.close();
        }
        let current = self.handle.current();
        let shown = self.view.as_ref().map(|view| view.request().clone());
        match (shown, current) {
            (None, None) => {}
            (Some(_), None) => {
                self.close();
                changed = true;
            }
            (Some(shown), Some(request)) if shown == request => {}
            (Some(shown), Some(request))
                if shown.device_path == request.device_path
                    && !shown.prompt.expects_answer()
                    && !request.prompt.expects_answer() =>
            {
                if let Some(view) = self.view.as_mut() {
                    view.set_request(request);
                }
                self.structure_dirty = true;
                changed = true;
            }
            (_, Some(request)) => {
                self.view = Some(PairingPromptView::new(request));
                self.node = None;
                self.focus_reset = true;
                self.structure_dirty = true;
                changed = true;
            }
        }
        changed
    }

    fn close(&mut self) {
        if self.view.take().is_some() {
            self.node = None;
            self.focus_reset = true;
            self.structure_dirty = true;
        }
    }

    pub fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    pub fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let view = self
            .view
            .as_mut()
            .is_some_and(PairingPromptView::take_structure_dirty);
        own || view
    }

    /// The dialog's node, filling the viewport without taking part in the
    /// desktop's flex placement.
    pub fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        let view = self.view.as_mut()?;
        let node = view.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(node) {
            let mut overlay = style.clone();
            overlay.position = taffy::Position::Absolute;
            overlay.inset = taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            };
            let _ = taffy.set_style(node, overlay);
        }
        self.node = Some(node);
        Some(node)
    }

    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, origin: Point) {
        if let (Some(view), Some(node)) = (self.view.as_mut(), self.node) {
            view.apply_layout(taffy, node, origin);
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.node.is_some()
            && let Some(view) = self.view.as_mut()
        {
            view.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Route one event to the open dialog. While it is open it owns every
    /// event, except the unmodified Tab and the Enter/Space activation the
    /// frame performs on the focused control when the dialog did not use
    /// them. Returns whether the dialog is open.
    /// `ctx.node_id` is the node the event arrived at; the dialog is
    /// dispatched at its own.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // Not laid out yet means not on screen: nothing to route to.
        let (Some(view), Some(node)) = (self.view.as_mut(), self.node) else {
            return false;
        };
        view.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        // A button writes its key into `state`; read it now so the answer
        // is the one given in this event, whatever document the next frame
        // pumps.
        view.sync(ctx.state.doc(), ctx.path);
        let modifiers = ctx.modifiers.state();
        let unmodified = !modifiers.control_key() && !modifiers.alt_key() && !modifiers.super_key();
        let frame_owned = unmodified
            && (pressed(event, NamedKey::Tab)
                || pressed(event, NamedKey::Enter)
                || pressed(event, NamedKey::Space));
        if !frame_owned {
            *ctx.handled = true;
        }
        true
    }
}

/// The catalogue key naming a Bluetooth service by what it does, for the
/// assigned 16-bit UUIDs a person would recognise. `uuid` is the full
/// 128-bit form on the Bluetooth base UUID.
fn bluetooth_service_key(uuid: &str) -> Option<&'static str> {
    let uuid = uuid.to_ascii_lowercase();
    let short = uuid
        .strip_suffix("-0000-1000-8000-00805f9b34fb")
        .and_then(|head| head.strip_prefix("0000"))
        .and_then(|short| u16::from_str_radix(short, 16).ok())?;
    Some(match short {
        0x110a | 0x110b | 0x110d => "bluetooth-service-audio",
        0x1108 | 0x1112 | 0x111e | 0x111f => "bluetooth-service-calls",
        0x110c | 0x110e | 0x110f => "bluetooth-service-remote-control",
        0x1124 | 0x1812 => "bluetooth-service-input",
        0x112f | 0x1130 => "bluetooth-service-contacts",
        0x1132..=0x1134 => "bluetooth-service-messages",
        0x1105 | 0x1106 => "bluetooth-service-files",
        0x1115..=0x1117 => "bluetooth-service-network",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn bluetooth_services_are_named_or_left_out_never_shown_as_uuids() {
        assert_eq!(
            super::bluetooth_service_key("0000110B-0000-1000-8000-00805F9B34FB"),
            Some("bluetooth-service-audio")
        );
        assert_eq!(
            super::bluetooth_service_key("00001124-0000-1000-8000-00805f9b34fb"),
            Some("bluetooth-service-input")
        );
        assert_eq!(
            super::bluetooth_service_key("0000fe2c-0000-1000-8000-00805f9b34fb"),
            None
        );
        assert_eq!(
            super::bluetooth_service_key("6e400001-b5a3-f393-e0a9-e50e24dcca9e"),
            None
        );
    }

    use super::*;
    use automerge::ReadDoc as _;

    fn view(prompt: PairingPrompt) -> PairingPromptView {
        PairingPromptView::new(PairingRequest {
            device_path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".into(),
            device_name: "Studio Headphones".into(),
            prompt,
        })
    }

    fn type_entry(v: &PairingPromptView, text: &str) {
        *v.entry.lock().unwrap() = text.to_string();
    }

    fn accept_with(prompt: PairingPrompt, entry: &str) -> Option<PairingReply> {
        let mut v = view(prompt);
        type_entry(&v, entry);
        let doc = AutoCommit::new();
        v.press_accept();
        v.sync(&doc, &automerge::ROOT);
        v.take_answer()
    }

    #[test]
    fn every_prompt_kind_builds_a_layout_with_honest_copy() {
        {
            let v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
            assert!(
                v.title().contains("Studio Headphones"),
                "a prompt the user cannot check is not a prompt: {}",
                v.title()
            );
        }
        {
            assert_eq!(spaced("424242"), "424 242");
            assert_eq!(spaced("0000"), "0000", "a legacy PIN is left alone");
        }
        // Phase `the_just_works_prompt_admits_it_cannot_be_verified`.
        // The just-works prompt must say out loud that there's nothing to check, because it's the weakest flow and looks identical otherwise.
        {
            let v = view(PairingPrompt::ConfirmPairing);
            assert!(
                v.explanation().contains("nothing to check"),
                "{}",
                v.explanation()
            );
        }
        {
            let prompts = [
                PairingPrompt::ConfirmPasskey { passkey: 424_242 },
                PairingPrompt::DisplayPasskey {
                    passkey: 1,
                    entered: 3,
                },
                PairingPrompt::DisplayPinCode { pin: "0000".into() },
                PairingPrompt::RequestPasskey,
                PairingPrompt::RequestPinCode,
                PairingPrompt::ConfirmPairing,
                PairingPrompt::AuthorizeService {
                    uuid: "0000110b-0000-1000-8000-00805f9b34fb".into(),
                },
            ];
            for p in prompts {
                let mut v = view(p);
                let theme = Theme::dark();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
            }
        }
    }

    #[test]
    fn the_dialog_is_a_modal_protected_focus_trap_that_speaks_its_code() {
        let v = view(PairingPrompt::ConfirmPasskey { passkey: 424_242 });
        assert!(v.traps_focus(), "Tab must not leave an unanswered question");
        let node = v.accessibility().expect("the dialog is announced");
        assert_eq!(node.role, ui::a11y::Role::Dialog);
        assert!(node.state.modal && node.state.protected);
        assert_eq!(node.name.as_deref(), Some(v.title().as_str()));
        let description = node.description.unwrap_or_default();
        assert!(
            description.contains("4 2 4 2 4 2"),
            "the code is spoken digit by digit: {description}"
        );
        let v = view(PairingPrompt::ConfirmPairing);
        assert!(
            !v.accessibility()
                .and_then(|n| n.description)
                .unwrap_or_default()
                .contains("Code"),
            "no code, nothing to speak"
        );
    }

    #[test]
    fn prompt_answers_are_exact_and_refuse_rather_than_guess() {
        {
            let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 424_242 });
            let doc = AutoCommit::new();
            v.press_accept();
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.take_answer(), Some(PairingReply::Accept));
        }
        {
            let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
            let doc = AutoCommit::new();
            v.press_reject();
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.take_answer(), Some(PairingReply::Reject));
        }
        // Phase `reject_wins_over_a_simultaneous_accept`.
        // Reject must win over accept when both keys are somehow set — the safe reading of an ambiguous state is "no".
        {
            let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
            let doc = AutoCommit::new();
            v.press_accept();
            v.press_reject();
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.take_answer(), Some(PairingReply::Reject));
        }
        {
            assert_eq!(
                accept_with(PairingPrompt::RequestPasskey, "654321"),
                Some(PairingReply::Passkey(654_321))
            );
        }
        // Phase `an_unparseable_passkey_refuses_rather_than_sending_zero`.
        // A blank, nonsense or out-of-range entry refuses rather than sending
        // a number the device rejects anyway and reads as an unexplained failure.
        {
            for entry in ["", "   ", "abcdef", "1000000"] {
                assert_eq!(
                    accept_with(PairingPrompt::RequestPasskey, entry),
                    Some(PairingReply::Reject),
                    "entry {entry:?}"
                );
            }
        }
        {
            assert_eq!(
                accept_with(PairingPrompt::RequestPinCode, "0000"),
                Some(PairingReply::PinCode("0000".into()))
            );
            assert_eq!(
                accept_with(PairingPrompt::RequestPinCode, &"9".repeat(17)),
                Some(PairingReply::Reject),
                "longer than Bluetooth's PIN"
            );
        }
        {
            assert_eq!(
                accept_with(
                    PairingPrompt::DisplayPasskey {
                        passkey: 1,
                        entered: 0
                    },
                    ""
                ),
                Some(PairingReply::Reject)
            );
        }
        {
            let mut v = view(PairingPrompt::RequestPinCode);
            type_entry(&v, "1234");
            let doc = AutoCommit::new();
            v.press_accept();
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.take_answer(), Some(PairingReply::PinCode("1234".into())));
            assert_eq!(
                doc.keys(&automerge::ROOT).count(),
                0,
                "answering writes nothing into the document"
            );
        }
    }

    #[test]
    fn the_overlay_follows_the_handle_and_hands_back_the_answer() {
        use tilo_bluez::PairingReplySender;

        let handle = PairingPromptHandle::new();
        let mut overlay = PairingOverlay::new(handle.clone());
        let doc = AutoCommit::new();
        assert!(!overlay.pump(&doc, &automerge::ROOT));

        let (reply, _displayed) = PairingReplySender::channel();
        let request = PairingRequest {
            device_path: "/org/bluez/hci0/dev_AA".into(),
            device_name: "Keyboard".into(),
            prompt: PairingPrompt::DisplayPasskey {
                passkey: 12,
                entered: 0,
            },
        };
        handle.post(request.clone(), reply);
        assert!(overlay.pump(&doc, &automerge::ROOT));
        assert!(overlay.is_open() && overlay.take_focus_reset());
        overlay.view_mut().unwrap().sync(&doc, &automerge::ROOT);
        assert_eq!(overlay.view_mut().unwrap().take_answer(), None);

        let (reply, _) = PairingReplySender::channel();
        handle.post(
            PairingRequest {
                prompt: PairingPrompt::DisplayPasskey {
                    passkey: 12,
                    entered: 1,
                },
                ..request
            },
            reply,
        );
        assert!(overlay.pump(&doc, &automerge::ROOT));
        assert!(!overlay.take_focus_reset(), "same dialog, same focus");
        assert!(overlay.take_structure_dirty());

        // The person declines a comparison: the agent hears a refusal and
        // the dialog closes.
        let (reply, rx) = PairingReplySender::channel();
        handle.post(
            PairingRequest {
                device_path: "/org/bluez/hci0/dev_BB".into(),
                device_name: "Speaker".into(),
                prompt: PairingPrompt::ConfirmPasskey { passkey: 7 },
            },
            reply,
        );
        assert!(overlay.pump(&doc, &automerge::ROOT));
        overlay.view().unwrap().press_reject();
        overlay.view_mut().unwrap().sync(&doc, &automerge::ROOT);
        assert!(overlay.pump(&doc, &automerge::ROOT));
        assert!(!overlay.is_open() && !handle.is_prompting());
        assert!(overlay.take_focus_reset(), "a closed dialog rebuilds focus");
        assert_eq!(rx.blocking_recv().unwrap(), PairingReply::Reject);
    }
}
