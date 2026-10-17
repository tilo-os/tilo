//! Compile the shipped seed executable and its account transport without UI.
extern crate self as installer;

#[path = "../../../../session/installer/src/account_handoff.rs"]
pub mod account_handoff;
#[path = "../../../../session/installer/src/bin/tilo-firstboot-home-seeds.rs"]
mod seeds;

fn main() -> std::process::ExitCode {
    seeds::main()
}
