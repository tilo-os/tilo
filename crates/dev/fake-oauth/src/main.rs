//! A stand-in for a provider's own sign-in page, on loopback: it approves
//! every sign-in, says the mailbox a local stack keeps, and prints the
//! address a server is pointed at, so a real browser can walk the whole
//! round trip with no real account.
//!
//! What it prints, and a stack's `--fake-oauth` reads:
//! `FAKE_OAUTH_URL=<origin>` (its authorize and token endpoints are
//! `/authorize` and `/token` there) and `FAKE_OAUTH_PROFILE=<url>`.
//! `--port N` listens on that loopback port, which a stack on another machine
//! forwards to the browser; without it any free port is taken.

use std::thread;

fn main() {
    let mut identity = "ada@example.test".to_owned();
    let mut port = 0;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match (argument.as_str(), args.next()) {
            ("--identity", Some(email)) => identity = email,
            ("--port", Some(number)) => {
                port = number.parse().unwrap_or_else(|_| {
                    eprintln!("fake OAuth: --port takes a port number, not {number}");
                    std::process::exit(2);
                });
            }
            _ => {}
        }
    }
    let fake = tilo_oauth::fake::FakeAuthorizationServer::start_at(port);
    fake.set_identity(&identity);
    println!("FAKE_OAUTH_URL={}", fake.url());
    println!("FAKE_OAUTH_PROFILE={}", fake.profile_url());
    println!("fake OAuth: signs in {identity} on every page; no real account is used");
    loop {
        thread::park();
    }
}
