//! Local identity of the executing checked component callback: its lifetime scope, person, and
//! device. Set only while a callback runs.
use std::cell::RefCell;

use crate::permissions::MemberId;

thread_local! {
    static EXECUTING_SCOPE: RefCell<Option<uuid::Uuid>> = const { RefCell::new(None) };
    static EXECUTING_DEVICE: RefCell<Option<String>> = const { RefCell::new(None) };
    static EXECUTING_PERSON: RefCell<Option<MemberId>> = const { RefCell::new(None) };
}

/// Identity of the checked component lifetime, not a caller-selected window ID.
#[must_use]
pub fn executing_scope() -> Option<uuid::Uuid> {
    EXECUTING_SCOPE.with_borrow(|scope| *scope)
}

pub(super) struct ExecutingScope(Option<uuid::Uuid>);
impl ExecutingScope {
    pub(super) fn enter(scope: uuid::Uuid) -> Self {
        Self(EXECUTING_SCOPE.replace(Some(scope)))
    }
}
impl Drop for ExecutingScope {
    fn drop(&mut self) {
        EXECUTING_SCOPE.replace(self.0.take());
    }
}

pub(super) struct ExecutingHostPerson(Option<MemberId>);
impl ExecutingHostPerson {
    pub(super) fn enter(person: &MemberId) -> Self {
        Self(EXECUTING_PERSON.replace(Some(person.clone())))
    }
}
impl Drop for ExecutingHostPerson {
    fn drop(&mut self) {
        EXECUTING_PERSON.replace(self.0.take());
    }
}

/// The immutable local person of the executing checked component callback.
/// Unavailable outside a callback; never derived from action context or a caller.
#[must_use]
pub fn executing_person() -> Option<MemberId> {
    EXECUTING_PERSON.with_borrow(Clone::clone)
}

/// Whether this callback is acting for its trusted local person. A delivered
/// invocation must name that actual person's actor; member identity and
/// action attribution cannot confer access. Outside a callback access is denied.
#[must_use]
pub fn local_person_invocation_permitted() -> bool {
    executing_person().is_some_and(|person| {
        crate::invocation_scope::current_invoker()
            .is_none_or(|invoker| invoker.acting.member_id() == Some(&person))
    })
}

pub(super) struct ExecutingHostDevice(Option<String>);
impl ExecutingHostDevice {
    pub(super) fn enter(device: &str) -> Self {
        Self(EXECUTING_DEVICE.replace(Some(device.to_owned())))
    }
}
impl Drop for ExecutingHostDevice {
    fn drop(&mut self) {
        EXECUTING_DEVICE.replace(self.0.take());
    }
}

/// The executing component's host device during a checked record callback.
/// This is local host identity, not replicated action context or a caller's
/// requested target device. Outside a component callback it is unavailable.
#[must_use]
pub fn executing_device() -> Option<String> {
    EXECUTING_DEVICE.with_borrow(Clone::clone)
}
