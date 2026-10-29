//! Compile the shipped seed executable and its account transport without UI.
extern crate self as imager;

// The shared installer modules below use these crates outside this package's
// source tree. Keep their linkage explicit at the executable's crate root.
extern crate account_custody;
#[cfg(test)]
extern crate accounts;
extern crate tilo_seal;
extern crate zeroize;

#[path = "../../../../../apps/imager/src/setup/destination/account_handoff.rs"]
pub mod account_handoff;
/// Where the seed executable names the transport in the Imager package.
pub mod setup {
    pub mod destination {
        pub use crate::account_handoff;
    }
}
#[path = "../../../../../apps/imager/src/bin/tilo-firstboot-home-seeds.rs"]
mod seeds;

fn main() -> std::process::ExitCode {
    seeds::main()
}
