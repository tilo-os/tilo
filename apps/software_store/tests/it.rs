// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "async_nonblocking.rs"]
mod async_nonblocking;
#[path = "shots_smoke.rs"]
mod shots_smoke;
