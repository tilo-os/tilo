// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "launcher_smoke.rs"]
mod launcher_smoke;
#[path = "netgate_egress.rs"]
mod netgate_egress;
#[path = "room_confinement.rs"]
mod room_confinement;
#[path = "sandbox_environment.rs"]
mod sandbox_environment;
