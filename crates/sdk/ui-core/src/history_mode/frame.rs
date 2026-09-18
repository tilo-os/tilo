//! Complete input-frame suppression, restored across nesting and unwind.
thread_local! { static FRAME_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) struct FrameworkInputFrame {
    previous_thread: bool,
    flag: std::rc::Rc<std::cell::Cell<bool>>,
    previous: bool,
}
impl FrameworkInputFrame {
    pub(crate) fn active() -> bool {
        FRAME_ACTIVE.get()
    }
    pub(crate) fn new(flag: &std::rc::Rc<std::cell::Cell<bool>>, framework: bool) -> Self {
        let previous = flag.replace(flag.get() || framework);
        let previous_thread = FRAME_ACTIVE.with(|active| active.replace(active.get() || framework));
        Self {
            flag: flag.clone(),
            previous,
            previous_thread,
        }
    }
}
impl Drop for FrameworkInputFrame {
    fn drop(&mut self) {
        self.flag.set(self.previous);
        FRAME_ACTIVE.set(self.previous_thread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_framework_input_guards_restore_runner_and_thread_after_unwind() {
        let a = std::rc::Rc::new(std::cell::Cell::new(false));
        let b = std::rc::Rc::new(std::cell::Cell::new(false));
        assert!(!FrameworkInputFrame::active());
        let outer = FrameworkInputFrame::new(&a, true);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _nested = FrameworkInputFrame::new(&b, true);
            let _ordinary = FrameworkInputFrame::new(&a, false);
            assert!(a.get() && b.get() && FrameworkInputFrame::active());
            panic!("fixture unwind");
        }));
        assert!(failed.is_err());
        assert!(a.get() && FrameworkInputFrame::active());
        assert!(!b.get());
        drop(outer);
        assert!(!a.get() && !FrameworkInputFrame::active());
    }
}
