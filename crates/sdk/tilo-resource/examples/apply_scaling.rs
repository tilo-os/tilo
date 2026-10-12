//! What one keystroke into a record's draft costs as the record grows: the
//! median time of a one-field action at 10, 250 and 1,000 items, starting
//! from the record the previous action kept and starting cold.
//!
//! `cargo run --release -p tilo-resource --example apply_scaling`

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::print_stdout,
    reason = "a benchmark that prints its measurements"
)]

use std::time::{Duration, Instant};

use tilo_resource::__private::{ROOT, forget_all};
use tilo_resource::{Collection, Fields, Id, Model, Replica, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Task {
    pub title: String,
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "example.apply-scaling", label = "Checklist")]
impl Checklist {
    /// Type into the draft.
    #[action]
    pub fn draft(&mut self, text: String) {
        self.draft = text;
    }
}

const SAMPLES: usize = 41;

fn median_draft_action(items: usize, warm: bool) -> Duration {
    let mut replica = Replica::<Checklist>::new();
    let mut record = Checklist::default();
    for n in 0..items {
        record.tasks.push(
            Id::from_raw(format!("task-{n:05}")),
            Task {
                title: format!("task number {n}"),
                done: n % 3 == 0,
            },
        );
    }
    record.write_fields(replica.doc_mut(), &ROOT).unwrap();
    replica.doc_mut().commit();
    let mut draft = String::new();
    let mut samples = Vec::with_capacity(SAMPLES);
    for n in 0..=SAMPLES {
        draft.push(char::from(b'a' + u8::try_from(n % 26).unwrap()));
        if !warm {
            forget_all();
        }
        let start = Instant::now();
        replica.apply(Checklist::DRAFT.with(draft.clone())).unwrap();
        // The first action reads the record whole either way.
        if n > 0 {
            samples.push(start.elapsed());
        }
    }
    samples.sort();
    samples[samples.len() / 2]
}

fn main() {
    println!("{:>6}  {:>12}  {:>12}", "items", "kept (ms)", "cold (ms)");
    for items in [10, 250, 1_000] {
        let warm = median_draft_action(items, true);
        let cold = median_draft_action(items, false);
        println!(
            "{items:>6}  {:>12.3}  {:>12.3}",
            warm.as_secs_f64() * 1e3,
            cold.as_secs_f64() * 1e3
        );
    }
}
