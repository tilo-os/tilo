//! `tilo-boot-signin --device DEV --volume NAME`
//!
//! Exit 0: the volume's key file is in place. 2: nobody can sign in here, or
//! the secret could not be tried. 3: the person asked for another way in.
//! Anything but 0 leaves the existing prompts to open the volume.

use std::path::PathBuf;
use std::process::ExitCode;

use tilo_boot_signin::auth::CliAuth;
use tilo_boot_signin::flow::{self, Exit};
use tilo_boot_signin::screen::{BootScreen, Plymouth};

/// A program the initramfs runs, unless the environment names a stand-in.
fn program(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn main() -> ExitCode {
    let mut device = None;
    let mut volume = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--device") => device = args.next().map(PathBuf::from),
            Some("--volume") => volume = args.next().and_then(|v| v.into_string().ok()),
            _ => {
                eprintln!("usage: tilo-boot-signin --device DEV --volume NAME");
                return ExitCode::from(Exit::NothingToTry.code());
            }
        }
    }
    let (Some(device), Some(volume)) = (device, volume) else {
        eprintln!("usage: tilo-boot-signin --device DEV --volume NAME");
        return ExitCode::from(Exit::NothingToTry.code());
    };
    let mut auth = CliAuth {
        program: program("TILO_SIGNIN_AUTH", "tilo-auth"),
        device,
        volume: volume.clone(),
        runtime: program("TILO_SIGNIN_RUNTIME", "/run/tilo-signin"),
    };
    let mut screen = BootScreen {
        ask_password: program("TILO_SIGNIN_ASK_PASSWORD", "systemd-ask-password"),
        plymouth: Plymouth::running(program("TILO_SIGNIN_PLYMOUTH", "plymouth")),
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
