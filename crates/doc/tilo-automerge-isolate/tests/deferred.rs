//! A copy that admits a writer's changes as they come and applies them only
//! where it is read.

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT};
use tilo_automerge_isolate::{Applied, Deferred, Dependencies, Isolate, Limits};

fn fresh_isolate() -> Isolate {
    Isolate::new(Limits {
        timeout: tilo_test_env::hang_bound(),
        ..Limits::default()
    })
}

/// `writer` after one more change: the change.
fn written(writer: &mut AutoCommit, key: &str) -> Vec<u8> {
    let heads = writer.get_heads();
    writer.put(ROOT, key, 1).unwrap();
    writer.commit();
    writer.save_after(&heads)
}

/// The copy stands where the writer does throughout, and once read is the
/// writer's document; a change it cannot wait for (one it holds) is applied
/// at once, and a malformed one leaves it as it was.
#[test]
fn a_deferred_copy_stands_where_its_writer_does_and_applies_where_read() {
    let isolate = fresh_isolate();
    let mut writer = AutoCommit::new().with_actor(ActorId::from(vec![0x52; 16]));
    writer.put(ROOT, "seed", "resident").unwrap();
    writer.commit();
    let mut copy = Deferred::from(writer.clone());
    let before = writer.get_heads();
    for round in 0..12_i64 {
        let heads = writer.get_heads();
        writer.put(ROOT, format!("k{round}"), round).unwrap();
        writer.commit();
        let made = writer
            .put_object(ROOT, format!("m{round}"), ObjType::Map)
            .unwrap();
        writer.put(&made, "inside", round).unwrap();
        writer.commit();
        let change = writer.save_after(&heads);
        assert_eq!(
            isolate.admit(&mut copy, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        let mut want = writer.get_heads();
        let mut have = copy.heads();
        want.sort_unstable();
        have.sort_unstable();
        assert_eq!(have, want, "round {round}");
        assert!(want.iter().all(|head| copy.holds(head)));
    }
    // Nothing was applied to it yet.
    assert_eq!(copy.unsettled().0.clone().get_heads(), before);

    // What it already holds is applied at once, and changes nothing.
    let held = writer.save_after(&before);
    assert_eq!(
        isolate.admit(&mut copy, &held, Dependencies::RequireComplete),
        Ok(Applied::Changed(false))
    );
    assert!(copy.unsettled().1.is_empty());
    assert_eq!(copy.settled().unwrap().save(), writer.save());

    // A malformed change is refused and leaves what waits as it was.
    let good = written(&mut writer, "last");
    assert_eq!(
        isolate.admit(&mut copy, &good, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    let mut bad = written(&mut writer.clone(), "broken");
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    assert!(
        isolate
            .admit(&mut copy, &bad, Dependencies::RequireComplete)
            .is_err()
    );
    assert_eq!(copy.settled().unwrap().save(), writer.save());

    // A worker that does not hold the copy's state is given it from the
    // document, settled first.
    let waiting = written(&mut writer, "waits");
    assert_eq!(
        isolate.admit(&mut copy, &waiting, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    let elsewhere = fresh_isolate();
    let change = written(&mut writer, "elsewhere");
    assert_eq!(
        elsewhere.admit(&mut copy, &change, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(copy.settled().unwrap().save(), writer.save());
}

/// A writer's own changes wait in the copy unapplied, and a worker process applies them only
/// once the copy is read: the copy stands where the writer does throughout, reads as the
/// writer's document, takes changes admitted the usual way after them, and refuses a change
/// that does not decode, as it was.
#[test]
fn a_writers_own_changes_wait_unapplied_until_the_copy_is_read() {
    let isolate = fresh_isolate();
    let mut writer = AutoCommit::new().with_actor(ActorId::from(vec![0x57; 16]));
    writer.put(ROOT, "seed", "resident").unwrap();
    writer.commit();
    let mut copy = Deferred::from(writer.clone());
    let before = writer.get_heads();
    for round in 0..8_i64 {
        let change = written(&mut writer, &format!("own{round}"));
        assert_eq!(copy.admit_unapplied(&change), Ok(Applied::Changed(true)));
        let (mut want, mut have) = (writer.get_heads(), copy.heads());
        want.sort_unstable();
        have.sort_unstable();
        assert_eq!(have, want, "round {round}");
    }
    assert_eq!(copy.unsettled().0.clone().get_heads(), before);

    let mut bad = written(&mut writer.clone(), "broken");
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    assert!(copy.admit_unapplied(&bad).is_err());

    let checked = written(&mut writer, "checked");
    assert_eq!(
        isolate.admit(&mut copy, &checked, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(copy.settled().unwrap().save(), writer.save());
}

/// A copy holding a writer's own changes unapplied takes another's change the usual way, in the
/// isolate every copy shares: what waits is applied first, never by asking a worker of the pool
/// while one of its workers is held for the change, which waited for itself for good.
#[test]
fn a_change_after_unapplied_ones_never_waits_for_the_pool_it_holds() {
    let finished = std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
    let done = std::sync::Arc::clone(&finished);
    std::thread::spawn(move || {
        let mut writer = AutoCommit::new().with_actor(ActorId::from(vec![0x58; 16]));
        writer.put(ROOT, "seed", "resident").unwrap();
        writer.commit();
        let mut copy = Deferred::from(writer.clone());
        for round in 0..8_i64 {
            let own = written(&mut writer, &format!("own{round}"));
            assert_eq!(copy.admit_unapplied(&own), Ok(Applied::Changed(true)));
            let other = written(&mut writer, &format!("other{round}"));
            assert_eq!(
                Isolate::shared().admit(&mut copy, &other, Dependencies::RequireComplete),
                Ok(Applied::Changed(true)),
                "round {round}"
            );
        }
        *done.0.lock().unwrap() = Some(copy.settled().unwrap().save() == writer.save());
        done.1.notify_all();
    });
    let (read, waited) = finished
        .1
        .wait_timeout_while(
            finished.0.lock().unwrap(),
            tilo_test_env::hang_bound(),
            |read| read.is_none(),
        )
        .unwrap();
    assert!(
        !waited.timed_out(),
        "every change is admitted, none waiting for the pool it holds"
    );
    assert_eq!(*read, Some(true), "the copy reads as the writer's document");
}

/// Another writer's change that arrived before what it builds on waits in the copy, and is no
/// reason to refuse a writer's own change made on what the copy holds: the own change is taken,
/// and the other one is applied once what it waits for comes. A change that itself builds on
/// what the copy lacks is still refused, and leaves the copy as it was.
#[test]
fn a_writers_own_change_is_taken_beside_another_waiting_for_its_base() {
    let mut writer = AutoCommit::new().with_actor(ActorId::from(vec![0x59; 16]));
    writer.put(ROOT, "seed", "resident").unwrap();
    writer.commit();
    let mut copy = Deferred::from(writer.clone());

    // Another member writes twice; only their second change has reached the copy.
    let mut other = writer.fork().with_actor(ActorId::from(vec![0x60; 16]));
    let first = written(&mut other, "other first");
    let second = written(&mut other, "other second");
    assert_eq!(
        copy.admit(&second, Dependencies::RetainOrphans),
        Ok(Applied::Changed(false))
    );

    // The writer's own change, made on what the copy holds, is taken.
    let own = written(&mut writer, "own");
    assert_eq!(copy.admit_unapplied(&own), Ok(Applied::Changed(true)));

    // One that builds on a change the copy never got is refused, and the copy keeps what it had.
    let mut ahead = writer.fork().with_actor(ActorId::from(vec![0x61; 16]));
    let _never_sent = written(&mut ahead, "never sent");
    let after = written(&mut ahead, "after it");
    let before = copy.settled().unwrap().save();
    assert_eq!(
        copy.admit_unapplied(&after),
        Ok(Applied::MissingDependencies)
    );
    assert_eq!(copy.settled().unwrap().save(), before);

    // The other member's first change arrives: both of theirs are in, beside the writer's own.
    assert_eq!(
        copy.admit(&first, Dependencies::RetainOrphans),
        Ok(Applied::Changed(true))
    );
    for change in [&first, &second] {
        Isolate::shared()
            .apply(&mut writer, change, Dependencies::RequireComplete)
            .unwrap();
    }
    let (mut want, mut have) = (writer.get_heads(), copy.heads());
    want.sort_unstable();
    have.sort_unstable();
    assert_eq!(have, want);
}
