//! The person's confirmation, at this device, of an action an agent makes
//! for them: the runtime's native prompt, answered for that one invocation.

use tilo_action_broker::app_actions::{
    AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};
use tokio::sync::oneshot;

/// Longest label a prompt quotes.
const LABEL_CHARS: usize = 80;

/// Asks the runtime's person to confirm an action an agent makes for them,
/// in the consent helper's native prompt (`--device-consent`, a process of
/// its own because the daemon has no main-thread run loop). Only a clear
/// Allow confirms, and only that one invocation: nothing is granted.
pub(super) struct ConfirmationPrompt {
    /// The person this runtime serves; nobody else is asked.
    pub(super) person: String,
    /// Where an app's name is read.
    pub(super) apps: std::sync::Arc<crate::apps::Apps>,
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
                    AppActionConsent::Once
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
