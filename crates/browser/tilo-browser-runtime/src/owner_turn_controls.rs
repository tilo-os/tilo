//! Bounded, opt-in correctness diagnostics for the original Scratch owner path.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use serde_json::{Value, json};

#[derive(Default)]
struct Trace {
    next: u64,
    slots: BTreeMap<u64, Weak<ScratchControl>>,
    rows: Vec<Value>,
    overflow: bool,
    scope: Option<u64>,
    after_scan: Option<Weak<ScratchControl>>,
    failure: Option<String>,
}
thread_local! {
    static TRACE: RefCell<Trace> = RefCell::default();
}

pub(crate) fn record(row: Value) {
    TRACE.with_borrow_mut(|trace| {
        if trace.rows.len() == 4096 {
            trace.overflow = true;
        } else {
            trace.rows.push(row);
        }
    });
}

pub(crate) struct ScratchControl {
    slot: u64,
    wake: ui::WakeHandle,
    remaining: Cell<u32>,
}
impl ScratchControl {
    pub(crate) fn new(wake: ui::WakeHandle) -> Result<Rc<Self>, ui::launch::LaunchError> {
        TRACE.with_borrow_mut(|trace| {
            trace.slots.retain(|_, slot| slot.strong_count() != 0);
            if trace.slots.len() >= 32 {
                return Err(ui::launch::LaunchError::Operation(
                    "Scratch diagnostic slot cap".into(),
                ));
            }
            trace.next = trace.next.checked_add(1).ok_or_else(|| {
                ui::launch::LaunchError::Operation("Scratch diagnostic slot overflow".into())
            })?;
            let control = Rc::new(Self {
                slot: trace.next,
                wake,
                remaining: Cell::new(0),
            });
            trace.slots.insert(control.slot, Rc::downgrade(&control));
            Ok(control)
        })
    }
    pub(crate) fn after_update(&self) {
        let scope = TRACE.with_borrow(|trace| trace.scope);
        record(
            json!({"phase":"scratch-update", "slot":self.slot.to_string(), "mount":scope.map(|id| id.to_string())}),
        );
        if scope.is_some() && self.remaining.get() != 0 {
            self.remaining.set(self.remaining.get().saturating_sub(1));
            self.wake.wake();
        }
    }
}
impl Drop for ScratchControl {
    fn drop(&mut self) {
        TRACE.with_borrow_mut(|trace| {
            trace.slots.remove(&self.slot);
        });
    }
}

pub(crate) fn wake(slot: u64, self_wakes: u32) -> Result<(), String> {
    if self_wakes > 64 {
        return Err("self-wake cap exceeded".into());
    }
    let control = current(slot)?;
    control.remaining.set(self_wakes);
    control.wake.wake();
    Ok(())
}

fn current(slot: u64) -> Result<Rc<ScratchControl>, String> {
    TRACE
        .with_borrow(|trace| trace.slots.get(&slot).and_then(Weak::upgrade))
        .filter(|control| control.wake.is_active())
        .ok_or_else(|| "closed Scratch diagnostic slot".to_owned())
}

/// Retains no app lifetime and schedules no task itself.
pub(crate) fn arm_after_scan(slot: u64) -> Result<(), String> {
    let control = current(slot)?;
    TRACE.with_borrow_mut(|trace| {
        if trace.after_scan.is_some() {
            return Err("after-scan control already armed".into());
        }
        trace.after_scan = Some(Rc::downgrade(&control));
        Ok(())
    })
}

pub(crate) fn cancel_after_scan() {
    TRACE.with_borrow_mut(|trace| trace.after_scan = None);
}

pub(crate) fn after_scan() {
    let pending = TRACE.with_borrow_mut(|trace| trace.after_scan.take());
    let Some(pending) = pending else { return };
    let Some(control) = pending.upgrade().filter(|control| control.wake.is_active()) else {
        TRACE.with_borrow_mut(|trace| {
            trace.failure = Some("closed Scratch after-scan diagnostic slot".into());
        });
        return;
    };
    record(json!({"phase":"after-scan-wake", "slot":control.slot.to_string()}));
    control.wake.wake();
}

pub(crate) fn drain(mounts: &[Value]) -> Result<String, String> {
    TRACE.with_borrow_mut(|trace| {
        if let Some(failure) = &trace.failure {
            return Err(failure.clone());
        }
        if trace.overflow {
            return Err("owner-turn trace overflow".into());
        }
        let slots: Vec<_> = trace
            .slots
            .iter()
            .filter_map(|(slot, weak)| {
                weak.upgrade()
                    .filter(|control| control.wake.is_active())
                    .map(|_| slot.to_string())
            })
            .collect();
        serde_json::to_string(
            &json!({"slots":slots,"mounts":mounts,"rows":std::mem::take(&mut trace.rows)}),
        )
        .map_err(|error| error.to_string())
    })
}

pub(crate) struct ServiceScope(u64);
impl ServiceScope {
    pub(crate) fn enter(mount: u64, permit: Option<u64>) -> Self {
        TRACE.with_borrow_mut(|trace| {
            assert!(trace.scope.replace(mount).is_none());
        });
        record(
            json!({"phase":"owner-enter","mount":mount.to_string(),"permit":permit.map(|id| id.to_string())}),
        );
        Self(mount)
    }
}
impl Drop for ServiceScope {
    fn drop(&mut self) {
        TRACE.with_borrow_mut(|trace| trace.scope = None);
        record(json!({"phase":"owner-return","mount":self.0.to_string()}));
    }
}
