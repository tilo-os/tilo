//! The page a window shows: `page.call` carried to the window that draws it, and answered there.
//!
//! The engine belongs to the window, and the app's actions run here: a page call is one message
//! to the window and one answer back, on the same connection the view travels on. A call waits
//! for its own answer, and whatever the person did meanwhile is kept in the order it came and
//! taken as the instance's next turn sees it.
//!
//! A call that answers a sign-in the person approved names no credentials of its own
//! (`"typed": true`): what the person typed is filled in here, from the approve the call was
//! delivered with, on its way to the window. It never becomes a value of the run, so its journal,
//! the action's record and history never keep it.
//!
//! A call an action makes of another app's action names a secret the same way, by where it is
//! rather than what it is ([`fill_secrets`]): `{"$typed": n}`, the nth thing the person typed on
//! the card for the call being run, or `{"$held": "<token>"}`, one the host read for the app from
//! what the person chose (a file they import) and holds under a token ([`hold`]). A call that
//! says `"$hold": true` has the text of its answer held the same way ([`keep_answer`]), so the
//! secret it answers reaches only what the host does with it next. One that says `"$show": true`
//! has it kept for the window instead: a view that leaves a capture out (`protect capture`) shows
//! it in the token's place ([`shown_in`]), for [`SHOWN_SECS`] at most, and every other view, the
//! run's journal and history read only the token.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, ToApp, ToUi, Transport};

thread_local! {
    /// The person's approve of the call being run on this thread, while it runs: what a sign-in
    /// it answers is filled from.
    static APPROVED: RefCell<Option<tilo_actions::remote::asked::Approval>> =
        const { RefCell::new(None) };
    /// The secrets the host holds for the app's code, each under its token, until a call or an
    /// engine takes it: each is taken once.
    static HELD: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());
    /// The calls whose answers are kept as they arrive, by the id each went out under, with how.
    static HOLDING: RefCell<std::collections::HashMap<String, Keep>> =
        RefCell::new(std::collections::HashMap::new());
    /// The secrets a view that leaves a capture out shows, each under its token, with the second
    /// it lapses at.
    static SHOWN: RefCell<std::collections::HashMap<String, (String, i64)>> =
        RefCell::new(std::collections::HashMap::new());
}

/// How long a secret answered for the window shows, in seconds: then it is hidden again.
pub const SHOWN_SECS: i64 = 30;

/// What stands in a protected view for a secret no longer shown.
const HIDDEN: &str = "••••••••••••";

/// How a call's answer is kept as it arrives ([`fill_secrets`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// As it is: it holds no secret.
    AsIs,
    /// Held for the host ([`hold`]), its code reading tokens.
    Held,
    /// Kept for a view that leaves a capture out ([`shown_in`]), its code reading tokens.
    Shown,
}

/// The most secrets the host holds for one app at once: a file of logins brought in at once.
const MOST_HELD: usize = 100_000;

/// Hold `secret` for the app's code, answering the token its code names it by
/// (`{"$held": "<token>"}`).
///
/// # Errors
/// The host holds as many as it holds for one app.
pub fn hold(secret: String) -> Result<String, String> {
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        if held.len() >= MOST_HELD {
            return Err("too many secrets are waiting to be used".to_owned());
        }
        let token = format!("held:{}", uuid::Uuid::new_v4().simple());
        held.insert(token.clone(), secret);
        Ok(token)
    })
}

/// Take the secret held under `token`: it is held no longer.
#[must_use]
pub fn take_held(token: &str) -> Option<String> {
    HELD.with(|held| held.borrow_mut().remove(token))
}

/// Fill in the secrets `value` names, wherever it names one: `{"$typed": n}` with the nth thing
/// the person typed for the call being run, `{"$held": token}` with what the host holds under
/// it. A top-level `"$hold": true` or `"$show": true` is taken out, and answers how the call's
/// answer is kept ([`keep_answer`]).
///
/// # Errors
/// A secret named that is not there to fill: nothing typed in that place, or no such token.
pub fn fill_secrets(value: &mut serde_json::Value) -> Result<Keep, String> {
    let mut keep = kept_as(value)?;
    // A call by address through the action registry carries the arguments of the action it
    // calls as its own `arguments`.
    if keep == Keep::AsIs
        && let Some(called) = value.get_mut("arguments")
    {
        keep = kept_as(called)?;
    }
    filled_in(value)?;
    Ok(keep)
}

