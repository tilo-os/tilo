//! The pairing prompt, drawn as OS chrome.
//!
//! Every variant of this dialog is the same question in different
//! clothes: *is the thing in front of you the thing that is asking?*
//! The passkey comparison is the strongest form — two screens showing
//! the same six digits is what makes a man-in-the-middle detectable —
//! so the number is the largest thing on the surface, with the device's name right next to it.
//!
//! Built from shared `ui` widgets, so it inherits the system theme and focus handling. Nothing here is hand-drawn.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value, transaction::Transactable};
use qos_bluez::{PairingPrompt, PairingReply, PairingRequest};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, ButtonStyle, Card, Column, Row, Spacer, Text, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

pub const KEY_ACCEPT: &str = "bluetooth.pair.accept";
pub const KEY_REJECT: &str = "bluetooth.pair.reject";
/// Where the typed passkey / PIN lands for the two entry prompts.
pub const KEY_ENTRY: &str = "bluetooth.pair.entry";

/// Width of the dialog in logical px.
const DIALOG_WIDTH: f32 = 460.0;

/// The prompt widget. Holds the question and, once the user acts, the answer for the host to drain.
pub struct PairingPromptView {
    request: PairingRequest,
    answer: Option<PairingReply>,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl PairingPromptView {
    pub fn new(request: PairingRequest) -> Self {
        Self {
            request,
            answer: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn request(&self) -> &PairingRequest {
        &self.request
    }

    /// Take the user's answer, if they have given one.
    pub fn take_answer(&mut self) -> Option<PairingReply> {
        self.answer.take()
    }

    /// Headline. Names the device, because "Pair with a device?" tells the user nothing they can check.
    fn title(&self) -> String {
        match &self.request.prompt {
            PairingPrompt::ConfirmPasskey { .. } => {
                format!("Does {} show this code?", self.request.device_name)
            }
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
                format!("Type this on {}", self.request.device_name)
            }
            PairingPrompt::RequestPasskey => {
                format!("Enter the code shown on {}", self.request.device_name)
            }
            PairingPrompt::RequestPinCode => {
                format!("Enter the PIN for {}", self.request.device_name)
            }
            PairingPrompt::ConfirmPairing => format!("Pair with {}?", self.request.device_name),
            PairingPrompt::AuthorizeService { .. } => {
                format!("Let {} connect?", self.request.device_name)
            }
        }
    }

    /// The sentence under the headline. Says what accepting actually grants, in terms of what the device will be able to do.
    fn explanation(&self) -> &'static str {
        match &self.request.prompt {
            PairingPrompt::ConfirmPasskey { .. } => {
                "Compare it with the code on the other device. If they differ, something else \
                 is trying to pair — say no."
            }
            PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
                "Enter this code on the other device to finish pairing."
            }
            PairingPrompt::RequestPasskey | PairingPrompt::RequestPinCode => {
                "The code is shown on the device you are pairing. Many keyboards ask you to \
                 type it and press Enter."
            }
            PairingPrompt::ConfirmPairing => {
                "This device cannot show a code to compare, so there is nothing to check \
                 against. Only continue if you started this pairing yourself."
            }
            PairingPrompt::AuthorizeService { .. } => {
                "A device you have already paired is asking to use a new service. Paired \
                 devices can send input and audio to this machine."
            }
        }
    }

    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_gap(18.0)
            .push(Box::new(Text::new(&self.title()).with_size(20.0)))
            .push(Box::new(
                Text::new(self.explanation())
                    .with_size(13.0)
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
                            .with_size(38.0)
                            .with_font_family(ui::text_render::FontChoice::Monospace),
                    ))
                    .push(Box::new(Spacer::new())),
            ))));
        }

        if let PairingPrompt::DisplayPasskey { entered, .. } = &self.request.prompt
            && *entered > 0
        {
            body = body.push(Box::new(
                Text::new(&format!("{entered} of 6 digits entered"))
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ));
        }

        if let PairingPrompt::AuthorizeService { uuid } = &self.request.prompt {
            body = body.push(Box::new(
                Text::new(&format!("Service {uuid}"))
                    .with_size(11.0)
                    .with_color(theme.text_dim),
            ));
        }

        if self.request.prompt.expects_entry() {
            body = body.push(Box::new(
                TextInput::new(Vec::new())
                    .with_id("bluetooth.pair.entry.field")
                    .with_on_change_path(KEY_ENTRY)
                    .with_placeholder(match self.request.prompt {
                        PairingPrompt::RequestPinCode => "PIN",
                        _ => "6-digit code",
                    })
                    .with_font_size(15.0),
            ));
        }

        // A display-only prompt has nothing to accept — an OK button would
        // suggest pressing it completes the pairing, when the other device does.
        let actions: Vec<Box<dyn Widget>> = if self.request.prompt.expects_answer() {
            vec![
                Box::new(Button::new(
                    "bluetooth.pair.reject",
                    "Don't pair",
                    Some(KEY_REJECT),
                )),
                Box::new(
                    Button::new(
                        "bluetooth.pair.accept",
                        accept_label(&self.request.prompt),
                        Some(KEY_ACCEPT),
                    )
                    .with_style(ButtonStyle::Primary),
                ),
            ]
        } else {
            vec![Box::new(Button::new(
                "bluetooth.pair.reject",
                "Cancel",
                Some(KEY_REJECT),
            ))]
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

fn accept_label(prompt: &PairingPrompt) -> &'static str {
    match prompt {
        PairingPrompt::ConfirmPasskey { .. } => "They match",
        PairingPrompt::RequestPasskey | PairingPrompt::RequestPinCode => "Pair",
        PairingPrompt::AuthorizeService { .. } => "Allow",
        _ => "Pair",
    }
}

