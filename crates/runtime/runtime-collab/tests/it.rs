// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "mock_broker.rs"]
mod mock_broker;
#[path = "schema_v2.rs"]
mod schema_v2;
