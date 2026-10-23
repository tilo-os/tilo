//! The palette asking before an action goes ahead.
//!
//! While it asks, the palette is the shared [`ModalSheet`] over the window:
//! the action's question as its title, what the action does or what is lost
//! as its body, and the action's own button beside Cancel. An irreversible
//! action that names what it destroys ("Erase Samsung T7?") asks for that
//! name in a labelled field in the body ([`PromptBody::confirm_phrase`]); its
//! button is the action's verb, drawn destructive, and stays disabled until
//! the name is typed exactly. A destructive question starts on Cancel, and
//! Enter in the field for the name never goes ahead, even once it matches:
//! only the action's button does. Nothing typed here is searched.
//!
//! [`ModalSheet`]: crate::widget::ModalSheet

use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use super::{ActionPalette, ConfirmationPolicy, ObjId, PendingOperation};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::modal_dialog::{ModalAction, ModalSpec, body_width};
use crate::widget::{Button, ButtonRole, PromptBody, Row, TextInput, Widget};

/// What an irreversible action with no description of its own loses.
const IRREVERSIBLE: &str = "This can't be undone.";

/// The answers in the order the palette keeps them: the action, then the way
/// out. The keyboard's parts are the field for a name (0), then these (1, 2).
pub(super) const SUBMIT: usize = 0;
pub(super) const CANCEL: usize = 1;

/// Live state while the palette asks.
pub(super) struct ConfirmState {
    pub(super) pending: PendingOperation,
    /// What the sheet asks, naming the action and what it acts on.
    pub(super) question: String,
    /// What the action does or what is lost.
    pub(super) detail: String,
    pub(super) destructive: bool,
    /// The action's own button: its label, or its verb where a name is typed.
    pub(super) button_label: String,
    phrase: Option<PhraseField>,
}

/// The name an irreversible action asks to be typed, the labelled field for
/// it, and where the sheet's last paint put the field.
struct PhraseField {
    body: PromptBody,
    placed: Option<(TaffyTree<()>, NodeId)>,
}

impl ConfirmState {
    /// Ask about `pending`, labelled `label` and described by `description`;
    /// `asked` says who decides where a room's grants ask rather than the
    /// action itself.
    pub(super) fn new(
        pending: PendingOperation,
        label: &str,
        description: Option<&str>,
        asked: Option<&str>,
        destructive: bool,
    ) -> Self {
        let (question, detail) = confirmation_text(label, description);
        let detail = asked.map_or(detail, str::to_owned);
        let button_label = label.trim().trim_end_matches(['.', '?']).to_owned();
        let button_label = if button_label.is_empty() {
            "Confirm".to_owned()
        } else {
            button_label
        };
        let ConfirmationPolicy::Phrase { phrase } = pending.confirmation.clone() else {
            return Self {
                pending,
                question,
                detail,
                destructive,
                button_label,
                phrase: None,
            };
        };
        let message = if detail.is_empty() {
            IRREVERSIBLE.to_owned()
        } else {
            detail
        };
        let mut body = PromptBody::confirm_phrase(&message, &phrase);
        let input = std::mem::replace(body.input_mut(), TextInput::local());
        *body.input_mut() = input
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_id("palette-confirmation-phrase");
        Self {
            question: self::question(label, Some(&phrase)),
            detail: message,
            destructive: true,
            button_label: verb(label),
            phrase: Some(PhraseField { body, placed: None }),
            pending,
        }
    }

    /// Whether this asks for a name to be typed.
    pub(super) fn typed(&self) -> bool {
        self.phrase.is_some()
    }

    /// The labelled field for the name, for an action that asks for one.
    pub(super) fn phrase_field(&self) -> Option<&PromptBody> {
        self.phrase.as_ref().map(|phrase| &phrase.body)
    }

    pub(super) fn phrase_field_mut(&mut self) -> Option<&mut PromptBody> {
        self.phrase.as_mut().map(|phrase| &mut phrase.body)
    }

    /// What the person typed as the name, for an action that asks for one.
    #[cfg(test)]
    pub(super) fn typed_text(&self) -> Option<&str> {
        self.phrase_field().map(PromptBody::value)
    }

    /// What stands in the field for the name; it searches nothing.
    pub(super) fn set_typed(&mut self, text: &str) {
        if let Some(body) = self.phrase_field_mut() {
            body.set_value(text);
        }
    }

    /// Whether what the person typed lets the action go ahead: always for
    /// a question, the exact name for a phrase.
    pub(super) fn satisfied(&self) -> bool {
        self.phrase_field().is_none_or(PromptBody::confirms)
    }

    /// The keyboard's first stop when the sheet opens: Cancel where the
    /// action destroys something, else the action's own button.
    pub(super) fn first_part(&self) -> usize {
        1 + if self.destructive { CANCEL } else { SUBMIT }
    }
}

