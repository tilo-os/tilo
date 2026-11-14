// One test binary instead of four: each tests/*.rs linked the whole app
// separately. #[path] keeps the files where they are, and nextest runs each
// test in its own process, so the isolation separate binaries gave them stays:
// the first window `perf_budget` times is still its process's first, and the
// environment it scopes is still its own.

#[path = "chrome_alignment.rs"]
mod chrome_alignment;
#[path = "perf_budget.rs"]
mod perf_budget;
#[path = "perf_interaction.rs"]
mod perf_interaction;
#[path = "private_launch.rs"]
mod private_launch;
