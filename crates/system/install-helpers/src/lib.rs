//! Privileged helpers of the live installer and its backend (`tilo-install`).
//!
//! Each was a Python script under `/usr/libexec`; each is now a binary of this
//! crate at the same path without the `.py`, with the same command line, the
//! same checks in the same order and the same exit statuses: 0 on success, 1
//! when a check refuses or an operation fails (the message on stderr), 2 for a
//! usage error, as `argparse` reported it.
//!
//! * [`handoff_copy`]: `tilo-install-handoff-copy SOURCE DESTINATION`
//! * [`dev_ssh`]: `tilo-install-dev-ssh --etc ETC --username USERNAME --key KEY`
//! * [`bootc_copy_cache`]: `tilo-vm-bootc-copy-cache check|record` (VMs only)
//! * [`offline_target`]: `tilo-installer-offline-target verify|import`
//! * [`dev_target_uki`]: `tilo-installer-offline-target dev-target-uki
//!   validate-image|promote` (development installer VMs only)
//! * [`staging_handoff`]: `tilo-staging-handoff detect|check-install|…`
//! * [`install_proof`]: `tilo-vm-install-proof --target-root … --output OUTPUT`
//!   (VMs only)
//! * [`code_refresh`]: `tilo-vm-install-proof code-refresh`, the development
//!   VM's installer helper refresh daemon

use std::ffi::OsString;
use std::fmt;
use std::process::ExitCode;

pub mod args;
pub mod bootc_copy_cache;
pub mod code_refresh;
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

/// Why a helper refused or failed: the message it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    /// A refusal with this message.
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

/// A helper's result.
pub type Result<T> = std::result::Result<T, Error>;

/// `require(ok, message)`: refuses with `message` unless `ok`.
///
/// # Errors
/// `message`, when `ok` is false.
pub fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(Error::new(message)) }
}

/// Runs one helper's `main`: parses `args` (without the program name) against
/// `command`, then `run`; prints help, usage errors and failures as the script
/// did, and returns its exit status.
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
