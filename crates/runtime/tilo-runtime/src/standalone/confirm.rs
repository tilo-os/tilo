//! The person's confirmation, at this device, of what an agent does for
//! them: the runtime's native prompt, answered for the agent's task.

use tilo_action_broker::app_actions::{
    AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use tokio::sync::oneshot;

/// Put to `person` in the runtime's native prompt what their agent asks to
/// confirm, through `broker`'s app actions; an app's name is read from `apps`.
pub(super) fn ask_the_person(
    broker: &tilo_action_broker::collab_broker::BrokerHandle,
    person: &str,
    apps: &std::sync::Arc<crate::apps::Apps>,
) {
    if let Some(actions) = broker.app_action_broker() {
        actions.set_app_action_consent(std::sync::Arc::new(ConfirmationPrompt {
            person: person.to_owned(),
            apps: std::sync::Arc::clone(apps),
        }));
    }
}

/// Longest label a prompt quotes.
const LABEL_CHARS: usize = 80;

/// Asks the runtime's person to let an agent work in an app for its task,
/// in the consent helper's native prompt (`--device-consent`, a process of
/// its own because the daemon has no main-thread run loop). Only a clear
/// Allow confirms: this invocation, and the agent's outside actions in that
/// app until its run ends. An action whose own contract asks for
/// confirmation still asks every time.
struct ConfirmationPrompt {
    /// The person this runtime serves; nobody else is asked.
    person: String,
    /// Where an app's name is read.
    apps: std::sync::Arc<crate::apps::Apps>,
}

impl AppActionConsentPrompter for ConfirmationPrompt {
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionConsent>> {
        // Only a confirmation of the person's own agent: a grant to another
        // application is not asked here, and so stays refused.
        if !request.confirming || request.person != self.person {
            return None;
        }
        let executable = std::env::current_exe().ok()?;
        let consent = consent(&request, &self.apps.display_name(&request.target_app));
        let (answer, answered) = oneshot::channel();
        std::thread::Builder::new()
            .name("tilo-confirm".into())
            .spawn(move || {
                let mut helper = std::process::Command::new(executable);
                helper.arg(crate::cli::CONSENT_HELPER_ARG);
                let allowed = tilo_dialogs::confined::ask_consent_in_helper(helper, &consent)
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "the confirmation prompt could not be shown");
                        false
                    });
                let _ = answer.send(if allowed {
                    AppActionConsent::Session
                } else {
                    AppActionConsent::Deny
                });
            })
            .ok()?;
        Some(answered)
    }
}

/// What the prompt says for `request` to an app named `app`, in the
/// person's language. Only words Tilo and the app declare: never the
/// resource's label, which can be text a web page wrote (a page's title),
/// put where it would argue for Allow.
fn consent(request: &AppActionConsentRequest, app: &str) -> tilo_dialogs::confined::Consent {
    let args = [
        (
            "action",
            ui::i18n::ArgValue::from(label(&request.action_label)),
        ),
        ("app", label(app).into()),
    ];
    tilo_dialogs::confined::Consent {
        message: crate::i18n::t_args("agent-confirm-question", &args),
        detail: crate::i18n::t_args("agent-confirm-detail", &args),
        allow: crate::i18n::t("agent-confirm-allow"),
        deny: crate::i18n::t("agent-confirm-deny"),
    }
}

/// `text` on one line, cut to [`LABEL_CHARS`].
fn label(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= LABEL_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(LABEL_CHARS).collect();
    cut.push('…');
    cut
}
