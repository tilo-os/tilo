//! Confirmations and questions from running shortcuts, answered in the
//! shell's modal sheet.
//!
//! An interactive run (from the palette or a keyboard trigger) hands every
//! step that needs its person to the broker's prompt queue. The host moves
//! each prompt onto the same sheet queue the portal's consent dialogs use,
//! so it looks, focuses and dismisses like them: a confirmation is a review
//! with Cancel and Confirm, a question offers its choices as rows or a text
//! field typed to the parameter, and a secret answer is masked on screen and
//! redacted from the run's report. Cancel declines the step, which stops the
//! run before anything after it is applied.

use std::fmt::Write as _;

use serde_json::Value;
use tilo_actions::contract::ConfirmationPolicy;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::Confirmation;

use super::ShellOverlayHost;
use crate::shell::automation_host::{PromptedStep, StepAnswer, StepPrompt};
use crate::shell::portal_dialog::{ShortcutStepDecision, ShortcutStepField, ShortcutStepRequest};

impl ShellOverlayHost {
    /// The sheet the shell is showing, if any: what a screen reader or a
    /// test reads to learn what is being asked.
    #[must_use]
    pub fn sheet_view(&self) -> Option<crate::shell::portal_dialog::DialogView> {
        self.portal_dialogs.view()
    }

    /// The sheet's text field as it is drawn: a secret is masked.
    #[must_use]
    pub fn sheet_field_display(&self) -> Option<String> {
        self.portal_dialogs.field_display()
    }

    /// Move every waiting shortcut prompt onto the sheet queue. Whether one
    /// was moved.
    pub fn pump_shortcut_step_prompts(&mut self) -> bool {
        let Some(broker) = self.shortcut_triggers.broker() else {
            return false;
        };
        let mut moved = false;
        while let Some(prompted) = broker.take_step_prompt() {
            let request = step_request(&prompted);
            let queue = self.portal_dialogs.queue().clone();
            if let Err(error) = queue.enqueue_shortcut_step(request, move |decision| {
                let answer = step_answer(&prompted.prompt, decision);
                prompted.answer(answer);
            }) {
                tracing::warn!(%error, "a shortcut step could not be shown");
            }
            moved = true;
        }
        if moved {
            self.pump_portal_dialogs();
        }
        moved
    }
}

/// The sheet showing `prompted`.
fn step_request(prompted: &PromptedStep) -> ShortcutStepRequest {
    let handle = format!("shortcut-step:{}", uuid::Uuid::new_v4());
    match &prompted.prompt {
        StepPrompt::Confirm(request) => {
            let mut detail = crate::shell::i18n::t_args(
                "shortcut-step-confirm-detail",
                &[
                    ("shortcut", prompted.shortcut.as_str().into()),
                    ("action", request.descriptor.action_label.as_str().into()),
                    (
                        "resource",
                        request.descriptor.resource_label.as_str().into(),
                    ),
                ],
            );
            for (name, value) in &request.arguments {
                let shown = value
                    .as_str()
                    .map_or_else(|| value.to_string(), ToString::to_string);
                let _ = write!(detail, "\n{name}: {shown}");
            }
            let field = match &request.policy {
                ConfirmationPolicy::Phrase { phrase } => {
                    detail.push('\n');
                    detail.push_str(&crate::shell::i18n::t_args(
                        "shortcut-step-type-phrase",
                        &[("phrase", phrase.as_str().into())],
                    ));
                    Some(ShortcutStepField { masked: false })
                }
                ConfirmationPolicy::Confirm | ConfirmationPolicy::None => None,
            };
            ShortcutStepRequest {
                handle,
                shortcut: prompted.shortcut.clone(),
                title: crate::shell::i18n::t_args(
                    "shortcut-step-confirm-title",
                    &[("action", request.descriptor.action_label.as_str().into())],
                ),
                detail,
                confirm_label: crate::shell::i18n::t("shortcut-step-confirm"),
                choices: Vec::new(),
                field,
            }
        }
        StepPrompt::Ask(request) => {
            let choices = choices_of(&request.param);
            ShortcutStepRequest {
                handle,
                shortcut: prompted.shortcut.clone(),
                title: if request.param.description.trim().is_empty() {
                    request.param.name.to_string()
                } else {
                    request.param.description.to_string()
                },
                detail: crate::shell::i18n::t_args(
                    "shortcut-step-ask-detail",
                    &[
                        ("shortcut", prompted.shortcut.as_str().into()),
                        ("param", request.param.name.to_string().into()),
                    ],
                ),
                confirm_label: crate::shell::i18n::t("shortcut-step-continue"),
                field: choices.is_empty().then_some(ShortcutStepField {
                    masked: request.param.ty == ParamType::Secret,
                }),
                choices,
            }
        }
    }
}

