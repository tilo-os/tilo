// Test-only on its own, so the packaging scan of resource kinds, which
// reads each file alone, does not take its probe kind for the SDK's.
#![cfg(test)]

use super::*;
use tilo_resource::{Model, Stamped, resource};

#[derive(Model, Default, Debug, Clone)]
pub struct Tally {
    pub count: i64,
}

#[resource(kind = "test.tally", label = "Tally")]
impl Tally {
    /// Set the count.
    #[action]
    pub fn set(&mut self, count: i64) {
        self.count = count;
    }
}

fn live() -> Live<Tally> {
    let mut doc = tilo_resource::Doc::new();
    Tally::initialize(&mut doc).unwrap();
    Live::new(Reader::<Tally>::read(&doc).unwrap())
}

fn set(count: i64) -> <Tally as Resource>::Action {
    Stamped {
        operation: TallyAction::Set(tally::Set { count }),
        now_ms: 0,
    }
}

fn counts(applied: &[<Tally as Resource>::Action]) -> Vec<i64> {
    applied
        .iter()
        .filter_map(|action| match &action.operation {
            TallyAction::Set(tally::Set { count }) => Some(*count),
            TallyAction::FieldEdit(_) => None,
        })
        .collect()
}

#[test]
fn a_surface_reads_what_was_applied_here_since_it_asked_last() {
    let live = live();
    let from = live.follow_applied();
    live.note_applied_here(set(1));
    live.note_applied_here(set(2));
    assert!(
        live.applied_since(from).0.is_empty(),
        "nothing is told before the state holds it"
    );
    live.place_landings();
    let (applied, next) = live.applied_since(from);
    assert_eq!(counts(&applied), [1, 2]);
    live.note_applied_here(set(3));
    live.place_landings();
    let (applied, after) = live.applied_since(next);
    assert_eq!(counts(&applied), [3], "only what came after the last read");
    assert!(live.applied_since(after).0.is_empty());
}

#[test]
fn a_surface_that_missed_more_than_is_kept_reads_the_latest_in_order() {
    let live = live();
    let from = live.follow_applied();
    let total = i64::try_from(APPLIED_KEPT).unwrap() + 5;
    for count in 0..total {
        live.note_applied_here(set(count));
    }
    live.place_landings();
    let (applied, next) = live.applied_since(from);
    assert_eq!(applied.len(), APPLIED_KEPT);
    assert_eq!(counts(&applied).first(), Some(&5), "the oldest are dropped");
    assert_eq!(counts(&applied).last(), Some(&(total - 1)));
    assert_eq!(next, u64::try_from(total).unwrap());
    let (later, _) = live.applied_since(next - 2);
    assert_eq!(counts(&later), [total - 2, total - 1]);
}

#[test]
fn nothing_applied_here_is_kept_once_no_surface_follows() {
    let live = live();
    live.follow_applied();
    live.note_applied_here(set(1));
    live.place_landings();
    live.unfollow_applied();
    let from = live.follow_applied();
    assert!(live.applied_since(0).0.is_empty());
    assert_eq!(from, 1, "numbers go on where they were");
}
