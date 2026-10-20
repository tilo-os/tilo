// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "device_account.rs"]
pub mod device_account;
#[path = "device_owner.rs"]
pub mod device_owner;
#[path = "jobs.rs"]
pub mod jobs;
#[path = "peer_route.rs"]
pub mod peer_route;
#[path = "rpc.rs"]
pub mod rpc;