/// The question and its detail for an action labelled `label`.
pub(super) fn confirmation_text(label: &str, description: Option<&str>) -> (String, String) {
    let label = label.trim().trim_end_matches(['.', '?']);
    let question = if label.is_empty() {
        "Go ahead with this action?".to_owned()
    } else {
        format!("{label}?")
    };
    // Generated action docs include their heading in the description. The
    // heading is already the dialog title, not another sentence in its body.
    let description = description.map(str::trim).map(|text| {
        text.strip_prefix(label)
            .filter(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
            .map_or(text, str::trim_start)
    });
    let detail = match description.filter(|text| !text.is_empty()) {
        Some(text) if text.ends_with(['.', '!', '?']) => text.to_owned(),
        Some(text) => format!("{text}."),
        None => String::new(),
    };
    (question, detail)
}

/// The evidence the door expects for `policy`, given by the person's click.
pub(super) fn confirmation_evidence(policy: &ConfirmationPolicy) -> serde_json::Value {
    match policy {
        ConfirmationPolicy::None => serde_json::json!({}),
        ConfirmationPolicy::Confirm => serde_json::json!({ "confirm": true }),
        ConfirmationPolicy::Phrase { phrase } => {
            serde_json::json!({ "confirmation_phrase": phrase })
        }
    }
}

/// The question for an action labelled `label`, naming `target` when it
/// destroys one: "Erase Samsung T7?" for "Erase drive" and "Samsung T7".
pub(super) fn question(label: &str, target: Option<&str>) -> String {
    let label = label.trim().trim_end_matches(['.', '?', '\u{2026}']);
    match (
        label.is_empty(),
        target.map(str::trim).filter(|t| !t.is_empty()),
    ) {
        (true, _) => "Go ahead with this action?".to_owned(),
        (false, Some(target)) => format!("{} {target}?", verb(label)),
        (false, None) => format!("{label}?"),
    }
}

/// The verb an action's label starts with: "Erase" of "Erase drive".
fn verb(label: &str) -> String {
    label
        .trim()
        .trim_end_matches(['.', '?', '\u{2026}'])
        .split_whitespace()
        .next()
        .unwrap_or("Confirm")
        .to_owned()
}

/// The answers, in [`SUBMIT`], [`CANCEL`] order: the action's button, in
/// `role` where it is the question's own, and Cancel.
pub(super) fn controls(
    command: &std::rc::Rc<std::cell::Cell<Option<bool>>>,
    submit: &str,
    role: Option<ButtonRole>,
) -> Row {
    let go = command.clone();
    let cancel = command.clone();
    let mut submit = Button::new("palette-submit", submit).on_click(move || go.set(Some(true)));
    if let Some(role) = role {
        submit = submit.role(role);
    }
    Row::new().push(Box::new(submit)).push(Box::new(
        Button::new("palette-cancel", "Cancel").on_click(move || cancel.set(Some(false))),
    ))
}

/// Lay `widget` out at `origin` in `width`, in a tree of its own.
#[allow(
    clippy::cast_possible_truncation,
    reason = "sheet widths are logical pixels, far inside f32's range"
)]
fn place(
    widget: &mut dyn Widget,
    theme: &Theme,
    origin: Point,
    width: f64,
    text: &mut TextRenderer,
) -> Option<(TaffyTree<()>, NodeId)> {
    let mut taffy = TaffyTree::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = widget.build_layout(&mut taffy, theme, &mut nodes);
    crate::widget::layout::solve_layout(
        &mut taffy,
        node,
        Size {
            width: AvailableSpace::Definite(width as f32),
            height: AvailableSpace::MaxContent,
        },
        &mut |known, available, id| {
            nodes.get(&id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, text)
            })
        },
    )
    .ok()?;
    widget.apply_layout(&taffy, node, origin);
    Some((taffy, node))
}

impl ActionPalette {
    /// Ask the person to confirm `pending`, labelled `label`.
    pub(super) fn present_confirmation(
        &mut self,
        pending: PendingOperation,
        label: &str,
        description: Option<&str>,
        asked: Option<&str>,
        destructive: bool,
    ) {
        let state = ConfirmState::new(pending, label, description, asked, destructive);
        let role = if state.destructive {
            ButtonRole::Destructive
        } else {
            ButtonRole::Primary
        };
        self.controls = controls(&self.command, &state.button_label, Some(role));
        self.confirmation_sheet.reset();
        self.search_announcement = Some(format!("{} {}", state.question, state.detail));
        // What the search field held (a query, an argument) is done with;
        // leaving the question rebuilds it.
        self.field = self.field_source.build().control_size(super::FIELD_SIZE);
        self.results.set_groups(Vec::new());
        self.results.set_empty_text(&state.detail);
        let first = state.first_part();
        self.confirm = Some(state);
        self.sync_confirmation();
        self.focus_part(first);
    }

