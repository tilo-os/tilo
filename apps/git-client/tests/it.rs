// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "remote_transport.rs"]
pub mod remote_transport;
#[path = "repo_ops.rs"]
pub mod repo_ops;
