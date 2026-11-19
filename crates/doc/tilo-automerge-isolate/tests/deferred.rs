//! A copy that admits a writer's changes as they come and applies them only
//! where it is read.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

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
