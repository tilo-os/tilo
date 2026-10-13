#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use tilo_ui::{LatestRequest, LatestTask, LocalState, WakeEvent, WakeScope};

type Message = (u64, WakeEvent);

struct Mount {
    scope: WakeScope,
    state: LocalState<u32>,
    syncs: usize,
}

fn mount(id: u64, tx: Sender<Message>) -> Mount {
    let mut scope = WakeScope::new();
    let state = {
        let _entry = scope.enter();
        LocalState::new(0)
    };
    scope
        .bind(move |event| {
            let _ = tx.send((id, event));
        })
        .unwrap();
    Mount {
        scope,
        state,
        syncs: 0,
    }
}

fn dispatch(mounts: &mut BTreeMap<u64, Mount>, message: Message) -> bool {
    let (id, event) = message;
    let Some(mount) = mounts.get_mut(&id) else {
        return false;
    };
    if !mount.scope.accepts(&event) {
        return false;
    }
    let _entry = mount.scope.enter();
    mount.syncs = mount.syncs.saturating_add(1);
    true
}

fn host() -> (BTreeMap<u64, Mount>, Sender<Message>, Receiver<Message>) {
    let (tx, rx) = mpsc::channel();
    (BTreeMap::new(), tx, rx)
}

#[test]
fn wake_scopes_route_only_to_their_own_mounts_and_hosts() {
    // Case `two_mounts_on_one_host_sync_only_their_own_background_changes`.
    {
        let (mut mounts, tx, rx) = host();
        mounts.insert(10, mount(10, tx.clone()));
        mounts.insert(20, mount(20, tx));
        let a = mounts.get(&10).unwrap().state.writer();
        let b = mounts.get(&20).unwrap().state.writer();
        assert!(std::thread::spawn(move || a.try_set(3)).join().unwrap());
        assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
        assert_eq!(mounts.get(&10).unwrap().syncs, 1);
        assert_eq!(mounts.get(&20).unwrap().syncs, 0);
        assert_eq!(mounts.get(&10).unwrap().state.get(), 3);
        assert_eq!(mounts.get(&20).unwrap().state.get(), 0);
        assert!(std::thread::spawn(move || b.try_set(4)).join().unwrap());
        assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
        assert_eq!(mounts.get(&10).unwrap().syncs, 1);
        assert_eq!(mounts.get(&20).unwrap().syncs, 1);
    }

    // Case `two_hosts_keep_destinations_when_the_other_binds_and_drops`.
    {
        let (mut a, tx_a, rx_a) = host();
        let (mut b, tx_b, rx_b) = host();
        a.insert(1, mount(1, tx_a));
        let writer_a = a.get(&1).unwrap().state.writer();
        b.insert(1, mount(1, tx_b));
        let writer_b = b.get(&1).unwrap().state.writer();
        writer_a.set(1);
        assert!(dispatch(&mut a, rx_a.try_recv().unwrap()));
        assert!(rx_b.try_recv().is_err());
        drop(a);
        assert!(!writer_a.try_set(2));
        writer_b.set(3);
        assert!(dispatch(&mut b, rx_b.try_recv().unwrap()));
        assert_eq!(b.get(&1).unwrap().state.get(), 3);
    }
}

#[test]
fn queued_and_detached_wakes_never_reach_the_wrong_mount() {
    // Case `queued_wake_cannot_recreate_missing_mount_or_reach_same_id_replacement`.
    {
        let (mut mounts, tx, rx) = host();
        mounts.insert(7, mount(7, tx.clone()));
        let old_state = mounts.get(&7).unwrap().state.clone();
        old_state.writer().set(1);
        let queued = rx.try_recv().unwrap();
        let duplicate = queued.clone();
        let mut old = mounts.remove(&7).unwrap();
        old.scope.close();
        assert!(!dispatch(&mut mounts, duplicate));
        assert!(mounts.is_empty());
        mounts.insert(7, mount(7, tx));
        assert!(!dispatch(&mut mounts, queued));
        assert_eq!(mounts.get(&7).unwrap().syncs, 0);
        assert_eq!(mounts.get(&7).unwrap().state.get(), 0);
        assert!(!old_state.writer().try_set(2));
        drop(old);
        mounts.get(&7).unwrap().state.writer().set(9);
        assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
        assert_eq!(mounts.get(&7).unwrap().syncs, 1);
        assert_eq!(mounts.get(&7).unwrap().state.get(), 9);
    }

    // Case `explicitly_detached_headless_work_stays_functional_after_host_creation`.
    {
        let (state, task, mut request) = {
            let _detached = WakeScope::enter_detached();
            (LocalState::new(0), LatestTask::new(0), LatestRequest::new())
        };
        let (mut mounts, tx, rx) = host();
        mounts.insert(1, mount(1, tx));
        {
            let _host_entry = mounts.get(&1).unwrap().scope.enter();
            assert!(state.writer().try_set(1));
            assert!(task.begin(1).complete(2));
            assert!(request.begin().complete(3));
        }
        assert_eq!(state.get(), 1);
        assert_eq!(task.state().get(), 2);
        assert_eq!(request.try_take(), Some(3));
        assert!(rx.try_recv().is_err());
        assert_eq!(mounts.get(&1).unwrap().syncs, 0);
    }
}
