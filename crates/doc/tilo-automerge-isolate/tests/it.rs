// One test binary instead of one per file; #[path] keeps the files in place.

// The worker an isolate starts is this binary, run as this test.
tilo_automerge_isolate::test_worker!();

#[path = "admission_cost.rs"]
mod admission_cost;
#[path = "isolation.rs"]
mod isolation;
