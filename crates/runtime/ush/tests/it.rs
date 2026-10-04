// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "host_serve.rs"]
pub mod host_serve;
#[path = "network.rs"]
pub mod network;
#[path = "serve_handoff.rs"]
pub mod serve_handoff;
#[path = "socket_activation.rs"]
pub mod socket_activation;
