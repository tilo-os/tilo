// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "atspi_acceptance.rs"]
pub mod atspi_acceptance;
#[path = "end_to_end.rs"]
pub mod end_to_end;
#[path = "one_photograph_one_address.rs"]
pub mod one_photograph_one_address;
