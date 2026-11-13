//! Privileged helpers of the live installer and its backend (`tilo-install`):
//! account handoff, the handoff copy, dev SSH, the VM helpers, offline import,
//! staging handoff, install proof and code refresh.
//!
//! Each is a binary under `/usr/libexec` whose command line, checks and exit
//! statuses are part of the contract with `tilo-install`: 0 on success, 1 when
//! a check refuses or an operation fails (the message on stderr), 2 for a usage
//! error.

use std::ffi::OsString;
use std::fmt;
use std::process::ExitCode;

pub mod account_handoff;
pub use tilo_pycompat::argparse as args;
pub mod bootc_copy_cache;
pub mod code_refresh;
pub mod code_refresh_probe;
pub mod dev_ssh;
pub mod dev_target_uki;
pub mod handoff_copy;
pub mod ini;
pub mod install_proof;
pub mod offline_target;
pub mod process;
pub mod py;
pub mod staging_handoff;
pub mod target_transport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self(std::io::Error::from(error).to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(Error::new(message)) }
}

pub fn main(
    command: &args::Command,
    args: &[OsString],
    run: impl FnOnce(&args::Values) -> Result<()>,
) -> ExitCode {
    match args::parse(command, args) {
        Ok(args::Parsed::Help(text)) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(usage) => {
            eprint!("{usage}");
            ExitCode::from(2)
        }
        Ok(args::Parsed::Run(values)) => match run(&values) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{}: {error}", command.prog);
                ExitCode::FAILURE
            }
        },
    }
}
