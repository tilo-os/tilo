#![allow(dead_code)]

#[cfg(service_api)]
use tilo_install_artifacts as install_artifacts;
#[cfg(sdk_api)]
use tilo_app::install_artifacts;

fn require_deserialize<T: serde::de::DeserializeOwned>() {}

fn main() {
    require_deserialize::<install_artifacts::ReleaseSelection>();
}