/// `123456` → `123 456`. Six digits in one run is read as a number; split in half it's read as two triples, how people actually compare them.
fn spaced(code: &str) -> String {
    if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

fn read_text(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Object(automerge::ObjType::Text), id))) => {
            state.text(&id).unwrap_or_default()
        }
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(t) => t.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

impl Widget for PairingPromptView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, KEY_REJECT) {
            let _ = state.put(root, KEY_REJECT, ScalarValue::Boolean(false));
            self.answer = Some(PairingReply::Reject);
        } else if read_bool(state, root, KEY_ACCEPT) {
            let _ = state.put(root, KEY_ACCEPT, ScalarValue::Boolean(false));
            // The typed prompts answer with what was typed — a blank or unparseable
            // entry is a refusal, not a zero passkey, which the device would reject anyway and read as "pairing mysteriously failed".
            self.answer = Some(match self.request.prompt {
                PairingPrompt::RequestPasskey => {
                    match read_text(state, root, KEY_ENTRY).trim().parse::<u32>() {
                        Ok(n) => PairingReply::Passkey(n),
                        Err(_) => PairingReply::Reject,
                    }
                }
                PairingPrompt::RequestPinCode => {
                    let pin = read_text(state, root, KEY_ENTRY).trim().to_string();
                    if pin.is_empty() {
                        PairingReply::Reject
                    } else {
                        PairingReply::PinCode(pin)
                    }
                }
                _ => PairingReply::Accept,
            });
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition(theme);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
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
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    #[allow(clippy::too_many_arguments)]
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
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "PairingPromptView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("bluetooth-pairing-prompt")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(prompt: PairingPrompt) -> PairingPromptView {
        PairingPromptView::new(PairingRequest {
            device_path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".into(),
            device_name: "Studio Headphones".into(),
            prompt,
        })
    }

    fn press(doc: &mut AutoCommit, key: &str) {
        let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
    }

    #[test]
    fn the_headline_names_the_device() {
        let v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
        assert!(
            v.title().contains("Studio Headphones"),
            "a prompt the user cannot check is not a prompt: {}",
            v.title()
        );
    }

    #[test]
    fn accepting_a_comparison_answers_accept() {
        let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 424_242 });
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_ACCEPT);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.take_answer(), Some(PairingReply::Accept));
    }

    #[test]
    fn declining_answers_reject() {
        let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_REJECT);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.take_answer(), Some(PairingReply::Reject));
    }

    /// Reject must win over accept when both keys are somehow set — the safe reading of an ambiguous state is "no".
    #[test]
    fn reject_wins_over_a_simultaneous_accept() {
        let mut v = view(PairingPrompt::ConfirmPasskey { passkey: 1 });
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_ACCEPT);
        press(&mut doc, KEY_REJECT);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.take_answer(), Some(PairingReply::Reject));
    }

    #[test]
    fn a_typed_passkey_is_parsed_and_returned() {
        let mut v = view(PairingPrompt::RequestPasskey);
        let mut doc = AutoCommit::new();
        let _ = doc.put(
            &automerge::ROOT,
            KEY_ENTRY,
            ScalarValue::Str("654321".into()),
        );
        press(&mut doc, KEY_ACCEPT);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.take_answer(), Some(PairingReply::Passkey(654_321)));
    }

    /// A blank or nonsense entry refuses rather than sending a zero, which the device rejects anyway and reads as an unexplained failure.
    #[test]
    fn an_unparseable_passkey_refuses_rather_than_sending_zero() {
        for entry in ["", "   ", "abcdef"] {
            let mut v = view(PairingPrompt::RequestPasskey);
            let mut doc = AutoCommit::new();
            let _ = doc.put(&automerge::ROOT, KEY_ENTRY, ScalarValue::Str(entry.into()));
            press(&mut doc, KEY_ACCEPT);
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                v.take_answer(),
                Some(PairingReply::Reject),
                "entry {entry:?}"
            );
        }
    }

    #[test]
    fn a_typed_pin_is_returned_verbatim() {
        let mut v = view(PairingPrompt::RequestPinCode);
        let mut doc = AutoCommit::new();
        let _ = doc.put(&automerge::ROOT, KEY_ENTRY, ScalarValue::Str("0000".into()));
        press(&mut doc, KEY_ACCEPT);
        v.sync(&mut doc, &automerge::ROOT);
        assert_eq!(v.take_answer(), Some(PairingReply::PinCode("0000".into())));
    }

    #[test]
    fn six_digits_are_split_for_comparison() {
        assert_eq!(spaced("424242"), "424 242");
        assert_eq!(spaced("0000"), "0000", "a legacy PIN is left alone");
    }

    /// The just-works prompt must say out loud that there's nothing to check, because it's the weakest flow and looks identical otherwise.
    #[test]
    fn the_just_works_prompt_admits_it_cannot_be_verified() {
        let v = view(PairingPrompt::ConfirmPairing);
        assert!(
            v.explanation().contains("nothing to check"),
            "{}",
            v.explanation()
        );
    }

    #[test]
    fn every_prompt_builds_a_layout() {
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
