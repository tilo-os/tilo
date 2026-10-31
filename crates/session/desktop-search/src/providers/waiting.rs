//! What is waiting on the person: the live notifications they can answer
//! (a message to reply to, a call, a question an app asked), listed while
//! nothing is typed and found by what they say.
//!
//! Each is the resource the notifications service publishes for it
//! (`notification:<id>`), with the actions its banner offers. Choosing one
//! does what clicking its banner does, by address: its default action, or
//! a reply when that is all it takes.

use tilo_actions::ResourceSpec;
use ui::search::FieldWeight;
use ui::search::system::{ResourceKind, rank_fields};

use crate::model::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};

/// A notification's action carrying out one of its actions, its parameter,
/// and the key of the action clicking the banner runs.
const INVOKE: &str = "invoke";
const INVOKE_PARAM: &str = "action";
const DEFAULT_ACTION: &str = "default";
/// A notification's action replying to it.
const REPLY: &str = "reply";

/// The kind they are grouped under, in the desktop's language.
#[must_use]
pub fn waiting_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-waiting"))
}

/// The live notifications, newest last, as published.
pub struct Waiting {
    room: String,
    notifications: Vec<ResourceSpec>,
}

impl Waiting {
    /// The notifications `notifications` published by the session itself,
    /// in the workspace of `room`.
    #[must_use]
    pub fn new(room: impl Into<String>, notifications: Vec<ResourceSpec>) -> Self {
        Self {
            room: room.into(),
            notifications,
        }
    }

    /// What answering `notification` runs; `None` when it asks nothing of
    /// the person.
    fn answer(&self, notification: &ResourceSpec) -> Option<InvokeTarget> {
        let id = notification.id.as_str();
        if notification.actions.iter().any(|spec| spec.id == INVOKE) {
            return Some(
                InvokeTarget::action(DESKTOP_APP, &self.room, id, INVOKE)
                    .with_argument(INVOKE_PARAM, DEFAULT_ACTION),
            );
        }
        let reply = tilo_actions::door::survey(std::slice::from_ref(notification))
            .into_iter()
            .find(|action| action.address.action == REPLY)?;
        let mut target = InvokeTarget::action(DESKTOP_APP, &self.room, id, REPLY);
        target.descriptor = Some(reply);
        Some(target)
    }
}

impl Source for Waiting {
    fn id(&self) -> &'static str {
        "waiting-on-you"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-waiting")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let browsing = needle.trim().is_empty();
        self.notifications
            .iter()
            .enumerate()
            .filter_map(|(at, notification)| {
                let presentation = notification.presentation.as_ref();
                let from = presentation.and_then(|shown| shown.detail.clone());
                let body = presentation.and_then(|shown| shown.preview.clone());
                let score = if browsing {
                    // Newest first: they are published newest last.
                    i32::try_from(at.saturating_add(1)).unwrap_or(i32::MAX)
                } else {
                    rank_fields(
                        needle,
                        &[
                            (&notification.label, FieldWeight::TITLE),
                            (from.as_deref().unwrap_or_default(), FieldWeight::SUBTITLE),
                            (body.as_deref().unwrap_or_default(), FieldWeight::BODY),
                        ],
                    )?
                };
                let target = self.answer(notification)?;
                Some(Hit {
                    faces: Vec::new(),
                    outcome: None,
                    kind: waiting_kind(),
                    provider: self.id().into(),
                    id: notification.id.as_str().to_owned(),
                    title: notification.label.clone(),
                    subtitle: body,
                    facts: from.into_iter().collect(),
                    score,
                    icon: Some(ui::icons::Icon::Bell),
                    target: Target::Invoke(Box::new(target)),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ResourcePresentation;

    fn notification(id: &str, label: &str, actions: &[&str]) -> ResourceSpec {
        let mut resource = ResourceSpec::new(id, "session.notification", label).with_presentation(
            ResourcePresentation {
                detail: Some("Chat".into()),
                badge: None,
                preview: Some("Are you coming?".into()),
            },
        );
        for action in actions {
            resource = resource.with_action(tilo_actions::ActionSpec::immediate_encoded(
                *action,
                *action,
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.action".into(),
                    payload: Vec::new(),
                },
            ));
        }
        resource
    }

    #[test]
    fn what_asks_something_is_listed_newest_first_and_answered_by_address() {
        let waiting = Waiting::new(
            "room",
            vec![
                notification("notification:1", "Mara", &["close", "invoke"]),
                notification("notification:2", "Backup done", &["close"]),
                notification("notification:3", "Lea", &["close", "invoke"]),
            ],
        );
        let mut hits = waiting.query("");
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["Lea", "Mara"], "what asks nothing waits on no one");
        assert_eq!(hits[0].facts, ["Chat"]);
        let Target::Invoke(target) = &hits[0].target else {
            panic!("answered by address");
        };
        assert_eq!(target.app, DESKTOP_APP);
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("notification:3", INVOKE)
        );
        assert_eq!(target.arguments[INVOKE_PARAM], DEFAULT_ACTION);
        assert_eq!(waiting.query("coming").len(), 2, "found by what it says");
        assert!(waiting.query("zebra").is_empty());
    }
}
