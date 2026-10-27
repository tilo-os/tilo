//! The verbs `tilo` hands to the program that owns them: `device`'s
//! development, identity and rollback-protection verbs, every `package`
//! verb, and `agent models`. Only the names and the help are `tilo`'s; each
//! program keeps its own arguments, its own `--help` and its own exit code.
//!
//! A verb whose program this computer doesn't have is left out of help, and
//! typed anyway answers with one sentence.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Arg, ArgMatches};

/// One verb another program owns.
pub struct Verb {
    pub name: &'static str,
    /// The program's name; see [`program`] for where it is looked for.
    pub program: &'static str,
    /// What goes before the person's arguments: the program's own verb.
    pub prefix: &'static [&'static str],
    pub about: &'static str,
    /// What the verb needs, for the sentence when its program is missing.
    pub needs: &'static str,
}

const VM: &str = "Tilo's development tools";

/// `tilo device …` beyond the system's own verbs.
pub const DEVICE: &[Verb] = &[
    Verb {
        name: "list",
        program: "tilo-vm",
        prefix: &["vm", "list"],
        about: "Your development devices and their state",
        needs: VM,
    },
    Verb {
        name: "rollback-protection",
        program: "tilo-rollback",
        prefix: &[],
        about: "Which older systems may still unlock this device's disk",
        needs: "a TPM-sealed disk",
    },
    Verb {
        name: "enroll",
        program: "tilo-enroll",
        prefix: &[],
        about: "Certify this device as one of yours, or approve another",
        needs: "Tilo's device enrollment",
    },
    Verb {
        name: "unlock-from",
        program: "tilo-sibling-unlock",
        prefix: &[],
        about: "Unlock this device's disk with another of your devices",
        needs: "an encrypted disk set up for it",
    },
    Verb {
        name: "swap",
        program: "tilo-vm",
        prefix: &["swap"],
        about: "Put a build of a component or app onto a device",
        needs: VM,
    },
    Verb {
        name: "slot",
        program: "tilo-vm",
        prefix: &["slot"],
        about: "Your slot on a development device: its user, session and builds",
        needs: VM,
    },
    Verb {
        name: "logs",
        program: "tilo-vm",
        prefix: &["logs"],
        about: "A slot's session journal",
        needs: VM,
    },
    Verb {
        name: "shot",
        program: "tilo-vm",
        prefix: &["shot"],
        about: "Save a picture of a slot's screen",
        needs: VM,
    },
    Verb {
        name: "input",
        program: "tilo-vm",
        prefix: &["input"],
        about: "Type, point, touch and draw on a slot's own seat",
        needs: VM,
    },
    Verb {
        name: "control",
        program: "tilo-vm",
        prefix: &["control"],
        about: "Drive a slot's session through its actions",
        needs: VM,
    },
    Verb {
        name: "scenario",
        program: "tilo-vm",
        prefix: &["scenario"],
        about: "Cold-boot a throwaway device to test what swapping can't replace",
        needs: VM,
    },
    Verb {
        name: "image",
        program: "tilo-vm",
        prefix: &["image"],
        about: "Build this checkout's development system",
        needs: VM,
    },
    Verb {
        name: "gc",
        program: "tilo-vm",
        prefix: &["gc"],
        about: "Delete builds and generations nothing needs any more",
        needs: VM,
    },
    Verb {
        name: "doctor",
        program: "tilo-vm",
        prefix: &["doctor"],
        about: "Check what development needs, with a fix for each failure",
        needs: VM,
    },
    Verb {
        name: "guide",
        program: "tilo-vm",
        prefix: &["guide"],
        about: "How developing on a device works",
        needs: VM,
    },
    Verb {
        name: "vm",
        program: "tilo-vm",
        prefix: &["vm"],
        about: "Create, start and stop development virtual machines",
        needs: VM,
    },
];

const PACKAGING: &str = "Tilo's packaging tools";

const fn packaging(
    name: &'static str,
    prefix: &'static [&'static str],
    about: &'static str,
) -> Verb {
    Verb {
        name,
        program: "tilo-app-package",
        prefix,
        about,
        needs: PACKAGING,
    }
}

