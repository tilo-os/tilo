//! What the window's page waits on the person's yes for, raised as the app's calls of its own
//! actions.
//!
//! The window reads the engine's events and tells the app each prompt that starts or stops
//! waiting (`ToApp::PagePrompt`): a site asking for the camera, a download the page offers, an
//! extension asking for more access, a server's sign-in, a request kept from leaving. Each one
//! raised is the app's call of its own page action (`allow_permission`, `save_download`,
//! `allow_extension_access`, `sign_in`, `let_leave`) on the page, through the dispatch. The
//! action asks the person first, so the dispatch keeps the call as its asked record, naming the
//! app and the site; on their approve the call comes back to the app and its action answers the
//! page. A call that ends without running (declined, refused, stopped) is the page's no, told
//! here. A prompt the page stops waiting on withdraws its call, which moves its record to
//! cancelled. The app keeps no question of its own: the record is the question, and the system
//! shows it.

use std::collections::BTreeMap;

use collaboration::CollabRequest;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
};

/// One prompt the page waits on, by its kind and the engine's id of it.
type Key = (String, u32);

/// A call raised for a prompt: its id, and what tells the page no.
struct Raised {
    invocation: InvocationId,
    no: String,
}

/// What the dispatch's answer of a call raised for a prompt means for the page.
#[derive(Debug, PartialEq, Eq)]
pub enum Answered {
    /// The call goes on, waiting for the person or running: follow it by its invocation until
    /// it moves, since nothing tells the app otherwise.
    Goes(InvocationId),
    /// Nothing more to tell the page: its action ran and answered it, or the call cannot move.
    Said,
    /// The call ended without running: the page request that tells the page no.
    No(String),
}

/// The calls raised for what the window's page waits on.
#[derive(Default)]
pub struct PagePrompts {
    raised: BTreeMap<Key, Raised>,
}

