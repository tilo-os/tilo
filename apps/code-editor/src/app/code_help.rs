//! Code help (suggestions, problems, go to definition) comes from a language
//! server. When it cannot run, the status bar says so in a person's words, and
//! the reason sits behind that readout: a click, or activating it from an
//! assistive technology, opens a sheet that says why and offers to try again.

use ui::chrome::status_bar::{StatusReadout, draw_readouts};
use ui::i18n::I18nManager;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::dialogs::{Answer, Wording};
use super::{CodeApp, LspStatus};
use crate::lsp::LspFailure;

/// The semantic target of the readout's accessible button.
const TARGET: &str = "code.code-help";

#[derive(Default)]
pub(super) struct CodeHelpReadout {
    /// Where the readout was last painted; empty while it is not shown.
    rect: Rect,
    hovered: bool,
    painted_hovered: bool,
    /// Whether the sheet saying why is up.
    asked: bool,
}

/// What each of the sheet's answers does, in the order it lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CodeHelpAnswer {
    TryAgain,
    CopyDetails,
    Close,
}

impl CodeApp {
    fn code_help_failure(&self) -> Option<&LspFailure> {
        match &self.lsp_status {
            LspStatus::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    /// What the status bar says while code help is missing, else empty.
    pub(super) fn code_help_readout(&self) -> String {
        if self.code_help_failure().is_none() {
            return String::new();
        }
        if !self.editor.is_bound() {
            return I18nManager::lookup_plain("code-editor-code-help-unavailable-any");
        }
        I18nManager::format(
            "code-editor-code-help-unavailable",
            &[("language", self.editor.language().label().into())],
        )
    }

    pub(super) fn code_help_asked(&self) -> bool {
        self.code_help.asked && self.code_help_failure().is_some()
    }

    fn ask_code_help(&mut self) {
        self.code_help.asked = self.code_help_failure().is_some();
    }

    pub(super) fn code_help_answers(&self) -> Vec<CodeHelpAnswer> {
        let mut answers = vec![CodeHelpAnswer::TryAgain];
        if self
            .code_help_failure()
            .and_then(LspFailure::detail)
            .is_some()
        {
            answers.push(CodeHelpAnswer::CopyDetails);
        }
        answers.push(CodeHelpAnswer::Close);
        answers
    }

    pub(super) fn code_help_wording(&self) -> Wording {
        let failure = self.code_help_failure();
        let server = failure.map_or("", LspFailure::server);
        let said = |key: &str| I18nManager::format(key, &[("server", server.into())]);
        let body = match failure {
            Some(LspFailure::NotInstalled { .. }) => {
                let how = if server == "rust-analyzer" {
                    I18nManager::lookup_plain("code-editor-code-help-install-rust")
                } else {
                    said("code-editor-code-help-install")
                };
                format!("{}\n\n{how}", said("code-editor-code-help-not-installed"))
            }
            Some(LspFailure::Stopped { .. }) => said("code-editor-code-help-stopped"),
            Some(LspFailure::Unknown { .. }) | None => said("code-editor-code-help-failed"),
        };
        let actions = self
            .code_help_answers()
            .into_iter()
            .map(|answer| match answer {
                CodeHelpAnswer::TryAgain => {
                    Answer::primary(I18nManager::lookup_plain("code-editor-code-help-try-again"))
                }
                CodeHelpAnswer::CopyDetails => Answer::secondary(I18nManager::lookup_plain(
                    "code-editor-code-help-copy-details",
                )),
                CodeHelpAnswer::Close => {
                    Answer::secondary(I18nManager::lookup_plain("code-editor-code-help-close"))
                }
            })
            .collect();
        Wording {
            title: I18nManager::lookup_plain("code-editor-code-help-title"),
            body,
            actions,
            dismissable: true,
        }
    }

    /// "Try again" lets the next sync start the server afresh; "Copy details"
    /// leaves the sheet up, so the person can still try again.
    pub(super) fn answer_code_help(&mut self, index: usize) {
        match self.code_help_answers().get(index) {
            Some(CodeHelpAnswer::TryAgain) => {
                self.code_help.asked = false;
                self.reset_language_server();
            }
            Some(CodeHelpAnswer::CopyDetails) => {
                if let Some(detail) = self.code_help_failure().and_then(LspFailure::detail) {
                    ui::text_selection::clipboard_copy(detail);
                }
            }
            Some(CodeHelpAnswer::Close) | None => self.code_help.asked = false,
        }
    }

    /// Paint the status bar in `band`, leading with the code-help readout
    /// while code help is unavailable.
    pub(super) fn draw_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        band: Rect,
    ) {
        let left = self.status.left();
        let right = self.status.right();
        let Some(trouble) = self.status.trouble() else {
            self.code_help.rect = Rect::ZERO;
            self.code_help.hovered = false;
            self.code_help.painted_hovered = false;
            ui::chrome::status_bar::draw(
                scene,
                transform,
                text_renderer,
                theme,
                band,
                &left,
                &right,
            );
            return;
        };
        let mut leading = vec![StatusReadout::action(&trouble)];
        if !left.is_empty() {
            leading.push(StatusReadout::text(&left));
        }
        let rects = draw_readouts(
            scene,
            transform,
            text_renderer,
            theme,
            band,
            &leading,
            &[StatusReadout::text(&right)],
            self.code_help.hovered.then_some(0),
        );
        self.code_help.rect = rects.first().copied().unwrap_or(Rect::ZERO);
        self.code_help.painted_hovered = self.code_help.hovered;
    }

    pub(super) fn code_help_needs_redraw(&self) -> bool {
        self.code_help.hovered != self.code_help.painted_hovered
    }

    /// Follow the pointer over the readout, and open the sheet on a press on
    /// it. Returns `true` when the press was the readout's.
    pub(super) fn code_help_pointer(&mut self, event: &WindowEvent) -> bool {
        self.code_help.hovered = self.code_help.rect.contains(self.last_cursor);
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if pressed && self.code_help.hovered {
            self.ask_code_help();
            return true;
        }
        false
    }

    pub(super) fn code_help_access(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let trouble = self.status.trouble()?;
        Some(
            AccessNode::new(Role::Button)
                .with_key(ui::a11y::hash_key(&TARGET))
                .with_name(trouble)
                .with_description(I18nManager::lookup_plain(
                    "code-editor-code-help-description",
                ))
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.code_help.rect)
                .with_action_target(TARGET),
        )
    }

    pub(super) fn code_help_action(&mut self, target: &str, action: &ui::a11y::Action) -> bool {
        if target != TARGET || !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        self.ask_code_help();
        true
    }
}
