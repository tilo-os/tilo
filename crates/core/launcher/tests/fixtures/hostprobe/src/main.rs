// Fixture for `tests/tilolang_process_confinement.rs`: it stands in for the tilolang host a
// `tilo-launcher tilolang-process` launch runs, and answers on the link it was handed each read
// or write it is asked to make: `OK`, or `ERR` and what the system answered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, Write};
use std::os::fd::FromRawFd;

/// Where the launcher names the link it placed (`runtime_portal::process_link::FD_ENV`).
const FD_ENV: &str = "TILO_SVC_PROCESS_FD";

fn main() {
    let fd: i32 = std::env::var(FD_ENV).unwrap().parse().unwrap();
    // SAFETY: the launcher placed the link at this descriptor for this process alone.
    let link = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    let mut answers = link.try_clone().unwrap();
    for line in std::io::BufReader::new(link).lines() {
        let line = line.unwrap();
        let (verb, path) = line.split_once(' ').expect("a verb and a path");
        let done = match verb {
            "read" => std::fs::read(path).map(drop),
            "write" => std::fs::write(path, b"written by the host"),
            // A service socket it was handed no descriptor for: the sandbox grants no path to
            // one, so connecting to it fails rather than reaching the service.
            "connect" => std::os::unix::net::UnixStream::connect(path).map(drop),
            other => panic!("no verb {other}"),
        };
        let answer = match done {
            Ok(()) => "OK".to_owned(),
            Err(error) => format!("ERR {:?}", error.kind()),
        };
        writeln!(answers, "{answer}").unwrap();
    }
}
