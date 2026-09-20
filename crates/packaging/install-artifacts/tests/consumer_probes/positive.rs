#![allow(dead_code)]

#[cfg(sdk_api)]
use tilo_app::install_artifacts;
#[cfg(service_api)]
use tilo_install_artifacts as install_artifacts;

use install_artifacts::{Client, Error, ReleaseSelection, Status, Token};

fn api(client: &Client, release: ReleaseSelection, bytes: Vec<u8>) -> Result<Token, Error> {
    client.start(release, bytes)
}

fn main() {
    let _ = api as fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error>;
    let _ = Client::status as fn(&Client) -> Option<Status>;
    let _ = Client::preference as fn(&Client) -> install_artifacts::PreferenceState;
    let _ = Client::resume as fn(&Client, ReleaseSelection) -> Result<Token, Error>;
}
