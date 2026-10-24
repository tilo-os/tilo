//! `tilo-boot-signin --device DEV --volume NAME`
//!
//! Exit 0: the volume's key file is in place. 2: nobody can sign in here, or
//! the secret could not be tried. 3: the person asked for another way in.
//! Anything but 0 leaves the existing prompts to open the volume.
//!
//! `tilo-boot-signin other-ways --device DEV --pid PID`
//!
//! Carries the questions of the systemd-cryptsetup `PID` to the boot screen
//! in place of Plymouth's password agent (see [`tilo_boot_signin::relay`]).
//! Exit 0: that program is done. 3: the person asked to go back to signing
//! in. 2: Plymouth cannot ask; its own agent should.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tilo_boot_signin::auth::CliAuth;
use tilo_boot_signin::flow::{self, Exit};
use tilo_boot_signin::relay::{Relay, Relayed};
use tilo_boot_signin::screen::{BootScreen, Plymouth};

const USAGE: &str = "usage: tilo-boot-signin --device DEV --volume NAME\n       \
                     tilo-boot-signin other-ways --device DEV --pid PID";

/// A program the initramfs runs, unless the environment names a stand-in.
fn program(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(Exit::NothingToTry.code())
}

fn main() -> ExitCode {
    let mut other_ways = false;
    let mut device = None;
    let mut volume = None;
    let mut pid = None;
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().and_then(|arg| arg.to_str()) == Some("other-ways") {
        other_ways = true;
        args.next();
    }
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--device") => device = args.next().map(PathBuf::from),
            Some("--volume") => volume = args.next().and_then(|v| v.into_string().ok()),
            Some("--pid") => pid = args.next().and_then(|v| v.to_str()?.parse::<u32>().ok()),
            _ => return usage(),
        }
    }
    let plymouth = Plymouth::running(program("TILO_SIGNIN_PLYMOUTH", "plymouth"));
    if other_ways {
        let (Some(device), Some(pid)) = (device, pid) else {
            return usage();
        };
        let Some(plymouth) = plymouth else {
            return ExitCode::from(Relayed::SteppedAside.code());
        };
        let relayed = Relay {
            plymouth: &plymouth,
            ask_dir: program("TILO_SIGNIN_ASK_DIR", tilo_ask_password::ASK_DIR),
            device,
            pid,
            poll: Duration::from_millis(100),
        }
        .run();
        if relayed == Relayed::Back {
            eprintln!("tilo-boot-signin: back to signing in");
        }
        return ExitCode::from(relayed.code());
    }
    let (Some(device), Some(volume)) = (device, volume) else {
        return usage();
    };
    let mut auth = CliAuth {
        program: program("TILO_SIGNIN_AUTH", "tilo-auth"),
        device,
        volume: volume.clone(),
        runtime: program("TILO_SIGNIN_RUNTIME", "/run/tilo-signin"),
    };
    let mut screen = BootScreen {
        ask_password: program("TILO_SIGNIN_ASK_PASSWORD", "systemd-ask-password"),
        plymouth,
        volume,
    };
    let exit = flow::run(&mut auth, &mut screen);
    match exit {
        Exit::Unlocked => eprintln!("tilo-boot-signin: signed in; the volume opens with its key"),
        Exit::NothingToTry => eprintln!("tilo-boot-signin: no sign-in here; asking as before"),
        Exit::OtherWays => eprintln!("tilo-boot-signin: another way in was chosen"),
    }
    ExitCode::from(exit.code())
}
