//! Account webhooks as a workspace holds them: each a saved shortcut whose one trigger is
//! [`TriggerSpec::Webhook`] and whose one step posts the event, owned by the key that registered
//! it. Each webhook is a resource of the provider ([`SavedWebhook::id`]), so a webhook's saved
//! grant reaches its own delivery alone. The workspace's governance actions register, list and
//! remove them; the device that keeps the workspace posts them (the app host's webhooks).

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::{ActionParam, ParamType};
use tilo_automation::{
    Binding, EVENT_INPUT, Selector, Shortcut, StepKind, TriggerSpec, WEBHOOK_POST,
    WEBHOOK_PROVIDER, WEBHOOK_RESOURCE,
};
use tilo_desktop_model::Workspace;

/// How long a URL has to answer one delivery.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// How often one delivery is posted before its invocation is failed.
pub const ATTEMPTS: u32 = 3;
/// The wait before each retry of a delivery.
pub const BACKOFF_MS: u64 = 50;

/// Why no delivery is posted to `url`, where none is: [`tilo_net::address::webhook_refusal`].
#[must_use]
pub fn refusal(url: &str) -> Option<&'static str> {
    tilo_net::address::webhook_refusal(url)
}

/// The provider these actions are published under.
pub const PROVIDER: &str = WEBHOOK_PROVIDER;

/// One webhook as its record holds it.
#[derive(Clone, Debug)]
pub struct SavedWebhook {
    pub id: String,
    pub workspace: String,
    pub url: String,
    pub events: Vec<String>,
    pub path: Option<String>,
    pub owner: String,
}

impl SavedWebhook {
    /// The row a key reads its webhook as.
    #[must_use]
    pub fn json(&self, undelivered: Option<&str>) -> Value {
        let mut row = serde_json::Map::new();
        row.insert("id".into(), json!(self.id));
        row.insert("workspace".into(), json!(self.workspace));
        row.insert("url".into(), json!(self.url));
        row.insert("events".into(), json!(self.events));
        if let Some(path) = &self.path {
            row.insert("path".into(), json!(path));
        }
        if let Some(why) = undelivered {
            row.insert("undelivered".into(), json!(why));
        }
        Value::Object(row)
    }
}

/// Every webhook saved in `workspace`, as its shortcuts carry them.
#[must_use]
pub fn saved(workspace: &Workspace, workspace_id: &str) -> Vec<SavedWebhook> {
    saved_in(&workspace.shortcuts, workspace_id)
}

/// [`saved`], read from `shortcuts` rather than a described workspace: what a device that keeps
/// the workspace reads without waiting on its lock.
#[must_use]
pub fn saved_in(
    shortcuts: &[tilo_automation::SavedShortcut],
    workspace_id: &str,
) -> Vec<SavedWebhook> {
    tilo_automation::decode_saved(shortcuts)
        .into_iter()
        .filter_map(|shortcut| {
            shortcut.triggers.iter().find_map(|trigger| {
                let TriggerSpec::Webhook {
                    url,
                    events,
                    path,
                    owner,
                    ..
                } = trigger
                else {
                    return None;
                };
                Some(SavedWebhook {
                    id: shortcut.id.clone(),
                    workspace: workspace_id.to_owned(),
                    url: url.clone(),
                    events: events.clone(),
                    path: path.clone(),
                    owner: owner.clone(),
                })
            })
        })
        .collect()
}

/// The shortcut that is webhook `id`, owned by `owner`: its one trigger posts `events` of the
/// workspace to `url`, and its one step is the delivery on the webhook's own resource (`id`),
/// which its grant reaches and no other webhook's does.
#[must_use]
pub fn shortcut(
    id: &str,
    url: &str,
    events: &[String],
    path: Option<&str>,
    owner: &str,
    since_ms: i64,
) -> Shortcut {
    let mut args = BTreeMap::new();
    args.insert("url".to_owned(), Binding::Literal(json!(url)));
    args.insert("webhook".to_owned(), Binding::Literal(json!(id)));
    args.insert("event".to_owned(), Binding::Input(EVENT_INPUT.to_owned()));
    args.insert("attempts".to_owned(), Binding::Literal(json!(ATTEMPTS)));
    if let Some(path) = path {
        args.insert("path".to_owned(), Binding::Literal(json!(path)));
    }
    let post = tilo_automation::Step::new(
        "post",
        StepKind::Invoke {
            action: tilo_actions::ActionKey::new(WEBHOOK_PROVIDER, WEBHOOK_RESOURCE, WEBHOOK_POST),
            // The delivery is this webhook's own resource's, so its saved grant reaches it and
            // no other webhook's.
            resource: Selector::Fixed(id.to_owned()),
            args,
            appearance: None,
        },
    );
    Shortcut {
        id: id.to_owned(),
        name: format!("Webhook {url}"),
        inputs: vec![ActionParam::required(
            EVENT_INPUT,
            ParamType::Object,
            "What fired the delivery.",
        )],
        steps: vec![post],
        step_timeout_secs: Some(ANSWER_WITHIN.as_secs()),
        output: None,
        // Registering the webhook is the person's consent to the one step it runs: an outside
        // effect posted from the device that keeps the workspace, to this webhook alone. Without
        // it the trigger fires and its run is refused, a delivery nothing posts.
        grant: tilo_automation::grant::rules(&[tilo_automation::grant::Permission {
            app_id: WEBHOOK_PROVIDER.to_owned(),
            reach: tilo_automation::grant::Reach::Resource(id.to_owned()),
            action: WEBHOOK_POST.to_owned(),
            kind: tilo_actions::ActionKind::Outside,
            label: format!("Post a webhook delivery in {WEBHOOK_RESOURCE}"),
        }]),
        triggers: vec![TriggerSpec::Webhook {
            url: url.to_owned(),
            events: events.to_vec(),
            path: path.map(str::to_owned),
            owner: owner.to_owned(),
            since_ms,
        }],
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shortcut a webhook is saved as grants the one step its trigger runs on that webhook's
    /// own resource: registering it is the person's consent to that delivery, and to no other
    /// webhook's in the workspace.
    #[test]
    fn a_webhooks_shortcut_grants_its_own_delivery_and_no_other() {
        let shortcut = shortcut(
            "wh_its_own",
            "http://127.0.0.1:9/hook",
            &["invocation".to_owned()],
            None,
            "owner",
            0,
        );
        let node = |webhook: &str| {
            tilo_automation::grant::step_node(
                None,
                WEBHOOK_PROVIDER,
                WEBHOOK_RESOURCE,
                &tilo_actions::api::ActionAddress::new(webhook, WEBHOOK_POST),
            )
        };
        let granted = tilo_automation::grant::grant_answer(
            &shortcut.grant,
            &node("wh_its_own"),
            tilo_actions::ActionKind::Outside,
        );
        assert_eq!(granted.answer, tilo_action_policy::Answer::Allow);
        assert!(granted.names_action, "the grant names the delivery itself");
        let other = tilo_automation::grant::grant_answer(
            &shortcut.grant,
            &node("wh_another"),
            tilo_actions::ActionKind::Outside,
        );
        assert_eq!(
            other.answer,
            tilo_action_policy::Answer::Deny,
            "one webhook's grant reaches no other webhook's delivery"
        );
    }
}
