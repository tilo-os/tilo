//! The command line as `argparse` read it: options in any order, `--opt=VALUE`
//! or `--opt VALUE`, unique prefixes of long options, `--` ending the options,
//! `-h`/`--help`, and usage errors with exit status 2.

use std::ffi::OsString;

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
const OPTIONS: [&str; 4] = ["--esp-partuuid", "--boot-id", "--loader", "--help"];

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

fn usage_error(message: &str) -> String {
    format!("{USAGE}tilo-boot-order: error: {message}\n")
}

/// The long option `word` names: itself or its unique prefix.
fn long_option(word: &str) -> Result<Option<&'static str>, String> {
    if let Some(exact) = OPTIONS.iter().find(|option| **option == word) {
        return Ok(Some(exact));
    }
    let matches: Vec<&str> = OPTIONS
        .iter()
        .copied()
        .filter(|option| option.starts_with(word))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        many => Err(usage_error(&format!(
            "ambiguous option: {word} could match {}",
            many.join(", ")
        ))),
    }
}

/// Reads the arguments (without the program name).
///
/// # Errors
/// The usage and the error, as argparse wrote them, for exit status 2.
pub fn parse(args: &[OsString]) -> Result<Parsed, String> {
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut values: [Option<String>; 3] = [None, None, None];
    let mut positionals: Vec<String> = Vec::new();
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0usize;
    let mut options_done = false;
    while let Some(word) = words.get(index) {
        index = index.saturating_add(1);
        if options_done || !word.starts_with('-') || word == "-" {
            positionals.push(word.clone());
            continue;
        }
        if word == "--" {
            options_done = true;
            continue;
        }
        if word == "-h" {
            return Ok(Parsed::Help(format!("{USAGE}{HELP}")));
        }
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (word.as_str(), None),
        };
        let option = if name.starts_with("--") {
            long_option(name)?
        } else {
            None
        };
        let Some(option) = option else {
            unrecognized.push(word.clone());
            continue;
        };
        if option == "--help" {
            return Ok(Parsed::Help(format!("{USAGE}{HELP}")));
        }
        let value = if let Some(value) = inline {
            value
        } else {
            match words.get(index) {
                Some(next) if !next.starts_with('-') || next == "-" => {
                    index = index.saturating_add(1);
                    next.clone()
                }
                _ => {
                    return Err(usage_error(&format!(
                        "argument {option}: expected one argument"
                    )));
                }
            }
        };
        let slot = OPTIONS.iter().position(|o| *o == option).unwrap_or(0);
        if let Some(target) = values.get_mut(slot) {
            *target = Some(value);
        }
    }
    let mut rest = positionals.into_iter();
    let command = rest.next();
    if let Some(command) = &command
        && command != "enlist"
    {
        return Err(usage_error(&format!(
            "argument enlist: invalid choice: '{command}' (choose from enlist)"
        )));
    }
    let mut missing = Vec::new();
    if command.is_none() {
        missing.push("enlist");
    }
    for (value, name) in values.iter().zip(OPTIONS) {
        if value.is_none() {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(usage_error(&format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )));
    }
    unrecognized.extend(rest);
    if !unrecognized.is_empty() {
        return Err(usage_error(&format!(
            "unrecognized arguments: {}",
            unrecognized.join(" ")
        )));
    }
    let [Some(esp_partuuid), Some(boot_id), Some(loader)] = values else {
        return Err(usage_error("the following arguments are required"));
    };
    Ok(Parsed::Run(Request {
        esp_partuuid,
        boot_id,
        loader,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "a failed fixture call fails the test"
    )]

    use super::*;

    fn run(words: &[&str]) -> Result<Parsed, String> {
        parse(&words.iter().map(OsString::from).collect::<Vec<_>>())
    }

    fn request(words: &[&str]) -> Request {
        match run(words) {
            Ok(Parsed::Run(request)) => request,
            other => panic!("{words:?}: {other:?}"),
        }
    }

    #[test]
    fn options_in_any_order_with_prefixes_and_equals() {
        let expected = Request {
            esp_partuuid: "g".into(),
            boot_id: "0100".into(),
            loader: r"\EFI\x".into(),
        };
        assert_eq!(
            request(&[
                "enlist",
                "--esp-partuuid",
                "g",
                "--boot-id",
                "0100",
                "--loader",
                r"\EFI\x"
            ]),
            expected
        );
        assert_eq!(
            request(&["--loader", r"\EFI\x", "--esp=g", "enlist", "--boot", "0100"]),
            expected
        );
        assert_eq!(
            request(&[
                "--esp-partuuid=x",
                "--esp-partuuid=g",
                "--boot-id=0100",
                "--l",
                r"\EFI\x",
                "enlist"
            ]),
            expected
        );
        assert_eq!(
            request(&[
                "--esp-partuuid",
                "g",
                "--boot-id",
                "0100",
                "--loader",
                r"\EFI\x",
                "--",
                "enlist"
            ]),
            expected
        );
    }

    #[test]
    fn usage_errors() {
        let error = |words: &[&str]| run(words).unwrap_err();
        assert!(error(&[]).ends_with(
            "error: the following arguments are required: enlist, --esp-partuuid, --boot-id, --loader\n"
        ));
        assert!(
            error(&[
                "list",
                "--esp-partuuid",
                "g",
                "--boot-id",
                "1",
                "--loader",
                "l"
            ])
            .contains("invalid choice: 'list'")
        );
        assert!(
            error(&[
                "enlist",
                "--esp-partuuid",
                "g",
                "--boot-id",
                "1",
                "--loader",
                "l",
                "x",
                "--y"
            ])
            .ends_with("unrecognized arguments: --y x\n")
        );
        assert!(
            error(&["enlist", "--loader"]).ends_with("argument --loader: expected one argument\n")
        );
        assert!(
            error(&["enlist", "--loader", "--boot-id", "1"])
                .ends_with("argument --loader: expected one argument\n")
        );
        assert!(error(&["enlist"]).starts_with("usage: tilo-boot-order [-h]"));
    }

    #[test]
    fn help_wins() {
        assert!(matches!(run(&["--bogus", "-h"]), Ok(Parsed::Help(_))));
        assert!(matches!(run(&["--he"]), Ok(Parsed::Help(_))));
    }
}
