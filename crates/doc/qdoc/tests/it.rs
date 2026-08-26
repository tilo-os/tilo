// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "convergence.rs"]
mod convergence;
#[path = "office_roundtrip.rs"]
mod office_roundtrip;
#[path = "sheet_roundtrip.rs"]
mod sheet_roundtrip;
