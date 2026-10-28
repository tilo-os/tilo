//! A transport within one process: an app's instance and its window side by side, as in a
//! capture, a test, or a browser page that runs both. The batches are the bytes any transport
//! carries, so what is validated and applied is exactly what a socket would deliver.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::Transport;
use crate::WireError;

type Queue = Rc<RefCell<VecDeque<Vec<u8>>>>;

/// One end of a pair: it sends to the other end and receives what the other end sent.
pub struct End {
    outgoing: Queue,
    incoming: Queue,
}

/// Two ends joined to each other.
#[must_use]
pub fn pair() -> (End, End) {
    let a: Queue = Rc::default();
    let b: Queue = Rc::default();
    (
        End {
            outgoing: Rc::clone(&a),
            incoming: Rc::clone(&b),
        },
        End {
            outgoing: b,
            incoming: a,
        },
    )
}

impl End {
    /// Whether the other end sent something this end has not received.
    #[must_use]
    pub fn has_incoming(&self) -> bool {
        !self.incoming.borrow().is_empty()
    }
}

impl Transport for End {
    fn send(&mut self, batch: Vec<u8>) -> Result<(), WireError> {
        self.outgoing.borrow_mut().push_back(batch);
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        Ok(self.incoming.borrow_mut().pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_end_receives_what_the_other_sent_in_order() {
        let (mut app, mut window) = pair();
        app.send(vec![1]).expect("sends");
        app.send(vec![2]).expect("sends");
        assert!(window.has_incoming());
        assert_eq!(window.recv(), Ok(Some(vec![1])));
        assert_eq!(window.recv(), Ok(Some(vec![2])));
        assert_eq!(window.recv(), Ok(None));
        window.send(vec![3]).expect("sends");
        assert_eq!(app.recv(), Ok(Some(vec![3])));
    }
}
