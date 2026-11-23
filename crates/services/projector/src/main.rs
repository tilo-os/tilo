//! Tilo screen-projection protocol broker.

mod cli;
mod discovery;
mod error;
mod http;
mod service;
mod session;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = cli::run(&args) {
        eprintln!("tilo-projector: {error}");
        std::process::exit(2);
    }
}