    /// The answers back to Confirm and Cancel once the palette stops asking
    /// about one action (a history review asks with them too).
    pub(super) fn reset_controls(&mut self) {
        self.controls = controls(&self.command, "Confirm", None);
    }

    /// The action's button follows what is typed: disabled until the name
    /// is exact.
    pub(super) fn sync_confirmation(&mut self) {
        let Some(state) = &self.confirm else { return };
        let enabled = state.satisfied();
        if let Some(submit) = self
            .controls
            .children_mut()
            .into_iter()
            .nth(SUBMIT)
            .and_then(|control| control.as_any_mut())
            .and_then(|any| any.downcast_mut::<Button>())
        {
            submit.set_enabled(enabled);
        }
    }

    /// Whether the action's button can be pressed now.
    pub(super) fn submit_enabled(&self) -> bool {
        self.confirm.as_ref().is_none_or(ConfirmState::satisfied)
    }

    /// The part Tab moves to from `focused`: for a typed name the field,
    /// Cancel and the action in the order they stand, skipping the action
    /// while it is disabled; for a question, between its two answers.
    pub(super) fn next_confirmation_part(&self, focused: usize, back: bool) -> usize {
        let typed = self.confirm.as_ref().is_some_and(ConfirmState::typed);
        if !typed {
            return if focused == 1 + SUBMIT {
                1 + CANCEL
            } else {
                1 + SUBMIT
            };
        }
        let mut order = vec![0, 1 + CANCEL];
        if self.submit_enabled() {
            order.push(1 + SUBMIT);
        }
        let at = order.iter().position(|&part| part == focused).unwrap_or(0);
        let next = if back {
            (at + order.len() - 1) % order.len()
        } else {
            (at + 1) % order.len()
        };
        order.get(next).copied().unwrap_or(0)
    }

    /// Hand `event` to the field for the name, when the palette asks for
    /// one; a click into it puts the keyboard there.
    pub(super) fn route_to_phrase(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) {
        let Some(phrase) = self
            .confirm
            .as_mut()
            .and_then(|state| state.phrase.as_mut())
        else {
            return;
        };
        // Before the sheet's first paint the field has no place yet; keys
        // and committed text still reach it.
        let unplaced;
        let (taffy, node) = if let Some((taffy, node)) = &phrase.placed {
            (taffy, *node)
        } else {
            let mut taffy = TaffyTree::new();
            let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
                return;
            };
            unplaced = taffy;
            (&unplaced, node)
        };
        let mut handled = false;
        let mut focus = None;
        phrase.body.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: scale,
                modifiers: &modifiers,
                state: doc.reborrow(),
                path,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy,
            },
        );
        if focus.is_some() {
            self.focus_part(0);
        }
        self.sync_confirmation();
    }

    /// Paint the question as the shared sheet over `window`.
    pub(super) fn draw_confirmation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let window = presence.viewport.get().unwrap_or(self.rect);
        let focused = |index: usize| {
            self.controls
                .children()
                .get(index)
                .is_some_and(|control| control.is_focused())
        };
        let (submit_focused, cancel_focused) = (focused(SUBMIT), focused(CANCEL));
        let enabled = self.submit_enabled();
        let Some(state) = self.confirm.as_mut() else {
            return;
        };
        let submit = if state.destructive {
            ModalAction::danger(&state.button_label)
        } else {
            ModalAction::primary(&state.button_label)
        };
        // In `SUBMIT`, `CANCEL` order: the sheet answers `Action(SUBMIT)`.
        let actions = [
            submit.enabled(enabled).focused(submit_focused),
            ModalAction::secondary("Cancel").focused(cancel_focused),
        ];
        let spec = ModalSpec::new(&state.question)
            .actions(&actions)
            .closable(false);
        match state.phrase.as_mut() {
            None => self.confirmation_sheet.paint_message(
                scene,
                transform,
                text_renderer,
                theme,
                window,
                &spec,
                &state.detail,
            ),
            Some(phrase) => {
                // The body is the labelled field: as tall as it lays out in
                // the sheet's body width, placed where the sheet puts it.
                let width = body_width(window, spec.max_width, theme);
                let height = place(&mut phrase.body, theme, Point::ZERO, width, text_renderer)
                    .map_or(0.0, |_| phrase.body.get_rect().height());
                let spec = spec.body_height(height);
                let mut placed = None;
                let body = &mut phrase.body;

                self.confirmation_sheet.paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    window,
                    &spec,
                    |scene, transform, text_renderer, theme, rect: Rect| {
                        placed = place(body, theme, rect.origin(), rect.width(), text_renderer);
                        body.draw(scene, transform, text_renderer, theme, presence);
                    },
                );
                phrase.placed = placed;
            }
        }
        if let Some(layout) = self.confirmation_sheet.layout() {
            self.rect = layout.scrim;
        }
    }
}
