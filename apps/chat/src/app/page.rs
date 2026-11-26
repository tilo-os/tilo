//! Chat's message composer on a hosted page. The field and Send address the same
//! editor and typed send action as their painted controls.

use ui::a11y::{AccessNode, AccessState, Role};
use ui::composition::{Ground, HtmlCx, Own, TextLook, rgba8};
use ui::tokens::TextRole;
use ui::widget::{ButtonFace, ControlSize};

use super::{ChatApp, Hit};
use crate::i18n::{t, t_args};

impl ChatApp {
    pub(super) fn shows_main_composer(&self) -> bool {
        self.alone.is_none()
            && !(self.call.active && self.call.expanded)
            && !self.thread_is_page()
            && self.takes_messages()
            && self.main_composer_can_edit()
    }

    pub(super) fn compose_message_field(&self, cx: &mut HtmlCx<'_>) {
        if !self.shows_main_composer() {
            return;
        }
        let theme = cx.theme().clone();
        let style = theme.text(TextRole::Body);
        let ink = Some(theme.tokens.colors.text_primary);
        let rect = self.composer_rect();
        let button_width = ButtonFace::height(ControlSize::Regular, &theme);
        let frame = tilo_rich_text::ComposerFrame::layout(rect, &theme, &[], &[button_width]);
        let draft = self.composer_main.value();
        let empty = draft.trim().is_empty();
        let field = cx.part("composer", "MessageComposer", frame.editor);
        field.semantics = Some(
            AccessNode::new(Role::MultilineTextInput)
                .with_name(t("chat-composer"))
                .with_action_target(Hit::FocusMain.semantic_id()),
        );
        field.attrs.push(("data-t", Hit::FocusMain.semantic_id()));
        let fill = theme.tokens.colors.surfaces.raised;
        field.ground = Some(Ground {
            fill: Some(rgba8(fill)),
            edge: Some(rgba8(ui::surface::edge_on(fill))),
            radius: f64::from(theme.tokens.radii.control),
            inset: [0.0; 2],
        });
        field.own = Some(Own::Field {
            value: draft,
            multiline: true,
            placeholder: Some(self.conversation_placeholder(&self.active_channel)),
            look: TextLook::new(String::new(), &style, ink),
            inset: [f64::from(theme.tokens.spacing.s2); 2],
        });
        field.layout = Some("flex:1;min-width:0;".into());
        if let Some(&slot) = frame.trailing.first() {
            let send = cx.words(
                "send",
                "button",
                slot,
                TextLook::new(t("chat-send"), &style, ink),
            );
            send.semantics = Some(
                AccessNode::new(Role::Button)
                    .with_name(t("chat-send-message"))
                    .with_action_target(Hit::Send.semantic_id())
                    .with_state(AccessState::new().disabled(empty)),
            );
            if empty {
                send.attrs.push(("disabled", String::new()));
            }
        }
        cx.group(
            "message-entry",
            "div",
            "display:flex;align-items:center;",
            |part| match part.path.rsplit_once('#')?.1 {
                "composer" => Some(0),
                "send" => Some(1),
                _ => None,
            },
        );
        if let Some(reason) = &self.not_sent {
            cx.words(
                "not-sent",
                "p",
                rect,
                TextLook::new(
                    t_args("chat-not-sent", &[("reason", reason.as_str().into())]),
                    &style,
                    ink,
                ),
            );
        }
    }
}
