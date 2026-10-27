use super::*;
struct Observation {
    owner: std::rc::Weak<Cell<bool>>,
    closing: Rc<RefCell<Vec<bool>>>,
}
thread_local! { static CURRENT: RefCell<Option<Observation>> = const { RefCell::new(None) }; }
pub(crate) struct Probe {
    previous: Option<Observation>,
    closing: Rc<RefCell<Vec<bool>>>,
}
impl Probe {
    pub(crate) fn new(binding: &CallbackBinding) -> Self {
        let closing = Rc::new(RefCell::new(Vec::new()));
        let previous = CURRENT.with_borrow_mut(|current| {
            current.replace(Observation {
                owner: Rc::downgrade(&binding.closing),
                closing: closing.clone(),
            })
        });
        Self { previous, closing }
    }
    pub(crate) fn attempts(&self) -> Vec<bool> {
        self.closing.borrow().clone()
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        CURRENT.with_borrow_mut(|current| *current = self.previous.take());
    }
}
pub(super) fn record(binding: &CallbackBinding) {
    CURRENT.with_borrow(|current| {
        if let Some(current) = current
            && std::rc::Weak::ptr_eq(&current.owner, &Rc::downgrade(&binding.closing))
        {
            current.closing.borrow_mut().push(binding.closing.get());
        }
    });
}
