//! The host's loop: what ends a wait, and what leaves an idle host asleep.

use super::*;

/// How many of `rounds` waits of the host's loop would end at once, nothing written to its wake:
/// each a wait that asks whether anything has something for the host, without waiting.
fn wakeups(host: &TilolangHost, rounds: usize) -> usize {
    let wake = tilolang_host::wait::Wake::new().unwrap();
    (0..rounds)
        .filter(|_| {
            tilolang_host::wait::wait(None, wake.fd(), None, host, 0)
                .unwrap()
                .any()
        })
        .count()
}

#[test]
fn the_hosts_wake_ends_one_wait_per_writing_once_consumed() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    let wake = tilolang_host::wait::Wake::new().unwrap();
    let waker = wake.waker();
    let woken = |wake: &tilolang_host::wait::Wake| {
        (0..100)
            .filter(|_| {
                tilolang_host::wait::wait(None, wake.fd(), None, &host, 0)
                    .unwrap()
                    .wake
            })
            .count()
    };
    assert_eq!(woken(&wake), 0, "an unwritten wake ended a wait");
    for _ in 0..1000 {
        waker();
    }
    assert!(woken(&wake) > 0, "a written wake did not end the wait");
    wake.consume().unwrap();
    assert_eq!(woken(&wake), 0, "the consumed wake still ended waits");
}

#[test]
fn an_idle_hosts_loop_does_not_wake_until_a_window_has_something() {
    let (mut host, mut window, _sent, _inbox, _route) = running("ada");
    settle(&mut host, &mut window);
    assert_eq!(
        wakeups(&host, 100),
        0,
        "an idle running instance woke its host"
    );

    // A second instance whose workspace has not answered its join, its window sending before
    // the instance runs: what it sent waits for the instance, and the loop sleeps meanwhile.
    let answer = Rc::new(std::cell::Cell::new(false));
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let mut manager = CollaborationManager::with_peer_transport(SlowBroker {
        route: route.clone(),
        answer: Rc::clone(&answer),
        asked: false,
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some("grace".to_owned()),
    }));
    let (ours, theirs) = Channel::pair().unwrap();
    host.open(
        Spec {
            app: APP.to_owned(),
            workspace: route.cluster_id.to_string(),
            instance: 2,
            person: "grace".to_owned(),
        },
        Arc::new(counter()),
        manager,
        ours,
    );
    let mut second = Window::over(ChannelTransport::new(theirs).unwrap());
    second.send(&[ToApp::Fire {
        lane: Lane::Root,
        key: "early".to_owned(),
        event: "pressed before it opened".to_owned(),
        value: None,
    }]);
    host.turn();
    assert_eq!(host.opening(), 1);
    assert_eq!(
        wakeups(&host, 100),
        0,
        "what an opening instance's window sent woke its host before the instance could read it"
    );

    // What a running window sends wakes the loop, and its frame takes it.
    let (key, event) = window.increment();
    window.send(&[ToApp::Fire {
        lane: Lane::Root,
        key,
        event,
        value: None,
    }]);
    assert_eq!(
        wakeups(&host, 1),
        1,
        "a press in a running window did not wake its host"
    );
    host.turn();
    window.take();
    assert_eq!(wakeups(&host, 100), 0, "the frame left the press unread");

    // An opening instance's window closing wakes the loop, which ends that opening.
    drop(second);
    assert_eq!(
        wakeups(&host, 1),
        1,
        "an opening window's closing did not wake its host"
    );
    host.turn();
    assert_eq!(host.opening(), 0);
    assert_eq!(host.take_ended(), vec![2]);
    assert_eq!(wakeups(&host, 100), 0);
}
