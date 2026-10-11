// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "history_and_agents.rs"]
pub mod history_and_agents;
#[path = "language_server.rs"]
pub mod language_server;
#[path = "windows_terminal.rs"]
pub mod windows_terminal;
