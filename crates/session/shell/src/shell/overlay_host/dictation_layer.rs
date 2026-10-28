//! Local speech controls on the existing shell overlay stack. A request opens
//! one-shot consent; neither a shortcut nor an action grants microphone access.
use super::layers::{
    Anchor, LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Material, Modality, Role,
    Routed, ShellLayer, Surface,
};
use crate::compositor::dictation_status::{DictationError, DictationStatus, SessionId};
use crate::shell::i18n::{t, t_args};
use ui::a11y::AccessNode;
use ui::widget::{Container, PermissionPrompt, PromptChoice, Text, Widget};
use vello::kurbo::Rect;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::Zeroize as _;

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
    // Shared Text has no Debug/serialization surface; words stay in this
    // local overlay, never in the permission prompt's diagnostic fields.
    preview: Option<Container>,
    preview_text: zeroize::Zeroizing<String>,
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
    /// Receipt-only fields serve speech preparation/recording, never ordinary
    /// IME composition. Recovery does not reopen the original editable field.
    pub(crate) fn needs_original_field(&self) -> bool {
        self.pending == Some(Request::Toggle)
            || matches!(self.phase, Some(Phase::Preparing | Phase::Consent))
            || matches!(
                self.phase,
                Some(Phase::Running(
                    DictationStatus::Loading
                        | DictationStatus::Listening
                        | DictationStatus::StoppingRecording
                        | DictationStatus::Finishing
                ))
            )
    }

    pub(crate) fn pending_consent(&self) -> bool {
        matches!(self.pending, Some(Request::Consent(id)) if Some(id) == self.id && self.is_consent())
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
    pub(crate) fn is_model_setup(&self) -> bool {
        self.phase == Some(Phase::Failure("dictation-model-required"))
    }
    pub(crate) fn preview(&mut self, transcript: Option<&str>) {
        let active = matches!(
            self.phase,
            Some(Phase::Running(
                DictationStatus::Listening
                    | DictationStatus::Finishing
                    | DictationStatus::StoppingRecording
            ))
        );
        let transcript = transcript.filter(|text| active && !text.is_empty());
        let Some(transcript) = transcript else {
            self.changed |= self.preview.take().is_some();
            self.preview_text.zeroize();
            return;
        };
        // Bound glyph work as well as transcript storage. The complete draft
        // remains in the private session; show only the last 160 scalars.
        let start = transcript
            .char_indices()
            .rev()
            .nth(159)
            .map_or(0, |(offset, _)| offset);
        let excerpt = transcript.get(start..).unwrap_or_default();
        if self.preview_text.as_str() == excerpt {
            return;
        }
        self.preview_text.zeroize();
        self.preview_text.push_str(excerpt);
        let label = t_args("dictation-live-preview", &[("text", excerpt.into())]);
        self.preview = Some(
            Container::new().with_background().with_child(Box::new(
                Text::new(&label)
                    .with_id("dictation.live-preview")
                    .with_role(ui::tokens::TextRole::Caption)
                    .with_live(ui::a11y::Politeness::Polite),
            )),
        );
        self.changed = true;
    }
    pub(crate) fn preview_target(&self) -> Option<Rect> {
        let rect = self.preview.as_ref()?.get_rect();
        (rect.area() > 0.0).then_some(rect)
    }
    fn preview_rect(&self, geometry: LayerGeometry<'_>) -> Rect {
        let prompt = self.surface_rect(geometry);
        let spacing = ui::tokens::Spacing::BASE;
        Rect::new(
            prompt.x0,
            prompt.y1,
            prompt.x1,
            prompt.y1
                + self.preview.as_ref().map_or_else(
                    || f64::from(spacing.s8 + spacing.s8),
                    |preview| {
                        preview
                            .get_rect()
                            .height()
                            .max(f64::from(spacing.s8 + spacing.s8))
                    },
                ),
        )
    }
    pub(crate) fn contains(&self, geometry: LayerGeometry<'_>, point: vello::kurbo::Point) -> bool {
        self.prompt.is_some()
            && (self.surface_rect(geometry).contains(point)
                || self.preview.is_some() && self.preview_rect(geometry).contains(point))
    }
    pub(crate) fn surface_rect(&self, geometry: LayerGeometry<'_>) -> Rect {
        let area = geometry.display_for(None);
        let spacing = ui::tokens::Spacing::BASE;
        let pad = f64::from(spacing.s4);
        Rect::new(
            area.x0 + pad,
            area.y0 + pad,
            area.x1 - pad,
            area.y0
                + pad
                + self.prompt.as_ref().map_or(
                    ui::widget::permission_prompt::PROMPT_HEIGHT,
                    |prompt| {
                        prompt
                            .get_rect()
                            .height()
                            .max(ui::widget::permission_prompt::PROMPT_HEIGHT)
                    },
                ),
        )
    }
    fn show(&mut self, phase: Phase, message: &'static str) {
        if self.phase == Some(phase) {
            return;
        }
        let labels = if phase == Phase::Failure("dictation-model-required") {
            ["dictation-open-settings", "dictation-cancel"]
        } else if phase == Phase::Consent {
            ["dictation-record-once", "dictation-cancel"]
        } else if matches!(phase, Phase::Running(DictationStatus::Recovery { .. })) {
            ["dictation-copy-transcript", "dictation-discard-transcript"]
        } else if phase == Phase::Running(DictationStatus::Listening) {
            ["dictation-finish", "dictation-cancel"]
        } else {
            ["dictation-cancel", "dictation-cancel"]
        };
        let icon = if phase == Phase::Running(DictationStatus::Listening) {
            ui::icons::Icon::Mic
        } else {
            ui::icons::Icon::MicOff
        };
        let mut prompt = PermissionPrompt::for_local_input(
            t("dictation-title"),
            t(message),
            labels,
            phase == Phase::Consent,
        )
        .with_icon(icon);
        prompt.set_focus(phase == Phase::Consent);
        self.changed = true;
        self.prompt = Some(prompt);
        self.phase = Some(phase);
    }
    pub(crate) fn prepare(&mut self, id: SessionId) {
        self.preview(None);
        self.id = Some(id);
        self.show(Phase::Preparing, "dictation-preparing");
    }
    pub(crate) fn consent(&mut self) {
        self.show(Phase::Consent, "dictation-consent");
    }
    pub(crate) fn status(&mut self, status: DictationStatus) {
        if !matches!(
            status,
            DictationStatus::Listening
                | DictationStatus::Finishing
                | DictationStatus::StoppingRecording
        ) {
            self.preview(None);
        }
        let message = match status {
            DictationStatus::Idle => {
                self.close();
                return;
            }
            DictationStatus::Recovery {
                reason,
                copied,
                copy_failed,
            } => {
                if copy_failed {
                    "dictation-copy-failed"
                } else if copied {
                    "dictation-transcript-copied"
                } else {
                    match reason {
                        crate::compositor::dictation_status::RecoveryReason::VocabularyChanged => {
                            "dictation-vocabulary-draft-retained"
                        }
                        crate::compositor::dictation_status::RecoveryReason::TooLong => {
                            "dictation-transcript-retained"
                        }
                        crate::compositor::dictation_status::RecoveryReason::Rejected => {
                            "dictation-edit-rejected"
                        }
                        crate::compositor::dictation_status::RecoveryReason::UnknownDelivery => {
                            "dictation-delivery-unknown"
                        }
                    }
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
        self.preview(None);
        let key = match error {
            DictationError::NoEditableField | DictationError::SensitiveField => {
                "dictation-editable-required"
            }
            DictationError::UnsupportedField => "dictation-unsupported-field",
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
            } else if self.is_model_setup() {
                "open-settings"
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
        self.preview = None;
        self.preview_text.zeroize();
    }
    fn choice(&mut self, choice: PromptChoice) -> Option<LayerIntent> {
        if self.is_model_setup() && choice == PromptChoice::Allow {
            return Some(LayerIntent::DictationSetup);
        }
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
        None
    }
}

fn error_key(error: crate::compositor::dictation_status::RuntimeError) -> &'static str {
    use crate::compositor::dictation_status::RuntimeError as E;
    use agent_provider_config::speech::SpeechError as S;
    match error {
        E::Worker(S::ModelUnavailable | S::ModelReceipt) => "dictation-model-required",
        E::Worker(S::UnsupportedLanguage) => "dictation-language-unavailable",
        E::MicrophoneUnavailable => "dictation-microphone-unavailable",
        E::VocabularyUnavailable => "dictation-vocabulary-unavailable",
        E::VocabularyChanged => "dictation-vocabulary-changed",
        E::AudioDiscontinuity => "dictation-audio-interrupted",
        E::TimedOut => "dictation-timeout",
        _ => "dictation-unavailable",
    }
}
impl ShellLayer for DictationSurface {
    fn id(&self) -> LayerId {
        LayerId::Dictation
    }

    /// Microphone consent and status under the caret it dictates into,
    /// window-modal while it is open: the field it dictates into keeps its
    /// focus, and nothing else on the desktop may be worked meanwhile.
    fn surface(&self) -> Option<Surface> {
        Some(Surface::shell(
            Role::Overlay,
            Anchor::BottomCenter(None),
            Modality::WindowModal,
            Material::SeeThrough,
            self.prompt.is_some(),
        ))
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
            .is_some_and(|point| self.contains(cx.geometry, point));
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
        let rect = self.surface_rect(cx.geometry);
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.set_rect(rect);
            prompt.handle_event(event, cx.event);
            if let Some(choice) = prompt.pending_choice.take()
                && let Some(intent) = self.choice(choice)
            {
                cx.request(intent);
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
        let rect = self.surface_rect(cx.geometry);
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.set_rect(rect);
            let rect = Rect::new(
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y0 + prompt.preferred_height(cx.text_renderer, cx.theme),
            );
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
        let rect = self.preview_rect(cx.geometry);
        if let Some(preview) = self.preview.as_mut() {
            if layout_preview(preview, rect, cx.text_renderer, cx.theme).is_err() {
                tracing::warn!("dictation caption layout unavailable");
                return;
            }
            preview.draw(
                cx.scene,
                cx.transform,
                cx.text_renderer,
                cx.theme,
                cx.presence,
            );
            preview.mark_drawn();
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
        if let Some(preview) = self.preview.as_ref() {
            preview.for_each_child(&mut |child| {
                if let Some(caption) = child.accessibility() {
                    node.children.push(caption);
                }
            });
        }
        Some(node)
    }
    fn accessibility_rect(&self, geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        let choices = self.prompt.as_ref()?.accessibility()?.children.len();
        if self.preview.is_some() && path == [choices] {
            return Some(self.preview_rect(geometry));
        }
        let mut prompt = self.prompt.clone()?;
        prompt.set_rect(self.surface_rect(geometry));
        if path.is_empty() {
            Some(if self.preview.is_some() {
                prompt.get_rect().union(self.preview_rect(geometry))
            } else {
                prompt.get_rect()
            })
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
        if self.is_model_setup() && target == self.affirmative_target() {
            return Some(LayerIntent::DictationSetup);
        }
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
}

/// A shared Container owns the caption's surface and themed inset; shared Text
/// owns shaping and wrapping. This tiny local layout follows the normal widget
/// passes and gives the complete bounded caption its natural height.
fn layout_preview(
    preview: &mut Container,
    rect: Rect,
    text: &mut ui::text_render::TextRenderer,
    theme: &ui::Theme,
) -> Result<(), taffy::TaffyError> {
    let mut tree = taffy::TaffyTree::<()>::new();
    let mut nodes = std::collections::HashMap::new();
    let root = preview.build_layout(&mut tree, theme, &mut nodes);
    let mut style = tree.style(root)?.clone();
    style.min_size.height =
        taffy::Dimension::length(theme.tokens.spacing.s8 + theme.tokens.spacing.s8);
    tree.set_style(root, style)?;
    tree.compute_layout_with_measure(
        root,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(ui::num::f32_of_f64(rect.width())),
            height: taffy::AvailableSpace::MaxContent,
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(taffy::Size::ZERO, |&widget| {
                // SAFETY: both nodes point into this exclusively borrowed
                // Container; no widget moves or receives events during layout.
                unsafe { Widget::measure(&mut *widget, known, available, text) }
            })
        },
    )?;
    preview.apply_layout(&tree, root, vello::kurbo::Point::new(rect.x0, rect.y0));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_streaming_preview_is_bounded_accessible_and_cleared_on_retirement() {
        let mut surface = DictationSurface::default();
        surface.status(DictationStatus::Listening);
        let finish = surface.affirmative_target();
        let draft = "Private public fixture 👋 日本語 ".repeat(200);
        surface.preview(Some(&draft));
        assert!(surface.preview_target().is_none());
        assert_eq!(surface.preview_text.chars().count(), 160);
        let geometry = LayerGeometry {
            viewport: Rect::new(0.0, 0.0, 1280.0, 800.0),
            outputs: &[],
            focused_output: None,
        };
        let node = surface.accessibility(geometry).unwrap();
        let caption_rect = surface.preview_rect(geometry);
        assert!(surface.contains(geometry, caption_rect.center()));
        assert!(
            surface
                .accessibility_rect(geometry, &[])
                .unwrap()
                .contains(caption_rect.center())
        );
        assert_eq!(node.children.len(), 3);
        assert_eq!(
            node.children.first().unwrap().action_target.as_deref(),
            Some(finish.as_str())
        );
        assert!(
            node.children
                .last()
                .unwrap()
                .name
                .as_deref()
                .unwrap()
                .contains("日本語")
        );
        assert_eq!(
            node.children.last().unwrap().name.as_deref(),
            Some(
                t_args(
                    "dictation-live-preview",
                    &[("text", surface.preview_text.as_str().into())]
                )
                .as_str()
            ),
            "the accessible preview must carry the same uncertainty label as the visible text"
        );
        assert!(
            !format!("{:?}", surface.prompt.as_ref().unwrap()).contains("Private public fixture")
        );
        let mut text = ui::text_render::TextRenderer::new();
        for theme in [
            ui::Theme::light(),
            ui::Theme::high_contrast().with_text_scale(1.3),
        ] {
            let preview = surface.preview.as_mut().unwrap();
            layout_preview(
                preview,
                Rect::new(16.0, 96.0, 784.0, 160.0),
                &mut text,
                &theme,
            )
            .unwrap();
            let panel = preview.get_rect();
            assert!(panel.height() >= 64.0);
            preview.for_each_child(&mut |child| {
                assert!(panel.contains(child.get_rect().origin()));
                assert!(child.get_rect().x1 <= panel.x1);
                assert!(child.get_rect().y1 <= panel.y1);
            });
            assert_eq!(surface.preview_target(), Some(panel));
        }
        assert!(matches!(
            surface.semantic_intent(&finish),
            Some(LayerIntent::Dictation(_))
        ));
        surface.status(DictationStatus::Recovery {
            reason: crate::compositor::dictation_status::RecoveryReason::TooLong,
            copied: false,
            copy_failed: false,
        });
        assert!(surface.preview.is_none() && surface.preview_text.is_empty());
        assert!(surface.preview_target().is_none());
        surface.status(DictationStatus::Idle);
        surface.preview(Some(&draft));
        assert!(surface.preview.is_none() && surface.accessibility(geometry).is_none());
        assert!(!surface.contains(geometry, caption_rect.center()));
    }
}
