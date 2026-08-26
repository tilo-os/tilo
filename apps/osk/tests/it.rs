// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "render_smoke.rs"]
mod render_smoke;
#[path = "shots_smoke.rs"]
mod shots_smoke;
