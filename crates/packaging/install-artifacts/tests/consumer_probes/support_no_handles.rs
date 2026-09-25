#![allow(dead_code)]

use tilo_install_artifacts::test_support::Server;

fn steal(server: &Server) {
    server.transport();
}

fn main() {}
