//! The person's answer, at this device, to what an agent does for them or an
//! app asks of another app: the runtime's native prompt. An agent's action is
//! confirmed for that one call; an app's use of another app's action is
//! allowed in the workspace, as a grant the person can revoke.

use tilo_workspace_service::apps::app_actions::{
    APP_ACTION_ASK, AppActionAnswer, AppActionConsentPrompter, AppActionConsentRequest,
};
use tokio::sync::oneshot;

/// Put to `person` in the runtime's native prompt what their agent asks to
/// confirm, through `broker`'s app actions; an app's name is read from `apps`.
pub(super) fn ask_the_person(
    broker: &tilo_workspace_service::apps::collab_broker::BrokerHandle,
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

/// Asks the runtime's person, in the consent helper's native prompt
/// (`--device-consent`, a process of its own because the daemon has no
/// main-thread run loop), to confirm what an agent does for them, or to let
/// an app use another app's action. Only a clear Allow answers yes: for an
/// agent, this invocation alone; for an app, a grant in the workspace.
struct ConfirmationPrompt {
    /// The person this runtime serves; nobody else is asked.
    person: String,
    /// Where an app's name is read.
    apps: std::sync::Arc<crate::apps::Apps>,
}

impl AppActionConsentPrompter for ConfirmationPrompt {
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionAnswer>> {
        // Only the person's own: a confirmation of their agent, or a grant
        // an app asks for. Anything else stays refused.
        let granting = request.handle.starts_with(APP_ACTION_ASK);
        if !(request.confirming || granting) || request.person != self.person {
            return None;
        }
        let executable = std::env::current_exe().ok()?;
        let consent = consent(
            &request,
            &self.apps.display_name(&request.target_app),
            granting.then(|| self.apps.display_name(&request.app_id)),
        );
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
                // The native prompt shows the draft read-only: an answer here
                // changes nothing in it.
                let _ = answer.send(AppActionAnswer::unedited(allowed.then_some(if granting {
                    tilo_action_policy::Lifetime::UntilRevoked
                } else {
                    tilo_action_policy::Lifetime::Once
                })));
            })
            .ok()?;
        Some(answered)
    }
}

/// What the prompt says for `request` to an app named `app`, asked by the app
/// named `asker` when an app asks for a grant, in the person's language. Only
/// words Tilo and the apps declare: never the resource's label, which can be
/// text a web page wrote (a page's title), put where it would argue for Allow.
fn consent(
    request: &AppActionConsentRequest,
    app: &str,
    asker: Option<String>,
) -> tilo_dialogs::confined::Consent {
    let (question, detail) = if asker.is_some() {
        ("app-grant-question", "app-grant-detail")
    } else {
        ("agent-confirm-question", "agent-confirm-detail")
    };
    let args = [
        (
            "action",
            ui::i18n::ArgValue::from(label(&request.action_label)),
        ),
        ("app", label(app).into()),
        ("asker", label(&asker.unwrap_or_default()).into()),
    ];
    tilo_dialogs::confined::Consent {
        message: crate::i18n::t_args(question, &args),
        detail: crate::i18n::t_args(detail, &args),
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
