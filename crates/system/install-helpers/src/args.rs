//! The command lines as `argparse` read them: long options in any order, as
//! `--opt VALUE` or `--opt=VALUE` or a unique prefix of either, `--` ending the
//! options, `-h`/`--help`, positionals in order, and usage errors (exit
//! status 2) worded as `argparse` worded them.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// One argument, in the order the script declared it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// `--flag VALUE`; `required` or optional (absent means its default).
    Option { flag: &'static str, required: bool },
    /// A positional, named in usage errors.
    Positional(&'static str),
}

/// A helper's command line.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    /// The name in usage errors and failure messages.
    pub prog: &'static str,
    /// The `usage: …` lines, each ending in a newline.
    pub usage: &'static str,
    /// What `--help` prints after the usage.
    pub help: &'static str,
    /// The arguments, in declaration order.
    pub args: &'static [Arg],
}

/// The parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Values {
    names: Vec<&'static str>,
    values: Vec<Option<OsString>>,
}

impl Values {
    /// The value of option `flag` or positional `name`, if given.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        let at = self.names.iter().position(|n| *n == name)?;
        self.values.get(at)?.as_deref()
    }

    /// The value of a required option or positional.
    ///
    /// # Errors
    /// Never for an argument the parser required; a missing one is a
    /// programming error reported as a refusal rather than a panic.
    pub fn required(&self, name: &str) -> crate::Result<&OsStr> {
        self.get(name).ok_or_else(|| {
            crate::Error::new(format!("the following arguments are required: {name}"))
        })
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Values),
    /// `-h`/`--help`: the text to print.
    Help(String),
}

fn usage_error(command: &Command, message: &str) -> String {
    format!("{}{}: error: {message}\n", command.usage, command.prog)
}

/// `argparse` takes `-5` and `-.5` for values, not options, when no option
/// looks like a number.
fn is_negative_number(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('-') else {
        return false;
    };
    let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if rest.contains('.') {
        digits(whole) && !fraction.is_empty() && digits(fraction)
    } else {
        !whole.is_empty() && digits(whole)
    }
}

fn looks_like_option(word: &str) -> bool {
    word.starts_with('-') && word != "-" && !is_negative_number(word) && !word.contains(' ')
}

/// The long option `word` names: itself or the one it is a unique prefix of.
fn long_option(command: &Command, word: &str) -> Result<Option<&'static str>, String> {
    let flags = command
        .args
        .iter()
        .filter_map(|arg| match arg {
            Arg::Option { flag, .. } => Some(*flag),
            Arg::Positional(_) => None,
        })
        .chain(std::iter::once("--help"));
    let flags: Vec<&'static str> = flags.collect();
    if let Some(exact) = flags.iter().find(|flag| **flag == word) {
        return Ok(Some(exact));
    }
    let matches: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|flag| flag.starts_with(word))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        many => Err(usage_error(
            command,
            &format!("ambiguous option: {word} could match {}", many.join(", ")),
        )),
    }
}

/// One word's effect on the parse.
enum Step {
    /// Print the help.
    Help,
    /// The word was read; `true` when it also took the next word as a value.
    Read(bool),
}

/// The parse so far.
struct State<'a> {
    command: &'a Command,
    names: Vec<&'static str>,
    values: Vec<Option<OsString>>,
    positionals: Vec<usize>,
    next_positional: usize,
    unrecognized: Vec<String>,
    options_done: bool,
}

impl<'a> State<'a> {
    fn new(command: &'a Command) -> Self {
        let names: Vec<&'static str> = command
            .args
            .iter()
            .map(|arg| match arg {
                Arg::Option { flag, .. } => *flag,
                Arg::Positional(name) => *name,
            })
            .collect();
        let positionals: Vec<usize> = command
            .args
            .iter()
            .enumerate()
            .filter_map(|(at, arg)| matches!(arg, Arg::Positional(_)).then_some(at))
            .collect();
        Self {
            command,
            values: vec![None; names.len()],
            names,
            positionals,
            next_positional: 0,
            unrecognized: Vec::new(),
            options_done: false,
        }
    }

    fn set(&mut self, name: &str, value: OsString) {
        if let Some(slot) = self.names.iter().position(|n| *n == name)
            && let Some(target) = self.values.get_mut(slot)
        {
            *target = Some(value);
        }
    }

