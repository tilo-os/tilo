//! The page a window shows: `page.call` carried to the window that draws it, and answered there.
//!
//! The engine belongs to the window, and the app's actions run here: a page call is one message
//! to the window and one answer back, on the same connection the view travels on. A call waits
//! for its own answer, and whatever the person did meanwhile is kept in the order it came and
//! taken as the instance's next turn sees it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, ToApp, ToUi, Transport};

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
            request: request.as_bytes().to_vec(),
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