/// How the answer to a call with the arguments `value` is kept, taking out what says so.
fn kept_as(value: &mut serde_json::Value) -> Result<Keep, String> {
    let serde_json::Value::Object(fields) = value else {
        return Ok(Keep::AsIs);
    };
    let yes = Some(serde_json::Value::Bool(true));
    let holds = fields.remove("$hold") == yes;
    let shows = fields.remove("$show") == yes;
    match (holds, shows) {
        (true, true) => Err("a call's answer is held or shown, not both".to_owned()),
        (true, false) => Ok(Keep::Held),
        (false, true) => Ok(Keep::Shown),
        (false, false) => Ok(Keep::AsIs),
    }
}

fn filled_in(value: &mut serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::Object(fields) if fields.len() == 1 => {
            if let Some(place) = fields.get("$typed").and_then(serde_json::Value::as_u64) {
                let typed = with_typed(|typed| {
                    usize::try_from(place)
                        .ok()
                        .and_then(|place| typed.get(place))
                        .map(|typed| (*typed).to_owned())
                })
                .ok_or("Nothing was typed on the card for it")?;
                *value = serde_json::Value::String(typed);
            } else if let Some(token) = fields.get("$held").and_then(serde_json::Value::as_str) {
                let held = take_held(token).ok_or("What was read for it is no longer here")?;
                *value = serde_json::Value::String(held);
            } else {
                for field in fields.values_mut() {
                    filled_in(field)?;
                }
            }
        }
        serde_json::Value::Object(fields) => {
            for field in fields.values_mut() {
                filled_in(field)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                filled_in(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keep the answer of the call that went out as `call` as `keep` says, as it arrives
/// ([`keep_answer`]).
pub fn keeping(call: &str, keep: Keep) {
    if keep != Keep::AsIs {
        HOLDING.with(|holding| holding.borrow_mut().insert(call.to_owned(), keep));
    }
}

/// `answer`, the answer of the call that went out as `call` and arrived at `now` (seconds since
/// the epoch), with each text in what it answered kept as that call asked ([`keeping`]) and
/// named by its token: held for the host ([`hold`]), or shown in a protected view until
/// [`SHOWN_SECS`] pass ([`shown_in`]). The code that made it reads only tokens.
pub fn keep_answer(call: &str, answer: &mut serde_json::Value, now: i64) {
    let Some(keep) = HOLDING.with(|holding| holding.borrow_mut().remove(call)) else {
        return;
    };
    // An action's output (`{value, resources}`); through the action registry, its value is the
    // invocation record, whose `output` is what the action answered.
    let Some(value) = answer.get_mut("value") else {
        return;
    };
    let answered = match value.get_mut("output") {
        Some(output) => output,
        None => value,
    };
    kept_texts(answered, &|text| match keep {
        Keep::Held => hold(text).ok(),
        Keep::Shown => Some(show(text, now.saturating_add(SHOWN_SECS))),
        Keep::AsIs => Some(text),
    });
}

fn kept_texts(value: &mut serde_json::Value, keep: &dyn Fn(String) -> Option<String>) {
    match value {
        serde_json::Value::String(text) => {
            // Nothing kept is nothing answered.
            *text = keep(std::mem::take(text)).unwrap_or_default();
        }
        serde_json::Value::Object(fields) => fields.values_mut().for_each(|v| kept_texts(v, keep)),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| kept_texts(v, keep)),
        _ => {}
    }
}

/// Keep `secret` for a protected view until `until`, answering the token the view shows it by.
fn show(secret: String, until: i64) -> String {
    let token = format!("shown:{}", uuid::Uuid::new_v4().simple());
    SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        if shown.len() < MOST_HELD {
            shown.insert(token.clone(), (secret, until));
        }
    });
    token
}

/// Forget each secret shown whose time is up at `now`, answering whether one was: the windows
/// showing it are drawn again, hiding it.
pub fn lapse_shown(now: i64) -> bool {
    SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let before = shown.len();
        shown.retain(|_, (_, until)| *until > now);
        shown.len() != before
    })
}

/// The second the first secret shown lapses at, if one is shown.
#[must_use]
pub fn next_lapse() -> Option<i64> {
    SHOWN.with(|shown| shown.borrow().values().map(|(_, until)| *until).min())
}

