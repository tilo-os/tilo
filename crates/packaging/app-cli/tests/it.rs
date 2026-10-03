// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "channel_cli.rs"]
pub mod channel_cli;
#[path = "registry_to_installed.rs"]
pub mod registry_to_installed;
#[path = "version_index.rs"]
pub mod version_index;