/// The rows a question offers instead of a text field.
fn choices_of(param: &ActionParam) -> Vec<String> {
    if !param.choices.is_empty() {
        return param.choices.iter().map(ToString::to_string).collect();
    }
    if param.ty == ParamType::Boolean {
        return vec![
            crate::shell::i18n::t("shortcut-step-yes"),
            crate::shell::i18n::t("shortcut-step-no"),
        ];
    }
    Vec::new()
}

/// What the sheet's `decision` answers to `prompt`.
fn step_answer(prompt: &StepPrompt, decision: ShortcutStepDecision) -> StepAnswer {
    let ShortcutStepDecision::Confirmed { choice, text } = decision else {
        return StepAnswer::Declined;
    };
    match prompt {
        StepPrompt::Confirm(request) => match &request.policy {
            ConfirmationPolicy::Phrase { .. } => StepAnswer::Confirmed(Confirmation::Phrase(text)),
            ConfirmationPolicy::Confirm | ConfirmationPolicy::None => {
                StepAnswer::Confirmed(Confirmation::Confirmed)
            }
        },
        StepPrompt::Ask(request) => {
            let param = &request.param;
            let choices = choices_of(param);
            if let Some(index) = choice
                && !choices.is_empty()
            {
                return match (param.ty, choices.get(index)) {
                    (ParamType::Boolean, Some(_)) if param.choices.is_empty() => {
                        StepAnswer::Answered(Value::Bool(index == 0))
                    }
                    (_, Some(label)) => StepAnswer::Answered(Value::String(label.clone())),
                    (_, None) => StepAnswer::Declined,
                };
            }
            StepAnswer::Answered(typed_value(param.ty, &text))
        }
    }
}

/// `text` as a value of type `ty`. Text that does not parse is passed on
/// as text, so the run refuses it by the parameter's own rule.
fn typed_value(ty: ParamType, text: &str) -> Value {
    let trimmed = text.trim();
    let parsed = match ty {
        ParamType::Integer => trimmed.parse::<i64>().ok().map(Value::from),
        ParamType::Number => trimmed.parse::<f64>().ok().map(Value::from),
        ParamType::Boolean => match trimmed.to_ascii_lowercase().as_str() {
            "yes" | "true" => Some(Value::Bool(true)),
            "no" | "false" => Some(Value::Bool(false)),
            _ => None,
        },
        ParamType::Object | ParamType::Array => serde_json::from_str(trimmed).ok(),
        ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret => None,
    };
    parsed.unwrap_or_else(|| Value::String(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_answer_is_parsed_to_its_parameter_type() {
        assert_eq!(typed_value(ParamType::Integer, " 42 "), Value::from(42));
        assert_eq!(typed_value(ParamType::Number, "1.5"), Value::from(1.5));
        assert_eq!(typed_value(ParamType::Boolean, "Yes"), Value::Bool(true));
        assert_eq!(
            typed_value(ParamType::Array, "[1, 2]"),
            serde_json::json!([1, 2])
        );
        assert_eq!(
            typed_value(ParamType::Integer, "many"),
            Value::String("many".into()),
            "left for the run to refuse by the parameter's rule"
        );
        assert_eq!(
            typed_value(ParamType::Secret, "hunter2"),
            Value::String("hunter2".into())
        );
    }
}