/// What a prompt asks the app to do: its action, the arguments it shows, and the page request
/// that tells the page no.
fn call_of(kind: &str, id: u32, facts: &serde_json::Value) -> Option<(&'static str, String)> {
    let fact = |name: &str| {
        facts
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let no = |op: &str| serde_json::json!({"op": op, "id": id, "allow": false}).to_string();
    Some(match kind {
        "permission" => ("allow_permission", no("answer_permission")),
        "download" => ("save_download", no("answer_download")),
        "extension" => ("allow_extension_access", no("answer_extension")),
        // A sign-in turned down names no credentials: the server shows its own page.
        "sign_in" => (
            "sign_in",
            serde_json::json!({"op": "answer_sign_in", "id": id, "asker": fact("asker")})
                .to_string(),
        ),
        "departure" => ("let_leave", no("answer_held_request")),
        _ => return None,
    })
}

/// What the page said of itself (a download the person saved, the pages it can go to), raised as the
/// app's call of its own action: the kind names the action and the note's other fields are its
/// arguments. `true` when it said anything this reads.
fn note(
    kind: &str,
    said: &serde_json::Value,
    (workspace, app, page): (&str, &str, Option<&str>),
    mut send: impl FnMut(CollabRequest) -> bool,
) -> bool {
    let action = match kind {
        "download" => "download",
        "extension_download" => "note_extension_download",
        "history" => "remember_history",
        "open" => "open_from_page",
        "credential" => "remember_credential",
        "extensions" => "remember_extensions",
        "fullscreen" => "remember_fullscreen",
        "extension" => "note_extension",
        // The page's video appeared or went: the app reads it again, so what it offers to float
        // follows the page.
        "media" => "refresh_page",
        "extension_updates" => "note_extension_updates",

        "print" => "note_print",
        "panel" => "remember_panel",
        "omnibox" => "remember_omnibox",
        _ => {
            tracing::warn!(kind, "a note the app has no page action for");
            return false;
        }
    };
    let (Some(page), serde_json::Value::Object(fields)) = (page, said) else {
        return false;
    };
    let arguments: serde_json::Map<String, serde_json::Value> = fields
        .iter()
        .filter(|(name, _)| name.as_str() != "note")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::application_action_target(workspace, app, page, action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let message = RemoteActionMessage::Invoke(request);
    if send(CollabRequest::RemoteAction {
        message,
        press: ui_core::press::current(),
    }) {
        true
    } else {
        tracing::warn!(kind, "a note's call did not reach the dispatch");
        false
    }
}

impl PagePrompts {
    /// Take what the window said of a prompt (`prompt`, as the page engine encodes it): one
    /// raised becomes the app's call of its own action on `page` in `workspace`, sent with
    /// `send`; one ended withdraws that call. Whether it said anything this reads.
    pub fn take(
        &mut self,
        prompt: &[u8],
        (workspace, app, page): (&str, &str, Option<&str>),
        mut send: impl FnMut(CollabRequest) -> bool,
    ) -> bool {
        let Ok(said) = serde_json::from_slice::<serde_json::Value>(prompt) else {
            tracing::warn!("the window said of a prompt what is no JSON");
            return false;
        };
        let id = said
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or_default();
        // What the page said of itself is the app's record's: its call runs at once, with no
        // question, and nothing waits to be withdrawn.
        if let Some(kind) = said.get("note").and_then(serde_json::Value::as_str) {
            return note(kind, &said, (workspace, app, page), send);
        }
        if let Some(kind) = said.get("ended").and_then(serde_json::Value::as_str) {
            // The page no longer waits: the person is asked no more.
            if let Some(raised) = self.raised.remove(&(kind.to_owned(), id)) {
                let message = RemoteActionMessage::Cancel {
                    invocation: raised.invocation,
                };
                if !send(CollabRequest::RemoteAction {
                    message,
                    press: None,
                }) {
                    tracing::warn!(kind, id, "a prompt's call was not withdrawn");
                }
            }
            return true;
        }
        let Some(kind) = said.get("raised").and_then(serde_json::Value::as_str) else {
            return false;
        };
        let facts = said.get("facts").cloned().unwrap_or_default();
        let (Some((action, no)), Some(page)) = (call_of(kind, id, &facts), page) else {
            tracing::warn!(kind, "a prompt the app has no page action for");
            return false;
        };
        let key = (kind.to_owned(), id);
        if self.raised.contains_key(&key) {
            return true;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("id".to_owned(), id.into());
        if let serde_json::Value::Object(facts) = facts {
            arguments.extend(facts);
        }
        let invocation = InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target: tilo_actions::remote::application_action_target(workspace, app, page, action),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let message = RemoteActionMessage::Invoke(request);
        if send(CollabRequest::RemoteAction {
            message,
            press: ui_core::press::current(),
        }) {
            self.raised.insert(key, Raised { invocation, no });
        } else {
            tracing::warn!(kind, id, "a prompt's call did not reach the dispatch");
        }
        true
    }

    /// What the dispatch answered of a call raised here: one that ended without running tells
    /// the page no, answered with the page request that says it. One that goes on is followed
    /// until it moves; one that ran (its action answered the page) says nothing more. `None` for
    /// a message of no call raised here.
    pub fn answered(&mut self, message: &RemoteActionMessage) -> Option<Answered> {
        let receipt = match message {
            RemoteActionMessage::Receipt(receipt) => receipt.clone(),
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => state.receipt(),
            _ => return None,
        };
        let key = self
            .raised
            .iter()
            .find(|(_, raised)| raised.invocation == receipt.invocation)
            .map(|(key, _)| key.clone())?;
        match &receipt.status {
            // Kept for the person in its asked record, or running: it moves later.
            ReceiptStatus::Accepted => Some(Answered::Goes(receipt.invocation)),
            // Refused for want of a yes where no record keeps it: nothing here moves it.
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired(_),
            } => Some(Answered::Said),
            ReceiptStatus::Completed { .. } => {
                self.raised.remove(&key);
                Some(Answered::Said)
            }
            ReceiptStatus::Rejected { .. }
            | ReceiptStatus::Failed { .. }
            | ReceiptStatus::Cancelled { .. } => self
                .raised
                .remove(&key)
                .map(|raised| Answered::No(raised.no)),
        }
    }

    /// The page went away with its window: every call raised for it is withdrawn.
    pub fn withdraw_all(&mut self, mut send: impl FnMut(CollabRequest) -> bool) {
        for (_, raised) in std::mem::take(&mut self.raised) {
            let message = RemoteActionMessage::Cancel {
                invocation: raised.invocation,
            };
            if !send(CollabRequest::RemoteAction {
                message,
                press: None,
            }) {
                tracing::warn!("a closed page's call was not withdrawn");
            }
        }
    }

    /// The calls that wait, by the invocation each went out as.
    #[cfg(test)]
    fn waiting(&self) -> Vec<InvocationId> {
        self.raised
            .values()
            .map(|raised| raised.invocation)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "tl:so.tilo.browser:page-1";

    fn raise(prompts: &mut PagePrompts, prompt: &serde_json::Value) -> Vec<CollabRequest> {
        let mut sent = Vec::new();
        prompts.take(
            prompt.to_string().as_bytes(),
            ("w1", "so.tilo.browser", Some(PAGE)),
            |request| {
                sent.push(request);
                true
            },
        );
        sent
    }

    fn camera() -> serde_json::Value {
        serde_json::json!({"raised": "permission", "id": 7, "on": "https://meet.example",
            "facts": {"origin": "https://meet.example", "wants": "camera"}})
    }

    /// A site asking for the camera is the app's own `allow_permission` on the page, naming the
    /// site, raised once however often the window says it.
    #[test]
    fn a_camera_request_is_the_apps_call_naming_the_site() {
        // The person's press in the window, which the page's call names.
        ui_core::press::note(7);
        let mut prompts = PagePrompts::default();
        let sent = raise(&mut prompts, &camera());
        assert!(raise(&mut prompts, &camera()).is_empty(), "raised once");
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                press,
            },
        ] = sent.as_slice()
        else {
            panic!("one call: {sent:?}");
        };
        // The page host names the press the window was handling: what lets the call run as the
        // person.
        assert_eq!(*press, Some(7));
        assert_eq!(request.target.address.resource, PAGE);
        assert_eq!(request.target.address.action, "allow_permission");
        let InvokeArgument::Json(arguments) = &request.argument else {
            panic!("arguments as JSON");
        };
        assert_eq!(arguments["origin"], "https://meet.example");
        assert_eq!(arguments["wants"], "camera");
        assert_eq!(arguments["id"], 7);
    }

    /// A call kept for the person is followed while it waits; once they decline it, it ends
    /// cancelled and the page is told no, once.
    #[test]
    fn a_declined_call_tells_the_page_no() {
        let mut prompts = PagePrompts::default();
        raise(&mut prompts, &camera());
        let invocation = prompts.waiting()[0];
        let waits = RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        });
        assert_eq!(
            prompts.answered(&waits),
            Some(Answered::Goes(invocation)),
            "it waits for the person, followed"
        );
        let declined = RemoteActionMessage::Update {
            invocation,
            state: Some(tilo_actions::remote::InvocationState::new(
                invocation,
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
                },
            )),
            correlation: None,
        };
        let Some(Answered::No(no)) = prompts.answered(&declined) else {
            panic!("the page's no");
        };
        let no: serde_json::Value = serde_json::from_str(&no).unwrap();
        assert_eq!(no["op"], "answer_permission");
        assert_eq!(no["allow"], false);
        assert_eq!(prompts.answered(&declined), None, "told once");
    }

    /// What the page says of itself is the app's own action on it, run at once with the note's
    /// fields as its arguments.
    #[test]
    fn a_saved_download_is_the_apps_own_download_from_the_page() {
        let mut prompts = PagePrompts::default();
        let note = serde_json::json!({"note": "download", "name": "report.pdf",
            "url": "https://example.org/report.pdf", "request": "0f3c"});
        let mut sent = Vec::new();
        assert!(prompts.take(
            note.to_string().as_bytes(),
            ("w1", "so.tilo.browser", Some(PAGE)),
            |request| {
                sent.push(request);
                true
            },
        ));
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one call: {sent:?}");
        };
        assert_eq!(request.target.address.resource, PAGE);
        assert_eq!(request.target.address.action, "download");
        assert!(request.confirmation.is_none(), "a note is no question");
        let InvokeArgument::Json(arguments) = &request.argument else {
            panic!("arguments as JSON");
        };
        assert_eq!(arguments["url"], "https://example.org/report.pdf");
        assert_eq!(arguments["name"], "report.pdf");
        assert_eq!(
            arguments["request"], "0f3c",
            "the call names the request's handle only"
        );
        assert!(arguments.get("cookies").is_none());
        assert!(
            arguments.get("note").is_none(),
            "the kind is the action, not an argument"
        );
    }

    /// The page's video appeared or went: the app reads the page again, which is where it keeps
    /// what it offers to float.
    #[test]
    fn a_page_video_appearing_or_going_is_the_apps_read_of_the_page() {
        let mut prompts = PagePrompts::default();
        let mut sent = Vec::new();
        assert!(prompts.take(
            br#"{"note": "media"}"#,
            ("w1", "so.tilo.browser", Some(PAGE)),
            |request| {
                sent.push(request);
                true
            },
        ));
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one call: {sent:?}");
        };
        assert_eq!(request.target.address.resource, PAGE);
        assert_eq!(request.target.address.action, "refresh_page");
        let InvokeArgument::Json(arguments) = &request.argument else {
            panic!("arguments as JSON");
        };
        assert!(
            arguments.get("note").is_none(),
            "the kind is the action, not an argument"
        );
    }

    /// A prompt the page stops waiting on, or a page that closes, withdraws its call.
    #[test]
    fn a_prompt_the_page_no_longer_waits_on_withdraws_its_call() {
        let mut prompts = PagePrompts::default();
        raise(&mut prompts, &camera());
        let invocation = prompts.waiting()[0];
        let sent = raise(
            &mut prompts,
            &serde_json::json!({"ended": "permission", "id": 7}),
        );
        assert!(
            matches!(
                sent.as_slice(),
                [CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Cancel { invocation: stopped },
                    ..
                }] if *stopped == invocation
            ),
            "{sent:?}"
        );
        raise(&mut prompts, &camera());
        let mut withdrawn = Vec::new();
        prompts.withdraw_all(|request| {
            withdrawn.push(request);
            true
        });
        assert_eq!(withdrawn.len(), 1, "the closed page's call is withdrawn");
        assert!(prompts.waiting().is_empty());
    }
}
