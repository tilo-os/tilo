#![allow(dead_code)]

#[cfg(not(unix))]
compile_error!("the support consumer probe requires the Unix fixture facade");

use std::path::Path;
use tilo_install_artifacts::test_support::Server;
use tilo_install_artifacts::{Client, ReleaseSelection};

fn fixture_api(
    server: &Server,
    root: &Path,
    preference: &Path,
    manifest: &[u8],
) -> (ReleaseSelection, Client) {
    let selection = server.selection(manifest);
    let client = server.client(root, preference);
    (selection, client)
}

fn main() {
    let _ = fixture_api as fn(&Server, &Path, &Path, &[u8]) -> (ReleaseSelection, Client);
}
