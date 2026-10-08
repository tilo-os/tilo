//! `tilo-sibling-unlock`: unlock an encrypted root with another of the
//! owner's devices.
//!
//! * `probe DEVICE`: exit 0 when the volume carries a sibling-unlock token.
//! * `unlock DEVICE`: one early-boot attempt (the initramfs runs it as
//!   `tilo-sibling-unlock@.service`, started by tilo-root-discover).
//! * `enrol DEVICE`: add the sibling key slot and register it with the
//!   owner's devices (`tilo-disk-encrypt enrol --sibling` runs it).
//! * `remove DEVICE`: remove the slot and its token.
//! * `status DEVICE`: what is enrolled, without secrets.
//! * `volumes`: every volume enrolled on this machine, without secrets
//!   (Settings reads it through polkit; the description is root's alone).
//! * `replay TRANSCRIPT`: send a kept exchange's frames to its sibling again
//!   and print the answers (verification: a replay must be refused).
//!
//! Secrets never travel on `argv`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use tilo_sibling_unlock::exchange::{self, Peers, Progress};
use tilo_sibling_unlock::material::Material;
use tilo_sibling_unlock::token::Sibling;
use tilo_sibling_unlock::unlock::{self, Outcome, Volume};
use tilo_sibling_unlock::{enrol, luks, network, seal};

#[derive(Parser)]
#[command(
    name = "tilo-sibling-unlock",
    about = "Unlock an encrypted root with another of your devices"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Exit 0 when DEVICE carries a sibling-unlock token, 1 otherwise.
    Probe { device: PathBuf },
    /// Ask the owner's other devices to unlock DEVICE for this boot, and
    /// answer its passphrase prompt with the result.
    Unlock { device: PathBuf },
    /// Add a key slot opened with a sibling's consent, and register it with
    /// the owner's devices.
    Enrol(enrol::Args),
    /// Remove the sibling key slot and its token.
    Remove {
        device: PathBuf,
        /// A key file holding an existing passphrase of the volume; without
        /// one cryptsetup asks for it.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Print what is enrolled on DEVICE, without secrets.
    Status { device: PathBuf },
    /// Print every volume enrolled on this machine, without secrets.
    Volumes,
    /// Send the frames of a kept exchange to its siblings again and print
    /// what they answer.
    Replay {
        #[arg(default_value = exchange::TRANSCRIPT)]
        transcript: PathBuf,
    },
}

/// A line on the console (through the kernel log, which the initramfs
/// forwards there) and in the journal.
fn say(message: &str) {
    eprintln!("{message}");
    if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(kmsg, "<5>tilo-sibling-unlock: {message}");
    }
}

struct BootScreen;

impl Progress for BootScreen {
    fn pending(&self, sibling: &Sibling, approval_code: &str) {
        let name = if sibling.label.is_empty() {
            "your other device"
        } else {
            &sibling.label
        };
        let message = format!("Approve unlocking on {name}: code {approval_code}");
        say(&message);
        let _ = std::process::Command::new("plymouth")
            .args(["display-message", &format!("--text={message}")])
            .status();
    }
}

struct RealVolume<'a>(&'a Path);

impl Volume for RealVolume<'_> {
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool {
        luks::opens(self.0, keyslot, passphrase)
    }
}

async fn unlock(device: &Path) -> Result<ExitCode> {
    let Some(token) = luks::token(device)? else {
        return Ok(ExitCode::SUCCESS);
    };
    let cert = token.cert()?;
    if luks::uuid_bytes(device)? != cert.slot.volume_uuid {
        bail!("the sibling-unlock token belongs to another volume");
    }
    let material = Material::from_json(&seal::unseal(token.sealing, &token.material)?)?;
    let boot_id = unlock::boot_id(Path::new("/proc/sys/kernel/random/boot_id"))?;
    let _network = match network::Network::up(&material.networks) {
        Ok(network) => Some(network),
        Err(error) => {
            say(&format!("could not bring up the network: {error:#}"));
            None
        }
    };
    say("asking your other devices to unlock this disk");
    let peers = Peers {
        progress: &BootScreen,
        transcript: Some(Path::new(exchange::TRANSCRIPT)),
    };
    let ask_dir = unlock::ask_dir();
    let volume = RealVolume(device);
    let attempt = unlock::attempt(
        device,
        &token,
        &material,
        boot_id,
        &peers,
        &volume,
        &ask_dir,
        Duration::from_secs(600),
    );
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("no SIGTERM handler")?;
    let outcome = tokio::select! {
        outcome = attempt => outcome?,
        _ = terminate.recv() => {
            say("the disk was opened another way");
            return Ok(ExitCode::SUCCESS);
        }
    };
    match outcome {
        Outcome::Unlocked => say("unlocked with the consent of your other device"),
        Outcome::Refused(refusal) => say(&format!("{refusal}; enter the passphrase to unlock")),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> Result<ExitCode> {
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("no async runtime")
    };
    match Cli::parse().command {
        Command::Probe { device } => Ok(match luks::token(&device) {
            Ok(Some(_)) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }),
        Command::Unlock { device } => runtime()?.block_on(unlock(&device)).inspect_err(|error| {
            say(&format!("{error:#}; enter the passphrase to unlock"));
        }),
        Command::Enrol(args) => enrol::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Remove { device, key_file } => {
            enrol::remove(&device, key_file.as_deref()).map(|()| ExitCode::SUCCESS)
        }
        Command::Status { device } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&enrol::status(&device)?)?
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Volumes => {
            println!("{}", serde_json::to_string_pretty(&enrol::enrolled()?)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Replay { transcript } => {
            let transcripts: Vec<exchange::Transcript> = serde_json::from_slice(
                &std::fs::read(&transcript)
                    .with_context(|| format!("could not read {}", transcript.display()))?,
            )
            .context("the transcript is malformed")?;
            let runtime = runtime()?;
            for kept in transcripts.iter().filter(|kept| !kept.sent.is_empty()) {
                let answer = runtime.block_on(exchange::replay(kept))?;
                println!("{}:\n{answer}", kept.sibling);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
