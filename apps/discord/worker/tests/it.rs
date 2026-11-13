// One test binary instead of one per file: each tests/*.rs linked this crate
// separately, and the bridge's the relay and its apps with it. #[path] keeps
// the files where they are, and nextest runs each test in its own process, so
// the isolation separate binaries gave them stays. The bridge's app and
// Automerge workers are this binary started again with their test's name as
// its filter, which matches that test under its module too.

#[path = "bridge.rs"]
mod bridge;
#[path = "connector.rs"]
mod connector;
#[path = "voice.rs"]
mod voice;
#[path = "worker.rs"]
mod worker;