/// `tree`, the tree of a view that leaves a capture out, with each secret it names by its
/// token in the token's place, and each one no longer shown hidden: the one place a secret
/// answered for the window ([`Keep::Shown`]) is read.
pub fn shown_in(tree: &mut tilo_render_protocol::patch::Node) {
    for value in tree.props.values_mut() {
        if let tilo_render_protocol::patch::Value::Text(text) = value
            && text.starts_with("shown:")
        {
            *text = SHOWN
                .with(|shown| {
                    shown
                        .borrow()
                        .get(text.as_str())
                        .map(|(secret, _)| secret.clone())
                })
                .unwrap_or_else(|| HIDDEN.to_owned());
        }
    }
    tree.children.iter_mut().for_each(shown_in);
}

/// Run `run` as the call the person approved with `approval`: a page call it makes that answers
/// a sign-in with what they typed (`"typed": true`) is filled from it. Nothing of it outlives
/// `run`.
pub fn answering<R>(
    approval: Option<tilo_actions::remote::asked::Approval>,
    run: impl FnOnce() -> R,
) -> R {
    let before = APPROVED.with(|slot| slot.replace(approval));
    let ran = run();
    APPROVED.with(|slot| slot.replace(before));
    ran
}

/// Whether this thread runs a call the person approved on the system's confirm
/// ([`answering`]).
pub(crate) fn approved() -> bool {
    APPROVED.with(|slot| slot.borrow().is_some())
}

/// Hand `take` what the person typed on the system's card for the call they approved and this
/// thread runs, one entry per take in order: what a host passes straight to where it goes (a
/// page's sign-in, the workspace's vault), so it never becomes a value of the run. Nothing
/// outside such a call.
pub fn with_typed<R>(take: impl FnOnce(&[&str]) -> R) -> R {
    APPROVED.with(|slot| {
        let held = slot.borrow();
        let typed: Vec<&str> = held
            .as_ref()
            .map(|approval| approval.typed.iter().map(|typed| typed.as_str()).collect())
            .unwrap_or_default();
        take(&typed)
    })
}

/// `request` with what the person typed filled in, where it asks for it (`"typed": true`): the
/// first take is the user name, the second the password. Without an approve that typed both, it
/// names no credentials, which turns the sign-in down.
fn filled(request: &str) -> std::borrow::Cow<'_, str> {
    let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_str(request) else {
        return request.into();
    };
    if fields.remove("typed") != Some(serde_json::Value::Bool(true)) {
        return request.into();
    }
    APPROVED.with(|slot| {
        if let Some(approval) = slot.borrow().as_ref()
            && let [user, password, ..] = approval.typed.as_slice()
        {
            fields.insert("username".to_owned(), user.as_str().into());
            fields.insert("password".to_owned(), password.as_str().into());
        }
    });
    serde_json::Value::Object(fields).to_string().into()
}

/// One app window's page: the connection to the window, and what arrived while a call waited.
pub struct PageStation {
    transport: Rc<RefCell<Box<dyn Transport>>>,
    /// What the window sent while a call waited for its answer, oldest first.
    waiting: RefCell<VecDeque<ToApp>>,
    /// The id of the last call this station asked about.
    next: Cell<u32>,
}

impl PageStation {
    #[must_use]
    pub fn new(transport: Rc<RefCell<Box<dyn Transport>>>) -> Self {
        Self {
            transport,
            waiting: RefCell::new(VecDeque::new()),
            next: Cell::new(0),
        }
    }

    /// What the window sent while a call waited, oldest first: the instance takes them as its
    /// next turn sees them.
    pub fn waiting(&self) -> Vec<ToApp> {
        self.waiting.borrow_mut().drain(..).collect()
    }
}

/// The station as the runtime's host: each call is one message to the window and its answer.
pub struct SharedPage(pub Rc<PageStation>);

