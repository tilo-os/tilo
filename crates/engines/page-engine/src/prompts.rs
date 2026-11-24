//! What the page waits on a person's yes for, raised once and ended once.
//!
//! A site asking for a capability (camera, microphone, location, notifications), a download the
//! page offers, an extension asking for more access, a server's sign-in and a request kept from
//! leaving each wait in the engine until somebody answers, or until the engine stops waiting
//! (the page went elsewhere, its own deadline). The window raises each one as the app's call of
//! its own action, which the system keeps as an asked record for the person; this says when one
//! starts waiting and when it stops, so a call is raised once and withdrawn once.

use std::collections::BTreeMap;

use tilo_webview::WebViewEvent;

/// What kind of thing a page waits on, as its answer names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromptKind {
    Permission,
    Download,
    Extension,
    SignIn,
    Departure,
}

impl PromptKind {
    /// The kind's name on the wire to the app.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Download => "download",
            Self::Extension => "extension",
            Self::SignIn => "sign_in",
            Self::Departure => "departure",
        }
    }

    /// The kind `name` names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "permission" => Self::Permission,
            "download" => Self::Download,
            "extension" => Self::Extension,
            "sign_in" => Self::SignIn,
            "departure" => Self::Departure,
            _ => return None,
        })
    }
}

/// One thing the page waits on, by the engine's id of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PromptKey {
    pub kind: PromptKind,
    pub id: u32,
}

/// What the person reads of one thing the page waits on: a fact per label, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub key: PromptKey,
    /// What the answer is about, as the person reads it: the site, the file, the extension.
    pub on: String,
    /// The facts beside it, by their names on the wire (`origin`, `wants`, `name`, ...).
    pub facts: Vec<(&'static str, String)>,
}

/// A prompt that starts or stops waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptChange {
    Raised(Prompt),
    Ended(PromptKey),
}

/// The prompts a page waits on now, and what changed since the window last took them.
#[derive(Default)]
pub struct Prompts {
    open: BTreeMap<PromptKey, ()>,
    changes: Vec<PromptChange>,
}

impl Prompts {
    /// Take what `event` says of the prompts.
    pub fn apply(&mut self, event: &WebViewEvent) {
        match event {
            WebViewEvent::PermissionRequested {
                id,
                origin,
                permissions,
            } => self.raise(Prompt {
                key: key(PromptKind::Permission, *id),
                on: origin.clone(),
                facts: vec![("origin", origin.clone()), ("wants", permissions.clone())],
            }),
            WebViewEvent::PermissionClosed { id } => self.end(key(PromptKind::Permission, *id)),
            WebViewEvent::NativeDownloadRequested { id, url, name } => self.raise(Prompt {
                key: key(PromptKind::Download, *id),
                on: name.clone(),
                facts: vec![("name", name.clone()), ("url", url.clone())],
            }),
            // An update of a download ends its offer once the engine no longer waits on where
            // it goes: saved, discarded, or ended at its own deadline.
            WebViewEvent::DownloadUpdated { id, state, .. } if state != "awaiting" => {
                self.end(key(PromptKind::Download, *id));
            }
            WebViewEvent::ExtensionPermissionRequest { request, id, asked } => {
                let could = asked
                    .permissions
                    .iter()
                    .chain(&asked.origins)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                self.raise(Prompt {
                    key: key(PromptKind::Extension, *request),
                    on: id.clone(),
                    facts: vec![("extension", id.clone()), ("could", could)],
                });
            }
            WebViewEvent::ExtensionPermissionClosed { request } => {
                self.end(key(PromptKind::Extension, *request));
            }
            WebViewEvent::HttpAuth(challenge) => self.raise(Prompt {
                key: key(PromptKind::SignIn, challenge.id),
                on: challenge.site().to_owned(),
                facts: vec![
                    ("asker", challenge.asker.clone()),
                    ("site", challenge.site().to_owned()),
                    ("realm", challenge.realm.clone()),
                    ("proxy", challenge.proxy.to_string()),
                    ("retry", challenge.retry.to_string()),
                ],
            }),
            WebViewEvent::HttpAuthClosed { id } => self.end(key(PromptKind::SignIn, *id)),
            WebViewEvent::HeldRequest(departure) => self.raise(Prompt {
                key: key(PromptKind::Departure, departure.id),
                on: departure.destination().to_owned(),
                facts: vec![
                    ("origin", departure.origin.clone()),
                    ("method", departure.method.clone()),
                    ("leaving", departure.describing()),
                ],
            }),
            WebViewEvent::HeldRequestClosed { id } => self.end(key(PromptKind::Departure, *id)),
            _ => {}
        }
    }

    /// What changed since the last call, in the order it changed.
    pub fn take(&mut self) -> Vec<PromptChange> {
        std::mem::take(&mut self.changes)
    }

    /// Whether anything changed since the last [`Self::take`].
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.changes.is_empty()
    }

    fn raise(&mut self, prompt: Prompt) {
        // One request is raised once, however often the engine reports it.
        if self.open.insert(prompt.key, ()).is_none() {
            self.changes.push(PromptChange::Raised(prompt));
        }
    }

    fn end(&mut self, key: PromptKey) {
        if self.open.remove(&key).is_some() {
            self.changes.push(PromptChange::Ended(key));
        }
    }
}

const fn key(kind: PromptKind, id: u32) -> PromptKey {
    PromptKey { kind, id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_raised_once_and_ended_once() {
        let mut prompts = Prompts::default();
        let asked = WebViewEvent::PermissionRequested {
            id: 3,
            origin: "https://meet.example".into(),
            permissions: "camera".into(),
        };
        prompts.apply(&asked);
        prompts.apply(&asked);
        let changes = prompts.take();
        assert_eq!(changes.len(), 1, "raised once: {changes:?}");
        prompts.apply(&WebViewEvent::PermissionClosed { id: 3 });
        prompts.apply(&WebViewEvent::PermissionClosed { id: 3 });
        assert_eq!(
            prompts.take(),
            vec![PromptChange::Ended(key(PromptKind::Permission, 3))]
        );
    }
}
