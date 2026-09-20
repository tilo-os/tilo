#![allow(dead_code)]

#[cfg(sdk_api)]
use tilo_app::install_artifacts;
#[cfg(service_api)]
use tilo_install_artifacts as install_artifacts;

use install_artifacts::ReleaseSelection;

fn forge() -> ReleaseSelection {
    ReleaseSelection {
        release: panic!("compile-only private-constructor probe"),
    }
}

fn main() {}
