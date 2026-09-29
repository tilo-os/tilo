//! Clipboard events a host dispatches to the app.
//!
//! A browser grants clipboard access only inside a trusted `copy`, `cut` or
//! `paste` event, or inside the user's click. The web runner wraps its
//! dispatch of such an event in [`during`] (or of a click in
//! [`during_input`]), and the web backend of [`system`](crate::system) reads
//! and writes through it. Nothing survives the event: a clipboard payload is
//! never kept past the dispatch that received it.
//!
//! The scopes compile and nest on every target, so a host's dispatch code has
//! no `cfg`; the desktop backends ignore them.
#![cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code, reason = "only the web backend reads the event scopes")
)]

use std::cell::RefCell;
use std::rc::Rc;

/// The data of one trusted clipboard event (the browser's `DataTransfer`).
pub trait EventTransfer {
    /// Offer `text` as `text/plain`. Returns whether the event took it.
    fn set_text(&self, text: &str) -> bool;
    /// The event's `text/plain`, if any.
    fn text(&self) -> Option<String>;
}

/// How a copy made inside a click ended, reported to whoever dispatched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyReport {
    Copied,
    Failed(String),
}

/// Where a copy made inside a click reports how it ended.
type Report = Rc<dyn Fn(CopyReport)>;

struct Transfer {
    data: Rc<dyn EventTransfer>,
    read: bool,
    copied: bool,
}

thread_local! {
    static TRANSFER: RefCell<Option<Transfer>> = const { RefCell::new(None) };
    static INPUT: RefCell<Option<Report>> = const { RefCell::new(None) };
}

/// Run `run` while `transfer` is the clipboard event being dispatched. The
/// event is read at most once and written at most once.
pub fn during<R>(transfer: Rc<dyn EventTransfer>, run: impl FnOnce() -> R) -> R {
    struct Clear(Option<Transfer>);
    impl Drop for Clear {
        fn drop(&mut self) {
            let previous = self.0.take();
            TRANSFER.with_borrow_mut(|slot| *slot = previous);
        }
    }
    let previous = TRANSFER.with_borrow_mut(|slot| {
        slot.replace(Transfer {
            data: transfer,
            read: false,
            copied: false,
        })
    });
    let _clear = Clear(previous);
    run()
}

/// Run `run` while dispatching the user's input event (a click on Copy).
/// A copy made inside may publish through the browser's own copy command;
/// `report` hears how it ended.
pub fn during_input<R>(report: Rc<dyn Fn(CopyReport)>, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Report>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            INPUT.with_borrow_mut(|slot| *slot = previous);
        }
    }
    let _restore = Restore(INPUT.with_borrow_mut(|slot| slot.replace(report)));
    run()
}

/// Whether the app wrote the clipboard event being dispatched. A host uses it
/// to decide whether to suppress the browser's default cut.
pub fn copied() -> bool {
    TRANSFER.with_borrow(|slot| slot.as_ref().is_some_and(|t| t.copied))
}

/// Whether a clipboard event is being dispatched on this thread.
pub fn in_event() -> bool {
    TRANSFER.with_borrow(Option::is_some)
}

/// Write `text` to the event being dispatched. `None` when there is none.
pub(crate) fn write(text: &str) -> Option<bool> {
    TRANSFER.with_borrow_mut(|slot| {
        let t = slot.as_mut()?;
        if !t.copied {
            t.copied = t.data.set_text(text);
        }
        Some(t.copied)
    })
}

/// Read the event being dispatched, once. `None` when there is none, it was
/// already read, or it holds no text.
pub(crate) fn read() -> Option<String> {
    TRANSFER.with_borrow_mut(|slot| {
        let t = slot.as_mut()?;
        if t.read {
            return None;
        }
        t.read = true;
        t.data.text().filter(|text| !text.is_empty())
    })
}

/// The report of the input event being dispatched, if any.
pub(crate) fn input_report() -> Option<Report> {
    INPUT.with_borrow(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Fake {
        offered: RefCell<Vec<String>>,
        text: Option<String>,
        refuse: bool,
        reads: Cell<usize>,
    }

    impl EventTransfer for Fake {
        fn set_text(&self, text: &str) -> bool {
            self.offered.borrow_mut().push(text.to_owned());
            !self.refuse
        }
        fn text(&self) -> Option<String> {
            self.reads.set(self.reads.get().saturating_add(1));
            self.text.clone()
        }
    }

    #[test]
    fn an_event_is_written_and_read_once_and_nothing_outlives_it() {
        let fake = Rc::new(Fake {
            text: Some("pasted".into()),
            ..Fake::default()
        });
        assert_eq!(write("x"), None);
        during(fake.clone(), || {
            assert!(in_event());
            assert_eq!(read().as_deref(), Some("pasted"));
            assert_eq!(read(), None, "a paste is read once");
            assert_eq!(write("first"), Some(true));
            assert_eq!(write("second"), Some(true));
            assert!(copied());
        });
        assert!(!in_event());
        assert!(!copied());
        assert_eq!(*fake.offered.borrow(), ["first"]);
        assert_eq!(fake.reads.get(), 1);
    }

    #[test]
    fn a_refused_write_is_not_copied_and_scopes_nest() {
        let refusing = Rc::new(Fake {
            refuse: true,
            ..Fake::default()
        });
        let outer = Rc::new(Fake::default());
        during(outer.clone(), || {
            during(refusing.clone(), || {
                assert_eq!(write("secret"), Some(false));
                assert!(!copied());
            });
            assert_eq!(write("outer"), Some(true));
        });
        assert_eq!(*outer.offered.borrow(), ["outer"]);
    }

    #[test]
    fn input_reports_are_scoped() {
        assert!(input_report().is_none());
        let heard = Rc::new(RefCell::new(Vec::new()));
        let sink = heard.clone();
        during_input(Rc::new(move |r| sink.borrow_mut().push(r)), || {
            input_report().unwrap()(CopyReport::Copied);
        });
        assert!(input_report().is_none());
        assert_eq!(*heard.borrow(), [CopyReport::Copied]);
    }
}
