//! Local speech controls on the existing shell overlay stack. A request opens
//! one-shot consent; neither a shortcut nor an action grants microphone access.
use super::layers::{LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Routed, ShellLayer};
use crate::compositor::dictation::{DictationError, DictationStatus, SessionId};
use crate::shell::i18n::t;
use std::time::Instant;
use ui::a11y::AccessNode;
use ui::widget::{PermissionPrompt, PromptChoice, Widget};
use vello::kurbo::Rect;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Toggle,
    Consent(SessionId),
    Finish(SessionId),
    Copy(SessionId),
    Cancel(Option<SessionId>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Consent,
    Running(DictationStatus),
    Failure(&'static str),
}
#[derive(Default)]
pub(crate) struct DictationSurface {
    id: Option<SessionId>,
    phase: Option<Phase>,
    prompt: Option<PermissionPrompt>,
    pending: Option<Request>,
    changed: bool,
}
impl DictationSurface {
    pub(crate) fn owns_key(&self, key: &Key) -> bool {
        self.prompt.as_ref().is_some_and(|prompt| {
            key == &Key::Named(NamedKey::Escape)
                || prompt.is_focused()
                    && matches!(
                        key,
                        Key::Named(
                            NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown
                                | NamedKey::Enter
                                | NamedKey::Space
                        )
                    )
        })
    }

    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
    pub(crate) fn request_toggle(&mut self) {
        self.pending = Some(Request::Toggle);
    }
    pub(crate) fn request_finish(&mut self) {
        if let Some(id) = self.id {
            self.pending = Some(Request::Finish(id));
        }
    }
    pub(crate) fn request_cancel(&mut self) {
        self.pending = Some(Request::Cancel(self.id));
    }
    pub(crate) fn enqueue(&mut self, request: Request) {
        self.pending = Some(request);
    }
    pub(crate) fn take_request(&mut self) -> Option<Request> {
        self.pending.take()
    }
    pub(crate) fn session(&self) -> Option<SessionId> {
        self.id
    }
    pub(crate) fn preparing(&self) -> bool {
        self.phase == Some(Phase::Preparing)
    }
    pub(crate) fn is_recovery(&self) -> bool {
        matches!(
            self.phase,
            Some(Phase::Running(DictationStatus::Recovery { .. }))
        )
    }
    pub(crate) fn is_consent(&self) -> bool {
        self.phase == Some(Phase::Consent)
    }
    pub(crate) fn surface_rect(geometry: LayerGeometry<'_>) -> Rect {
        let area = geometry.focused_output.unwrap_or(geometry.viewport);
        let spacing = ui::tokens::Spacing::BASE;
        let pad = f64::from(spacing.s4);
        Rect::new(
            area.x0 + pad,
            area.y0 + pad,
            area.x1 - pad,
            area.y0 + pad + ui::widget::permission_prompt::PROMPT_HEIGHT,
        )
    }
    fn show(&mut self, phase: Phase, message: &'static str) {
        if self.phase == Some(phase) {
            return;
        }
        let labels = if phase == Phase::Consent {
            ["dictation-record-once", "dictation-cancel"]
        } else if matches!(phase, Phase::Running(DictationStatus::Recovery { .. })) {
            ["dictation-copy-transcript", "dictation-discard-transcript"]
        } else if phase == Phase::Running(DictationStatus::Listening) {
            ["dictation-finish", "dictation-cancel"]
        } else {
            ["dictation-cancel", "dictation-cancel"]
        };
        let mut prompt = PermissionPrompt::for_local_input(
            t("dictation-title"),
            t(message),
            labels,
            phase == Phase::Consent,
        );
        prompt.set_focus(phase == Phase::Consent);
        self.changed = true;
        self.prompt = Some(prompt);
        self.phase = Some(phase);
    }
    pub(crate) fn prepare(&mut self, id: SessionId) {
        self.id = Some(id);
        self.show(Phase::Preparing, "dictation-preparing");
    }
    pub(crate) fn consent(&mut self) {
        self.show(Phase::Consent, "dictation-consent");
    }
    pub(crate) fn status(&mut self, status: DictationStatus) {
        let message = match status {
            DictationStatus::Idle => {
                self.close();
                return;
            }
            DictationStatus::Recovery {
                copied,
                copy_failed,
            } => {
                if copy_failed {
                    "dictation-copy-failed"
                } else if copied {
                    "dictation-transcript-copied"
                } else {
                    "dictation-transcript-retained"
                }
            }
            DictationStatus::Preparing => "dictation-preparing",
            DictationStatus::Loading => "dictation-loading",
            DictationStatus::Listening => "dictation-listening",
            DictationStatus::StoppingRecording => "dictation-stopping-microphone",
            DictationStatus::Finishing => "dictation-finishing",
            DictationStatus::Stopping => "dictation-stopping",
            DictationStatus::Unavailable(error) => {
                self.id = None;
                self.show(Phase::Failure(error_key(error)), error_key(error));
                return;
            }
        };
        self.show(Phase::Running(status), message);
    }
    pub(crate) fn failure(&mut self, error: DictationError) {
        let key = match error {
            DictationError::NoEditableField | DictationError::SensitiveField => {
                "dictation-editable-required"
            }
            DictationError::Busy => "dictation-busy",
            DictationError::TargetChanged | DictationError::StaleSession => {
                "dictation-target-changed"
            }
            DictationError::TimedOut => "dictation-timeout",
            _ => "dictation-unavailable",
        };
        self.id = None;
        self.show(Phase::Failure(key), key);
    }
    fn affirmative_target(&self) -> String {
        format!(
            "dictation:{:?}:{}",
            self.id,
            if matches!(
                self.phase,
                Some(Phase::Running(DictationStatus::Recovery { .. }))
            ) {
                "copy-transcript"
            } else if self.is_consent() {
                "record-once"
            } else if self.phase == Some(Phase::Running(DictationStatus::Listening)) {
                "finish"
            } else {
                "dismiss"
            }
        )
    }
    fn cancel_target(&self) -> String {
        format!("dictation:{:?}:cancel", self.id)
    }
    fn close(&mut self) {
        self.changed |= self.prompt.is_some();
        self.id = None;
        self.phase = None;
        self.prompt = None;
    }
    fn choice(&mut self, choice: PromptChoice) {
        self.pending = match (self.phase, self.id, choice) {
            (
                Some(Phase::Running(DictationStatus::Recovery { .. })),
                Some(id),
                PromptChoice::Allow,
            ) => Some(Request::Copy(id)),
            (Some(Phase::Consent), Some(id), PromptChoice::Allow) => Some(Request::Consent(id)),
            (Some(Phase::Running(DictationStatus::Listening)), Some(id), PromptChoice::Allow) => {
                Some(Request::Finish(id))
            }
            _ => Some(Request::Cancel(self.id)),
        };
    }
}
fn error_key(error: crate::compositor::dictation::runtime::RuntimeError) -> &'static str {
    use crate::compositor::dictation::runtime::RuntimeError as E;
    use agent_provider_config::speech::SpeechError as S;
    match error {
        E::Worker(S::ModelUnavailable | S::ModelReceipt) => "dictation-model-required",
        E::Worker(S::UnsupportedLanguage) => "dictation-language-unavailable",
        E::MicrophoneUnavailable => "dictation-microphone-unavailable",
        E::AudioDiscontinuity => "dictation-audio-interrupted",
        E::TimedOut => "dictation-timeout",
        _ => "dictation-unavailable",
    }
}
impl ShellLayer for DictationSurface {
    fn id(&self) -> LayerId {
        LayerId::Dictation
    }
    fn is_open(&self) -> bool {
        self.prompt.is_some()
    }
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        if matches!(event, WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ElementState::Pressed && key.logical_key == Key::Named(NamedKey::Escape))
        {
            self.request_cancel();
            cx.request(LayerIntent::Repaint);
            return Routed::Consumed;
        }
        let inside = cx
            .cursor
            .is_some_and(|point| Self::surface_rect(cx.geometry).contains(point));
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        );
        if !(pointer && inside
            || self.prompt.as_ref().is_some_and(Widget::is_focused)
                && matches!(event, WindowEvent::KeyboardInput { .. }))
        {
            return Routed::Pass;
        }
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.set_rect(Self::surface_rect(cx.geometry));
            prompt.handle_event(event, cx.event);
            if let Some(choice) = prompt.pending_choice.take() {
                self.choice(choice);
            }
        }
        cx.request(LayerIntent::Repaint);
        if pointer || *cx.event.handled {
            Routed::Consumed
        } else {
            Routed::Pass
        }
    }
    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        if let Some(prompt) = self.prompt.as_mut() {
            let rect = Self::surface_rect(cx.geometry);
            prompt.set_rect(rect);
            prompt.draw(
                cx.scene,
                cx.transform,
                cx.text_renderer,
                cx.theme,
                cx.presence,
            );
            if self.phase == Some(Phase::Consent) {
                crate::shell::trusted_ui::paint_seal(
                    cx.scene,
                    cx.transform,
                    rect,
                    cx.theme,
                    f64::from(cx.theme.tokens.radii.control),
                );
            }
        }
    }
    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        let mut node = self.prompt.as_ref()?.accessibility()?;
        node.live = Some(ui::a11y::Politeness::Polite);
        let single = node.children.len() == 1;
        if let Some(child) = node.children.get_mut(0) {
            child.action_target = Some(if single {
                self.cancel_target()
            } else {
                self.affirmative_target()
            });
        }
        if let Some(child) = node.children.get_mut(1) {
            child.action_target = Some(self.cancel_target());
        }
        Some(node)
    }
    fn accessibility_rect(&self, geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        let mut prompt = self.prompt.clone()?;
        prompt.set_rect(Self::surface_rect(geometry));
        if path.is_empty() {
            Some(prompt.get_rect())
        } else {
            prompt.synthetic_accessibility_rect(path)
        }
    }
    fn focus_accessibility(&mut self, path: &[usize]) -> bool {
        self.prompt
            .as_mut()
            .is_some_and(|prompt| prompt.focus_synthetic_accessibility(path))
    }
    fn semantic_intent(&self, target: &str) -> Option<LayerIntent> {
        let request = if target == self.cancel_target() {
            Request::Cancel(self.id)
        } else if target == self.affirmative_target() {
            match (self.phase, self.id) {
                (Some(Phase::Running(DictationStatus::Recovery { .. })), Some(id)) => {
                    Request::Copy(id)
                }
                (Some(Phase::Consent), Some(id)) => Request::Consent(id),
                (Some(Phase::Running(DictationStatus::Listening)), Some(id)) => Request::Finish(id),
                _ => Request::Cancel(self.id),
            }
        } else {
            return None;
        };
        Some(LayerIntent::Dictation(request))
    }
    fn dismiss(&mut self) -> bool {
        if self.prompt.is_none() {
            return false;
        }
        self.request_cancel();
        true
    }
    fn tick(&mut self, _now: Instant) -> bool {
        false
    }
}
