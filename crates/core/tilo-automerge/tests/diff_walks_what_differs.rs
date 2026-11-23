//! A diff between two versions walks only the objects that differ between them and the objects
//! above those (vendor/automerge/TILO-PATCH.md): its patches are what crates.io automerge 0.12.0
//! gives for the same versions. What it costs is pinned where undo pays it, in collaboration's
//! `undo_of_a_small_step_costs_the_step_not_the_record`.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, ScalarValue};
use tilo_automerge::ApplyIncoming;

use crate::upstream::Rng;

const KEYS: &[&str] = &["a", "b", "k1", "k2"];

/// The objects made so far, by kind: maps, lists and texts. A replica that has not taken one in
/// yet refuses to write in it, and the edit is skipped.
#[derive(Default)]
struct Made {
    maps: Vec<ObjId>,
    lists: Vec<ObjId>,
    texts: Vec<ObjId>,
}

/// One random edit: values put and deleted, objects made under other objects and dropped with
/// them (so a diff backwards brings back whole objects), list inserts and deletes, typing,
/// counters.
fn edit(doc: &mut AutoCommit, made: &mut Made, rng: &mut Rng, step: usize) {
    let map = if made.maps.is_empty() || rng.below(3) == 0 {
        ROOT
    } else {
        rng.pick(&made.maps).clone()
    };
    let key = *rng.pick(KEYS);
    let _ = match rng.below(14) {
        0..=2 => doc.put(&map, key, i64::try_from(step).unwrap()),
        3 => doc.delete(&map, key),
        4 => doc
            .put_object(&map, key, ObjType::Map)
            .map(|obj| made.maps.push(obj)),
        5 => doc
            .put_object(&map, key, ObjType::List)
            .map(|obj| made.lists.push(obj)),
        6 => doc
            .put_object(&map, key, ObjType::Text)
            .map(|obj| made.texts.push(obj)),
        7 => doc.put(&map, "c", ScalarValue::counter(1)),
        8 => doc.increment(&map, "c", 2),
        9 | 10 if !made.lists.is_empty() => {
            let list = rng.pick(&made.lists).clone();
            let len = doc.length(&list);
            if len > 0 && rng.below(3) == 0 {
                doc.delete(&list, rng.below(len as u64) as usize)
            } else {
                doc.insert(
                    &list,
                    rng.below(len as u64 + 1) as usize,
                    i64::try_from(step).unwrap(),
                )
            }
        }
        _ if !made.texts.is_empty() => {
            let text = rng.pick(&made.texts).clone();
            let len = doc.length(&text);
            let at = rng.below(len as u64 + 1) as usize;
            doc.splice_text(&text, at, isize::from(at < len), "xy")
        }
        _ => doc.put(&map, key, "v"),
    };
}

fn upstream_heads(heads: &[ChangeHash]) -> Vec<upstream_automerge::ChangeHash> {
    heads
        .iter()
        .map(|hash| upstream_automerge::ChangeHash::try_from(hash.as_ref()).unwrap())
        .collect()
}

#[test]
fn a_diff_between_versions_is_what_upstream_gives() {
    for seed in 1..=12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut actors: Vec<AutoCommit> = (1..=3u8)
            .map(|i| AutoCommit::new().with_actor(ActorId::from(&[i][..])))
            .collect();
        let mut made = Made::default();
        let mut versions = vec![Vec::new()];
        for step in 0..120 {
            let a = rng.below(3) as usize;
            if rng.below(6) == 0 {
                let mut other = actors[(a + 1) % 3].fork();
                actors[a].merge_incoming(&mut other).unwrap();
            } else {
                for _ in 0..=rng.below(3) {
                    edit(&mut actors[a], &mut made, &mut rng, step);
                }
                actors[a].commit();
            }
            versions.push(actors[a].get_heads());
        }
        let (first, rest) = actors.split_at_mut(1);
        for other in rest {
            first[0].merge_incoming(other).unwrap();
        }
        let doc = &mut first[0];
        versions.push(doc.get_heads());
        let mut theirs = upstream_automerge::AutoCommit::load(&doc.save()).unwrap();
        for _ in 0..60 {
            let before = rng.pick(&versions).clone();
            let after = rng.pick(&versions).clone();
            let ours = format!("{:?}", doc.diff(&before, &after));
            let upstream = format!(
                "{:?}",
                theirs.diff(&upstream_heads(&before), &upstream_heads(&after))
            );
            assert_eq!(ours, upstream, "seed {seed}");
        }
    }
}
