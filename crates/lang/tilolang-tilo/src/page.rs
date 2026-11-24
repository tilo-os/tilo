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

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, ToApp, ToUi, Transport};

thread_local! {
    /// The person's approve of the call being run on this thread, while it runs: what a sign-in
    /// it answers is filled from.
    static APPROVED: RefCell<Option<tilo_actions::remote::asked::Approval>> =
        const { RefCell::new(None) };
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
        approval.typed = typed.iter().map(|typed| (*typed).to_owned().into()).collect();
        approval
    }

    const SIGN_IN: &str = r#"{"op":"answer_sign_in","id":4,"asker":"a1","typed":true}"#;

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
