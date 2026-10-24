//! The room's presence as state of the frame: the host lends it around the
//! pass every widget gets whether or not it is drawn
//! ([`crate::widget::Widget::update`]), so a widget that is not on screen
//! still knows who is here.

use std::cell::Cell;
use std::ptr::NonNull;

use super::PresenceCtx;

thread_local! {
    static LENT: Cell<Option<NonNull<PresenceCtx>>> = const { Cell::new(None) };
}

/// Run `pass` with `presence` as this frame's presence ([`frame`]).
pub fn lend<R>(presence: &PresenceCtx, pass: impl FnOnce() -> R) -> R {
    struct Restore(Option<NonNull<PresenceCtx>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LENT.with(|lent| lent.set(self.0));
        }
    }
    let _restore = Restore(LENT.with(|lent| lent.replace(Some(NonNull::from(presence)))));
    pass()
}

/// Read this frame's presence, if a host lent one ([`lend`]).
pub fn frame<R>(read: impl FnOnce(&PresenceCtx) -> R) -> Option<R> {
    let lent = LENT.with(Cell::get)?;
    // SAFETY: `lend` stores a shared borrow for exactly the call that holds
    // it and puts the previous value back when that call ends (unwinding
    // included), so the pointer is valid and only shared while it is set.
    Some(read(unsafe { lent.as_ref() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lent_presence_is_read_only_inside_its_pass() {
        let presence = PresenceCtx::empty();
        assert!(frame(|_| ()).is_none());
        let inside = lend(&presence, || {
            frame(|lent| std::ptr::eq(lent, std::ptr::from_ref(&presence)))
        });
        assert_eq!(inside, Some(true));
        assert!(frame(|_| ()).is_none());
    }
}
