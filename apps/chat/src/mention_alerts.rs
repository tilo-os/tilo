//! Telling this window's person when someone mentions them: a notification
//! through the host's notifications, for a mention that arrives while they
//! are not looking at its channel.
//!
//! What the window already held when it opened is not news; neither is a
//! mention in the channel the person is reading in a focused window, nor
//! one they wrote themselves.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::model::{Id, Message};

/// The notification scheme a mention's activation carries: clicking one names
/// the channel and the message it was about, so the window opens that message
/// rather than whatever channel happened to be in front.
pub(crate) const ACTIVATION_SCHEME: &str = "tilo-chat-mention";

/// The mentions this window has seen, and where it tells about new ones.
#[derive(Default)]
pub(crate) struct MentionAlerts {
    seen: HashSet<Id>,
    /// The first look took in what the window opened with.
    primed: bool,
    notifier: Option<Arc<dyn ui::notify::Notifier>>,
    /// Mentions the person clicked, waiting for the window's next update.
    opens: Arc<Mutex<Vec<(Id, Id)>>>,
    /// The activation handler is registered once per process.
    listening: bool,
}

/// One mention worth telling about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Alert {
    pub channel: Id,
    pub message: Id,
    pub title: String,
    pub body: String,
}

/// How old a mention may be and still be news, in seconds: older history
/// that loads later (scrolling back) is not.
const FRESH_SECS: i64 = 15 * 60;

/// Longest message excerpt a notification carries.
const EXCERPT_CHARS: usize = 180;

impl MentionAlerts {
    /// Take in every message of `channels` that mentions one of `me`;
    /// returns the new ones to tell about. `reading` is the channel in view
    /// while the window has focus; `now` is in seconds since the epoch.
    pub(crate) fn take_new(
        &mut self,
        channels: &[(Id, String, &[Message])],
        me: &[&str],
        (reading, now): (Option<&str>, i64),
        name_of: impl Fn(&str) -> String,
    ) -> Vec<Alert> {
        let primed = self.primed;
        self.primed = true;
        let mut alerts = Vec::new();
        for (channel, channel_name, messages) in channels {
            for message in *messages {
                let mentions_me = message
                    .mentions
                    .iter()
                    .any(|mention| me.contains(&mention.user.as_str()));
                if !mentions_me || me.contains(&message.author.as_str()) {
                    continue;
                }
                if !self.seen.insert(message.id.clone()) || !primed {
                    continue;
                }
                if reading == Some(channel.as_str()) || message.ts < now - FRESH_SECS {
                    continue;
                }
                let excerpt: String = message.text.trim().chars().take(EXCERPT_CHARS).collect();
                alerts.push(Alert {
                    channel: channel.clone(),
                    message: message.id.clone(),
                    title: crate::i18n::t_args(
                        "chat-mention-alert",
                        &[
                            ("name", name_of(&message.author).into()),
                            ("channel", channel_name.clone().into()),
                        ],
                    ),
                    body: excerpt,
                });
            }
        }
        alerts
    }

    /// Show `alerts` through the host's notifications, each one opening the
    /// message it is about when the person chooses it.
    pub(crate) fn show(&mut self, alerts: &[Alert]) {
        if alerts.is_empty() {
            return;
        }
        let notifier = self
            .notifier
            .get_or_insert_with(|| ui::host::current().notifications)
            .clone();
        if !self.listening {
            self.listening = true;
            let opens = Arc::clone(&self.opens);
            ui::notify::set_activation_handler(ACTIVATION_SCHEME, move |uri| {
                let Some(rest) = uri.strip_prefix(ACTIVATION_SCHEME) else {
                    return;
                };
                let Some(rest) = rest.strip_prefix(':') else {
                    return;
                };
                let Some((channel, message)) = rest.split_once('/') else {
                    return;
                };
                if let Ok(mut opens) = opens.lock() {
                    opens.push((channel.to_owned(), message.to_owned()));
                }
            });
        }
        for alert in alerts {
            let notification = ui::notify::Notification::new(
                format!("chat-mention-{}", alert.message),
                alert.title.clone(),
                alert.body.clone(),
            )
            .with_activation(format!(
                "{ACTIVATION_SCHEME}:{}/{}",
                alert.channel, alert.message
            ));
            // A host without notifications leaves the badge to tell.
            drop(notifier.show(&notification));
        }
    }

    /// The mention the person chose to open since the last call, if any.
    pub(crate) fn take_open(&self) -> Option<(Id, Id)> {
        self.opens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, author: &str, mentions: &[&str]) -> Message {
        Message {
            id: id.into(),
            channel: "c".into(),
            author: author.into(),
            ts: 0,
            text: "hey".into(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            reactions: Vec::new(),
            reference: None,
            source_provider: None,
            attachment: None,
            pinned: false,
            work: None,
            appearance: None,
            share: None,
            mentions: mentions
                .iter()
                .map(|user| crate::mention::Mention {
                    user: (*user).into(),
                    start: 0,
                    len: 1,
                    is_agent: false,
                })
                .collect(),
        }
    }

    #[test]
    fn only_a_new_mention_by_someone_else_away_from_view_is_told() {
        let mut alerts = MentionAlerts::default();
        let old = [message("m1", "bob", &["me"])];
        let channels = [("c".to_owned(), "general".to_owned(), &old[..])];
        assert!(
            alerts
                .take_new(&channels, &["me"], (None, 0), str::to_owned)
                .is_empty(),
            "what the window opened with is not news"
        );
        let new = [
            message("m1", "bob", &["me"]),
            message("m2", "bob", &["me"]),
            message("m3", "me", &["me"]),
            message("m4", "bob", &["ada"]),
        ];
        let channels = [("c".to_owned(), "general".to_owned(), &new[..])];
        let told = alerts.take_new(&channels, &["me"], (Some("other"), 0), str::to_owned);
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].message, "m2");
        let newer = [message("m5", "bob", &["me"])];
        let channels = [("c".to_owned(), "general".to_owned(), &newer[..])];
        assert!(
            alerts
                .take_new(&channels, &["me"], (Some("c"), 0), str::to_owned)
                .is_empty(),
            "a mention in the channel being read is seen already"
        );
    }
}
