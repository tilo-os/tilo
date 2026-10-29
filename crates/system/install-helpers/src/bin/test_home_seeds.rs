//! Compile the shipped seed executable and its account transport without UI.
extern crate self as installer;

// The shared installer modules below use these crates outside this package's
// source tree. Keep their linkage explicit at the executable's crate root.
extern crate account_custody;
#[cfg(test)]
extern crate accounts;
extern crate tilo_seal;
extern crate zeroize;

#[path = "../../../../session/installer/src/account_handoff.rs"]
pub mod account_handoff;
#[path = "../../../../session/installer/src/bin/tilo-firstboot-home-seeds.rs"]
mod seeds;

fn main() -> std::process::ExitCode {
    seeds::main()
}
