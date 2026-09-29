// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "mock_fs.rs"]
pub mod mock_fs;
#[path = "native_grants.rs"]
pub mod native_grants;
#[path = "native_operations.rs"]
pub mod native_operations;
#[path = "workspace_server.rs"]
pub mod workspace_server;
