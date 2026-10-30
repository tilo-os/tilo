//! The command line as `argparse` read it.

use std::ffi::OsString;

use tilo_pycompat::argparse::{self, Arg, Command};

const USAGE: &str = "usage: tilo-boot-order [-h] --esp-partuuid ESP_PARTUUID --boot-id BOOT_ID\n                       --loader LOADER\n                       {enlist}\n";
const HELP: &str = "

Enlist a successfully booted Tilo after Windows without changing the default.
Run manually after checking the first Tilo boot. The supplied identities come
from the physical install readback and its armed-boot receipt, not discovery by
label. This command never creates or replaces an EFI boot option.

positional arguments:
  {enlist}

options:
  -h, --help            show this help message and exit
  --esp-partuuid ESP_PARTUUID
  --boot-id BOOT_ID
  --loader LOADER
";
const COMMAND: Command = Command {
    prog: "tilo-boot-order",
    usage: USAGE,
    help: HELP,
    args: &[
        Arg::Choice {
            name: "enlist",
            choices: &["enlist"],
        },
        Arg::Option {
            flag: "--esp-partuuid",
            required: true,
        },
        Arg::Option {
            flag: "--boot-id",
            required: true,
        },
        Arg::Option {
            flag: "--loader",
            required: true,
        },
    ],
};

/// A parsed `enlist` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub esp_partuuid: String,
    pub boot_id: String,
    pub loader: String,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Request),
    /// `-h`/`--help`: the text to print.
    Help(String),
}

/// Reads the arguments (without the program name).
///
/// # Errors
/// The usage and the error, as argparse wrote them, for exit status 2.
pub fn parse(args: &[OsString]) -> Result<Parsed, String> {
    let values = match argparse::parse(&COMMAND, args)? {
        argparse::Parsed::Help(text) => return Ok(Parsed::Help(text)),
        argparse::Parsed::Run(values) => values,
    };
    let text = |flag: &str| {
        values
            .required(flag)
            .map(|value| value.to_string_lossy().into_owned())
    };
    Ok(Parsed::Run(Request {
        esp_partuuid: text("--esp-partuuid")?,
        boot_id: text("--boot-id")?,
        loader: text("--loader")?,
    }))
}