    /// Reads `raw`; `next` is the word after it, which an option may take.
    fn step(&mut self, raw: &OsString, next: Option<&OsString>) -> Result<Step, String> {
        let word = raw.to_string_lossy();
        if self.options_done || !looks_like_option(&word) {
            if let Some(slot) = self.positionals.get(self.next_positional).copied() {
                self.next_positional = self.next_positional.saturating_add(1);
                if let Some(value) = self.values.get_mut(slot) {
                    *value = Some(raw.clone());
                }
            } else {
                self.unrecognized.push(word.into_owned());
            }
            return Ok(Step::Read(false));
        }
        if word == "--" {
            self.options_done = true;
            return Ok(Step::Read(false));
        }
        if word == "-h" {
            return Ok(Step::Help);
        }
        let bytes = raw.as_bytes();
        let (name, inline) = match bytes.iter().position(|b| *b == b'=') {
            Some(equals) if word.starts_with("--") => {
                let name = bytes.get(..equals).unwrap_or_default();
                let value = bytes.get(equals.saturating_add(1)..).unwrap_or_default();
                (
                    String::from_utf8_lossy(name).into_owned(),
                    Some(OsStr::from_bytes(value).to_owned()),
                )
            }
            _ => (word.clone().into_owned(), None),
        };
        let option = if name.starts_with("--") {
            long_option(self.command, &name)?
        } else {
            None
        };
        let Some(option) = option else {
            self.unrecognized.push(word.into_owned());
            return Ok(Step::Read(false));
        };
        if option == "--help" {
            return Ok(Step::Help);
        }
        if let Some(value) = inline {
            self.set(option, value);
            return Ok(Step::Read(false));
        }
        match next {
            Some(next) if !looks_like_option(&next.to_string_lossy()) => {
                self.set(option, next.clone());
                Ok(Step::Read(true))
            }
            _ => Err(usage_error(
                self.command,
                &format!("argument {option}: expected one argument"),
            )),
        }
    }

    fn finish(self) -> Result<Parsed, String> {
        let missing: Vec<&str> = self
            .command
            .args
            .iter()
            .zip(&self.values)
            .filter_map(|(arg, value)| match arg {
                Arg::Option {
                    flag,
                    required: true,
                } if value.is_none() => Some(*flag),
                Arg::Positional(name) if value.is_none() => Some(*name),
                _ => None,
            })
            .collect();
        if !missing.is_empty() {
            return Err(usage_error(
                self.command,
                &format!(
                    "the following arguments are required: {}",
                    missing.join(", ")
                ),
            ));
        }
        if !self.unrecognized.is_empty() {
            return Err(usage_error(
                self.command,
                &format!("unrecognized arguments: {}", self.unrecognized.join(" ")),
            ));
        }
        Ok(Parsed::Run(Values {
            names: self.names,
            values: self.values,
        }))
    }
}

/// Reads `args` (without the program name).
///
/// # Errors
/// The usage and the error, as `argparse` wrote them, for exit status 2.
pub fn parse(command: &Command, args: &[OsString]) -> Result<Parsed, String> {
    let mut state = State::new(command);
    let mut index = 0usize;
    while let Some(raw) = args.get(index) {
        index = index.saturating_add(1);
        match state.step(raw, args.get(index))? {
            Step::Help => {
                return Ok(Parsed::Help(format!("{}{}", command.usage, command.help)));
            }
            Step::Read(took_value) => {
                if took_value {
                    index = index.saturating_add(1);
                }
            }
        }
    }
    state.finish()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "a failed fixture call fails the test"
    )]

    use super::*;

    const COMMAND: Command = Command {
        prog: "tool",
        usage: "usage: tool [-h] --etc ETC [--name NAME] source\n",
        help: "\noptions:\n",
        args: &[
            Arg::Option {
                flag: "--etc",
                required: true,
            },
            Arg::Option {
                flag: "--name",
                required: false,
            },
            Arg::Positional("source"),
        ],
    };

    fn run(words: &[&str]) -> Result<Parsed, String> {
        parse(
            &COMMAND,
            &words.iter().map(OsString::from).collect::<Vec<_>>(),
        )
    }

    fn values(words: &[&str]) -> Values {
        match run(words) {
            Ok(Parsed::Run(values)) => values,
            other => panic!("{words:?}: {other:?}"),
        }
    }

    #[test]
    fn options_in_any_order_with_prefixes_and_equals() {
        for words in [
            &["--etc", "/e", "src"][..],
            &["src", "--etc=/e"],
            &["--e", "/e", "src"],
            &["--etc=/x", "--et=/e", "--", "src"],
        ] {
            let parsed = values(words);
            assert_eq!(parsed.get("--etc"), Some(OsStr::new("/e")), "{words:?}");
            assert_eq!(parsed.get("source"), Some(OsStr::new("src")), "{words:?}");
            assert_eq!(parsed.get("--name"), None);
        }
        assert_eq!(
            values(&["--etc", "-5", "--name", "", "-"]).get("--name"),
            Some(OsStr::new(""))
        );
    }

    #[test]
    fn usage_errors() {
        let error = |words: &[&str]| run(words).unwrap_err();
        assert_eq!(
            error(&[]),
            "usage: tool [-h] --etc ETC [--name NAME] source\n\
             tool: error: the following arguments are required: --etc, source\n"
        );
        assert!(
            error(&["--etc"]).ends_with("tool: error: argument --etc: expected one argument\n")
        );
        assert!(
            error(&["--etc", "--name", "x", "s"])
                .ends_with("argument --etc: expected one argument\n")
        );
        assert!(
            error(&["--etc", "e", "s", "t", "--bogus"])
                .ends_with("unrecognized arguments: t --bogus\n")
        );
        assert!(
            error(&["--etc", "e", "--bogus"])
                .ends_with("the following arguments are required: source\n")
        );
    }

    #[test]
    fn help_wins() {
        assert!(matches!(run(&["--bogus", "-h"]), Ok(Parsed::Help(_))));
        assert!(matches!(run(&["--he"]), Ok(Parsed::Help(_))));
    }
}
