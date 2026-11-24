//! Waking one of the person's sleeping devices from this one.
//!
//! A wake is asked in the record of the person's devices, through this device's daemon, and the
//! daemons of their devices carry it out from there
//! ([`tilo_workspace_service::workspaces::person_workspace::wake`]): one on the sleeping device's
//! LAN sends the magic packets, and the device says it is back once it is. This session asks the
//! wake the person pressed for, and tells them how each wake it asked ended, read from the same
//! record.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_workspace_service::workspaces::person_workspace::devices::Devices;
use tilo_workspace_service::workspaces::person_workspace::wake::WakeState;

use crate::Shared;

/// The wakes this session asked, as it tells the person how they end.
pub(crate) struct Wake {
    /// When this session started, ns since the epoch: a wake asked before it was told by the
    /// session that asked it.
    since_ns: i64,
    /// The wakes it told the person the end of, by device and when it was asked.
    told: RefCell<BTreeSet<(String, i64)>>,
    /// When how one of its wakes stands next changes without anything written.
    deadline: Cell<Option<Instant>>,
}

impl Wake {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            since_ns: now_ns(),
            told: RefCell::default(),
            deadline: Cell::new(None),
        })
    }

    /// Wake the device the person named, by its key or its label.
    ///
    /// # Errors
    /// Why it cannot be woken, in the person's words.
    pub(crate) fn request(&self, shared: &Shared, name: &str) -> Result<(), String> {
        shared
            .owner_devices
            .as_ref()
            .ok_or_else(|| "this device has not read the person's devices yet".to_owned())?
            .wake(name)
    }

    /// Tell the person how each wake this device asked ended, once the record says it did.
    /// Called every pass.
    pub(crate) fn follow(&self, shared: &Shared) {
        let Some(devices) = shared.owner_devices.as_ref() else {
            return;
        };
        let (_, local) = devices.who();
        let record = devices.devices();
        let now = now_ns();
        let mut next: Option<i64> = None;
        let mut told = self.told.borrow_mut();
        for (target, ask) in record.wakes.iter() {
            if ask.asked_by != local || ask.asked_ns < self.since_ns {
                continue;
            }
            let state = ask.state(now);
            if matches!(state, WakeState::Asked | WakeState::Sent { .. }) {
                if let Some(at) = ask.next_change_ns() {
                    next = Some(next.map_or(at, |next| next.min(at)));
                }
                continue;
            }
            if !told.insert((target.as_str().to_owned(), ask.asked_ns)) {
                continue;
            }
            if let Some(notice) = crate::notices::wake_notice(&label(&record, target), &state) {
                shared.notifications.borrow().tell(&notice);
            }
        }
        self.deadline.set(next.map(|at| {
            let wait = u64::try_from(at.saturating_sub(now)).unwrap_or(0);
            Instant::now() + Duration::from_nanos(wait)
        }));
    }

    /// When how one of this device's wakes stands next changes.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.deadline.get()
    }
}

/// What the person calls the device `record` keeps under `key`.
fn label(record: &Devices, key: &tilo_resource::Id) -> String {
    record
        .devices
        .get(key)
        .and_then(|device| Some(device.called(&device.cert()?)))
        .unwrap_or_else(|| key.as_str().to_owned())
}

fn now_ns() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns()).unwrap_or(i64::MAX)
}
