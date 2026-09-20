#![allow(dead_code)]

#[cfg(sdk_api)]
use tilo_app::install_artifacts;
#[cfg(service_api)]
use tilo_install_artifacts as install_artifacts;

fn steal(client: &install_artifacts::Client) {
    client.consume();
}

fn main() {}
