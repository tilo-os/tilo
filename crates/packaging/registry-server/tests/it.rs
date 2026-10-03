// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "delegated_promotion_cli.rs"]
pub mod delegated_promotion_cli;
#[path = "full_entry.rs"]
pub mod full_entry;
#[path = "http_end_to_end.rs"]
pub mod http_end_to_end;
#[path = "promotion_cli.rs"]
pub mod promotion_cli;
#[path = "publisher_applications.rs"]
pub mod publisher_applications;
#[path = "publisher_client.rs"]
pub mod publisher_client;
#[path = "snapshot_cli.rs"]
pub mod snapshot_cli;