/// `tilo package …`: making and signing packages, for people who make apps.
pub const PACKAGE: &[Verb] = &[
    packaging(
        "pack",
        &["pack"],
        "Pack a directory and its manifest into a signed .tapp",
    ),
    packaging(
        "pack-desktop",
        &["pack-desktop"],
        "Pack an app's Linux, Windows or macOS build",
    ),
    packaging(
        "sign",
        &["sign"],
        "Sign a .tapp again with another identity",
    ),
    packaging(
        "verify",
        &["verify"],
        "Check every signature and hash in a .tapp",
    ),
    packaging("inspect", &["inspect"], "Show a .tapp's manifest and files"),
    packaging(
        "unpack",
        &["unpack"],
        "Extract a .tapp's files and manifest",
    ),
    packaging("keygen", &["keygen"], "Make a publisher identity"),
    packaging("attest", &["attest"], "Vouch for a package as an attestor"),
    packaging("thin", &["thin"], "Write a .tapp's metadata-only form"),
    packaging(
        "split",
        &["split"],
        "Split a .tapp into the files a repository hosts",
    ),
    packaging("compress", &["compress"], "Write a .tapp's compressed twin"),
    packaging(
        "entry",
        &["entry"],
        "Print a .tapp's signed repository entry",
    ),
    packaging(
        "bot-entry",
        &["bot-entry"],
        "Print a bot's signed repository entry",
    ),
    packaging(
        "pending",
        &["pending"],
        "Print the entry a publisher has waiting for review",
    ),
    packaging(
        "publish",
        &["publish"],
        "Publish a .tapp to a repository directory",
    ),
    packaging(
        "seal-image",
        &["seal-image"],
        "Seal a directory into a signed environment image",
    ),
    Verb {
        name: "runtime",
        program: "tilo-runtime-bundle",
        prefix: &[],
        about: "Build, sign and verify runtime bundles",
        needs: PACKAGING,
    },
    Verb {
        name: "test-registry",
        program: "tilo-test-registry",
        prefix: &[],
        about: "Serve a throwaway repository for testing updates",
        needs: PACKAGING,
    },
];

/// Where `name` is: Tilo's own program directory, beside this program (a
/// development build), or on `PATH`.
pub fn program(name: &str) -> Option<PathBuf> {
    let libexec = crate::aliases::libexec().join(name);
    if crate::executable_file(&libexec) {
        return Some(libexec);
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|path| crate::executable_file(path));
    if sibling.is_some() {
        return sibling;
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| crate::executable_file(path))
    })
}

/// Whether this computer has `name`.
pub fn present(name: &str) -> bool {
    program(name).is_some()
}

/// `verbs` as subcommands of `noun`: each takes any arguments and hands
/// them on, `--help` included. The ones `has` says are missing are hidden.
pub fn with_verbs(noun: clap::Command, verbs: &[Verb], has: fn(&str) -> bool) -> clap::Command {
    verbs.iter().fold(noun, |noun, verb| {
        noun.subcommand(
            clap::Command::new(verb.name)
                .about(verb.about)
                .disable_help_flag(true)
                .hide(!has(verb.program))
                .arg(
                    Arg::new("args")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .value_parser(clap::value_parser!(OsString)),
                ),
        )
    })
}

/// Whether any of `verbs` works here.
pub fn any_present(verbs: &[Verb], has: fn(&str) -> bool) -> bool {
    verbs.iter().any(|verb| has(verb.program))
}

/// The verb `matches` (a noun's matches) picked, if it is one of `verbs`,
/// with the arguments to hand on.
pub fn picked<'a>(matches: &ArgMatches, verbs: &'a [Verb]) -> Option<(&'a Verb, Vec<OsString>)> {
    let (name, sub) = matches.subcommand()?;
    let verb = verbs.iter().find(|verb| verb.name == name)?;
    let args = sub
        .get_many::<OsString>("args")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    Some((verb, args))
}

/// Run `verb` with `args`; its output and exit code are the person's.
pub fn run(verb: &Verb, args: &[OsString]) -> Result<ExitCode> {
    let Some(path) = program(verb.program) else {
        bail!(
            "`{}` needs {}, which this computer doesn't have",
            verb.name,
            verb.needs
        );
    };
    let status = Command::new(&path)
        .args(verb.prefix)
        .args(args)
        .status()
        .with_context(|| format!("running {}", path.display()))?;
    Ok(crate::exit_code(status.code()))
}