impl tilolang_runtime::page::PageHost for SharedPage {
    fn call(&self, request: &str) -> Result<String, String> {
        let request_id = self.0.next.get().wrapping_add(1);
        self.0.next.set(request_id);
        let batch = ui::encode_batch(&[ToUi::Page {
            request_id,
            request: filled(request).into_owned().into_bytes(),
        }])
        .map_err(|error| format!("the page request did not encode: {error}"))?;
        self.0
            .transport
            .borrow_mut()
            .send(batch)
            .map_err(|error| format!("the window could not be asked: {error}"))?;
        loop {
            let batch = match self.0.transport.borrow_mut().recv() {
                Ok(Some(batch)) => batch,
                Ok(None) => return Err("the window closed".to_owned()),
                Err(error) => return Err(format!("the window could not answer: {error}")),
            };
            let messages: Vec<ToApp> = ui::decode_batch(&batch)
                .map_err(|error| format!("the window sent what is no batch: {error}"))?;
            let mut answered = None;
            for message in messages {
                match message {
                    ToApp::PageAnswer {
                        request_id: id,
                        answer,
                    } if id == request_id => answered = Some(answer),
                    other => self.0.waiting.borrow_mut().push_back(other),
                }
            }
            if let Some(answer) = answered {
                if answer.is_empty() {
                    return Err("this app's window shows no page".to_owned());
                }
                return String::from_utf8(answer)
                    .map_err(|_| "the page answered what is no text".to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilolang_runtime::page::PageHost as _;

    const PASSWORD: &str = "correct horse battery staple";

    /// A window that answers every page call `{"ok":true}` and keeps what it was asked.
    #[derive(Default)]
    struct Window {
        asked: Rc<RefCell<Vec<Vec<u8>>>>,
        answer: Option<Vec<u8>>,
    }

    impl Transport for Window {
        fn send(&mut self, batch: Vec<u8>) -> Result<(), tilo_render_protocol::WireError> {
            for message in ui::decode_batch::<ToUi>(&batch).unwrap() {
                if let ToUi::Page {
                    request_id,
                    request,
                } = message
                {
                    self.asked.borrow_mut().push(request);
                    self.answer = Some(
                        ui::encode_batch(&[ToApp::PageAnswer {
                            request_id,
                            answer: br#"{"ok":true}"#.to_vec(),
                        }])
                        .unwrap(),
                    );
                }
            }
            Ok(())
        }

        fn recv(&mut self) -> Result<Option<Vec<u8>>, tilo_render_protocol::WireError> {
            Ok(self.answer.take())
        }
    }

    fn page() -> (SharedPage, Rc<RefCell<Vec<Vec<u8>>>>) {
        let window = Window::default();
        let asked = Rc::clone(&window.asked);
        let transport: Box<dyn Transport> = Box::new(window);
        let station = PageStation::new(Rc::new(RefCell::new(transport)));
        (SharedPage(Rc::new(station)), asked)
    }

    fn approve(typed: &[&str]) -> tilo_actions::remote::asked::Approval {
        let mut approval = tilo_actions::remote::asked::Approval::new(
            tilo_actions::remote::asked::RecordRef {
                workspace: "w1".into(),
                record: "r1".into(),
                device: None,
            },
            None,
        );
        approval.typed = typed
            .iter()
            .map(|typed| (*typed).to_owned().into())
            .collect();
        approval
    }

    const SIGN_IN: &str = r#"{"op":"answer_sign_in","id":4,"asker":"a1","typed":true}"#;

    /// A call names what the person typed and what the host holds by where they are: each is
    /// filled in on its way out, a held one once, and a call that asks has its answer held.
    #[test]
    fn a_call_names_its_secrets_and_never_holds_them() {
        let token = hold("made-up-held-secret".to_owned()).unwrap();
        let mut asked = serde_json::json!({
            "$hold": true,
            "address": "tilo.vault/vault#add",
            "arguments": {
                "secret": {"$typed": 0},
                "notes": {"$held": token},
                "fields": [{"label": "PIN", "value": {"$typed": 1}}],
            },
        });
        let holds = answering(Some(approve(&[PASSWORD, "4711"])), || {
            fill_secrets(&mut asked)
        })
        .unwrap();
        assert_eq!(holds, Keep::Held);
        assert_eq!(
            asked,
            serde_json::json!({
                "address": "tilo.vault/vault#add",
                "arguments": {
                    "secret": PASSWORD,
                    "notes": "made-up-held-secret",
                    "fields": [{"label": "PIN", "value": "4711"}],
                },
            })
        );
        // A held secret is taken once, and nothing is typed outside an approved call.
        let mut again = serde_json::json!({"secret": {"$held": token}});
        assert!(fill_secrets(&mut again).is_err());
        let mut untyped = serde_json::json!({"secret": {"$typed": 0}});
        assert!(fill_secrets(&mut untyped).is_err());

        keeping("call-1", holds);
        let mut answer =
            serde_json::json!({"value": {"status": "ended", "output": {"value": PASSWORD}}});
        keep_answer("call-1", &mut answer, 0);
        let held = answer["value"]["output"]["value"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(held, PASSWORD);
        assert_eq!(
            answer["value"]["status"], "ended",
            "only what it answered is held"
        );
        assert_eq!(take_held(&held).as_deref(), Some(PASSWORD));
        let mut later = serde_json::json!({"value": {"output": {"value": PASSWORD}}});
        keep_answer("call-1", &mut later, 0);
        assert_eq!(
            later["value"]["output"]["value"], PASSWORD,
            "a call's answer is held once"
        );
    }

    /// A secret answered for the window shows only in a protected view's tree, in its token's
    /// place, and is hidden once its time is up.
    #[test]
    fn a_shown_secret_reaches_only_a_protected_tree_and_lapses() {
        // Called by its address through the action registry, which carries its arguments.
        let mut asked = serde_json::json!({
            "address": "tilo.vault/vault#reveal",
            "arguments": {"$show": true, "item": "vault.item:1"},
        });
        let keep = fill_secrets(&mut asked).unwrap();
        assert_eq!(keep, Keep::Shown);
        assert_eq!(
            asked,
            serde_json::json!({
                "address": "tilo.vault/vault#reveal",
                "arguments": {"item": "vault.item:1"},
            })
        );
        keeping("call-2", keep);
        let mut answer = serde_json::json!({"value": {"output": {"value": PASSWORD}}});
        keep_answer("call-2", &mut answer, 100);
        let token = answer["value"]["output"]["value"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(token.starts_with("shown:"), "{token}");
        assert_eq!(
            take_held(&token),
            None,
            "a shown secret is no host's to take"
        );

        let tree = |text: &str| {
            let mut row = tilo_render_protocol::patch::Node::empty("text", "secret");
            row.props.insert(
                "text".to_owned(),
                tilo_render_protocol::patch::Value::Text(text.to_owned()),
            );
            let mut root = tilo_render_protocol::patch::Node::empty("stack", "root");
            root.children.push(row);
            root
        };
        let text_of =
            |root: &tilo_render_protocol::patch::Node| root.children[0].props["text"].clone();
        let mut drawn = tree(&token);
        shown_in(&mut drawn);
        assert_eq!(
            text_of(&drawn),
            tilo_render_protocol::patch::Value::Text(PASSWORD.to_owned())
        );
        assert!(
            !lapse_shown(100 + SHOWN_SECS - 1),
            "it shows until its time is up"
        );
        assert!(lapse_shown(100 + SHOWN_SECS));
        let mut drawn = tree(&token);
        shown_in(&mut drawn);
        assert_eq!(
            text_of(&drawn),
            tilo_render_protocol::patch::Value::Text(HIDDEN.to_owned())
        );
    }

    /// The sign-in the person approved reaches the window with what they typed, though the call
    /// the action made names none of it; outside that approved call nothing is filled in.
    #[test]
    fn a_sign_in_carries_what_the_person_typed_only_to_the_window() {
        let (page, asked) = page();
        page.call(SIGN_IN).unwrap();
        answering(Some(approve(&["ada", PASSWORD])), || page.call(SIGN_IN)).unwrap();
        page.call(SIGN_IN).unwrap();
        let asked: Vec<serde_json::Value> = asked
            .borrow()
            .iter()
            .map(|request| serde_json::from_slice(request).unwrap())
            .collect();
        let [before, approved, after] = asked.as_slice() else {
            panic!("three calls: {asked:?}");
        };
        assert_eq!(approved["username"], "ada");
        assert_eq!(approved["password"], PASSWORD);
        assert!(approved.get("typed").is_none(), "{approved}");
        for unapproved in [before, after] {
            assert!(
                unapproved.get("password").is_none() && unapproved.get("username").is_none(),
                "a sign-in nobody approved names no credentials, which turns it down: {unapproved}"
            );
        }
        assert!(
            !SIGN_IN.contains(PASSWORD),
            "the action's own call never names the password"
        );
    }
}
