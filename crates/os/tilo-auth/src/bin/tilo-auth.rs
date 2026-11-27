//! `tilo-auth`: open the system volume at boot, and talk to `tilo-authd`.
//!
//! `boot-unlock`, `boot-people`, `boot-comforts` and `sibling unlock` work in
//! the initrd and need no daemon; `sibling` also enrols and removes the
//! sibling factor. The rest ask the daemon. Secrets are read from a file or
//! stdin, never argv.

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tilo_auth::boot::{self, BootOutcome, BootUnlock};
use tilo_auth::client::Client;
use tilo_auth::handoff;
use tilo_auth::luks;
use tilo_auth::protocol::{Reply, Request};
use tilo_auth::record::{self, Store};
use tilo_auth::sibling::{enrol, exchange};
use tilo_auth::tpm::{self, Tries};
use tilo_rollback::exec::Real;
use tilo_rollback::policy::Workspace;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "tilo-auth",
    about = "One secret per person: sign in at boot and manage it"
)]
struct Cli {
    /// TPM2 TCTI. Omit for the kernel resource manager at /dev/tpmrm0.
    #[arg(long, global = true)]
    tcti: Option<String>,
    /// tilo-authd's socket.
    #[arg(long, global = true, default_value = tilo_auth::SOCKET)]
    socket: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the system volume with a person's secret, before the root is
    /// mounted. Exit 0: unlocked; 1: wrong secret (a JSON line on stderr says
    /// how many tries are left); 2: nothing could be tried.
    BootUnlock {
        #[arg(long)]
        device: PathBuf,
        /// The volume's name, as systemd-cryptsetup knows it.
        #[arg(long)]
        volume: String,
        /// The secret, without a trailing newline (one is ignored). `-` reads stdin.
        #[arg(long)]
        secret_file: PathBuf,
        /// Whose secret it is. Needed when several people sign in with the TPM.
        #[arg(long)]
        uid: Option<u32>,
        #[arg(long, default_value = "/run/cryptsetup-keys.d", hide = true)]
        keys_dir: PathBuf,
        #[arg(long, default_value = handoff::DESCRIPTION, hide = true)]
        handoff: String,
    },
    /// The people who can open the system volume, as JSON, for the boot screen.
    BootPeople {
        #[arg(long)]
        device: PathBuf,
    },
    /// The device's sign-in comforts, as JSON with every value clamped, or
    /// `null` when none were chosen, for the boot screen.
    BootComforts {
        #[arg(long)]
        device: PathBuf,
    },
    /// Check a secret read from stdin.
    Verify {
        #[arg(long)]
        user: String,
    },
    /// Whether a person's secret is checked by tilo-authd, and their factors.
    Status {
        #[arg(long)]
        user: String,
    },
    /// Change a secret. Reads the current secret (or, with --recovery, the
    /// recovery key) and then the new secret from stdin, one per line.
    SetSecret {
        #[arg(long)]
        user: String,
        #[arg(long)]
        recovery: bool,
    },
    /// Let a recovery key open a person's user key. Reads their secret and
    /// then the recovery key from stdin, one per line. Root only.
    AddRecovery {
        #[arg(long)]
        user: String,
    },
    /// Every person's record, without anything secret. Root only.
    Records {
        #[arg(long, default_value = record::DIR)]
        dir: PathBuf,
    },
    /// Unlock the system volume with another of your devices.
    #[command(subcommand)]
    Sibling(SiblingCmd),
}

#[derive(Subcommand)]
enum SiblingCmd {
    /// Exit 0 when DEVICE carries a sibling-unlock token, 1 otherwise.
    Probe { device: PathBuf },
    /// Ask the owner's other devices to unlock DEVICE for this boot, and
    /// answer its passphrase prompt with the result.
    Unlock { device: PathBuf },
    /// Add a key slot opened with a sibling's consent, and register it with
    /// the owner's devices. `--unattended` also asks the owner to accept this
    /// machine's boot chain, so their devices release its half without a
    /// prompt.
    Enrol(enrol::Args),
    /// Root's part of `enrol` where the daemon runs as an account of its own, a server's: add
    /// the key slot, and hand the boot key's request to that account (`register`).
    Prepare(enrol::PrepareArgs),
    /// As the daemon's own account, ask its daemon to have the person's root certify the boot
    /// key `prepare` handed to DIR, and keep the answer there. It waits for as long as the
    /// person takes to answer.
    Register { dir: PathBuf },
    /// Root's part once `register` was answered: write the token, or remove the slot again.
    Finish(enrol::FinishArgs),
    /// Remove the sibling key slot and its token.
    Remove {
        device: PathBuf,
        /// A key file holding an existing passphrase of the volume.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Print what is enrolled on DEVICE, without secrets.
    Status { device: PathBuf },
    /// Print every volume enrolled on this machine, without secrets
    /// (Settings reads it through polkit; the description is root's alone).
    Volumes,
    /// Send the frames of a kept exchange to its siblings again and print
    /// what they answer (verification: a replay must be refused).
    Replay {
        #[arg(default_value = exchange::TRANSCRIPT)]
        transcript: PathBuf,
    },
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no async runtime")
}

fn sibling(cli: &Cli, cmd: &SiblingCmd) -> Result<ExitCode> {
    let runner = Real::new().with_tcti(cli.tcti.as_deref());
    match cmd {
        SiblingCmd::Probe { device } => Ok(if boot::sibling::enrolled(&runner, device) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        SiblingCmd::Unlock { device } => {
            if let Err(error) = runtime()?.block_on(boot::sibling::unlock(&runner, device)) {
                boot::sibling::say(&format!("{error:#}; enter the passphrase to unlock"));
                return Err(error);
            }
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Enrol(args) => {
            let machine = enrol::Machine {
                runner: &runner,
                has_tpm: tpm::present(cli.tcti.as_deref()),
            };
            enrol::run(args, machine)?;
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Prepare(args) => {
            let machine = enrol::Machine {
                runner: &runner,
                has_tpm: tpm::present(cli.tcti.as_deref()),
            };
            enrol::prepare(args, machine)?;
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Register { dir } => {
            let daemon = enrol::DaemonRegistrar::find()?;
            // The daemon's own start bound (its unit's), with room to spare.
            daemon.listening(std::time::Duration::from_secs(120))?;
            enrol::register(dir, &daemon)?;
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Finish(args) => {
            let machine = enrol::Machine {
                runner: &runner,
                has_tpm: tpm::present(cli.tcti.as_deref()),
            };
            enrol::finish(args, machine)?;
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Remove { device, key_file } => {
            let existing = key_file
                .as_deref()
                .map(|file| enrol::existing_passphrase(device, Some(file)))
                .transpose()?;
            enrol::remove(&runner, device, existing.as_deref().map(Vec::as_slice))?;
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Status { device } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&enrol::status(&runner, device)?)?
            );
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Volumes => {
            println!("{}", serde_json::to_string_pretty(&enrol::enrolled()?)?);
            Ok(ExitCode::SUCCESS)
        }
        SiblingCmd::Replay { transcript } => {
            let transcripts: Vec<exchange::Transcript> = serde_json::from_slice(
                &std::fs::read(transcript)
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

fn stdin_lines(count: usize) -> Result<Vec<Zeroizing<String>>> {
    let mut lines = Vec::with_capacity(count);
    let mut stdin = std::io::stdin().lock();
    for _ in 0..count {
        let mut line = Zeroizing::new(String::new());
        stdin.read_line(&mut line).context("reading stdin")?;
        lines.push(Zeroizing::new(
            line.trim_end_matches(['\n', '\r']).to_owned(),
        ));
    }
    drop(stdin);
    Ok(lines)
}

fn read_secret_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    if path == Path::new("-") {
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes).context("reading stdin")?;
        return Ok(bytes);
    }
    std::fs::read(path)
        .map(Zeroizing::new)
        .with_context(|| format!("reading {}", path.display()))
}

fn tries_json(error: &str, tries: Option<Tries>) -> String {
    serde_json::json!({
        "error": error,
        "tries_left": tries.map(|t| t.left),
        "retry_in_s": tries.and_then(|t| t.retry_in_s),
    })
    .to_string()
}

fn boot_unlock(cli: &Cli, args: &BootUnlockArgs<'_>) -> ExitCode {
    let secret = match read_secret_file(args.secret_file) {
        Ok(secret) => secret,
        Err(e) => {
            eprintln!("tilo-auth: {e:#}");
            return ExitCode::from(2);
        }
    };
    let Ok(scratch) = tempfile::Builder::new().prefix("tilo-auth.").tempdir() else {
        eprintln!("tilo-auth: could not create a scratch directory");
        return ExitCode::from(2);
    };
    let runner = Real::new()
        .with_tcti(cli.tcti.as_deref())
        .with_cwd(scratch.path());
    let ws = Workspace::new(scratch.path());
    let outcome = BootUnlock {
        runner: &runner,
        ws: &ws,
        device: args.device,
        volume: args.volume,
        uid: args.uid,
        keys_dir: args.keys_dir,
        handoff: Some(args.handoff),
    }
    .run(&secret);
    match outcome {
        BootOutcome::Unlocked { .. } => ExitCode::SUCCESS,
        BootOutcome::Wrong(tries) => {
            eprintln!("{}", tries_json("wrong", tries));
            ExitCode::from(1)
        }
        BootOutcome::LockedOut(tries) => {
            eprintln!("{}", tries_json("locked-out", tries));
            ExitCode::from(1)
        }
        BootOutcome::CouldNotTry(why) => {
            eprintln!("tilo-auth: {why}");
            ExitCode::from(2)
        }
    }
}

struct BootUnlockArgs<'a> {
    device: &'a Path,
    volume: &'a str,
    secret_file: &'a Path,
    uid: Option<u32>,
    keys_dir: &'a Path,
    handoff: &'a str,
}

fn print_reply(reply: &Reply) -> Result<()> {
    println!("{}", serde_json::to_string(reply)?);
    if !reply.ok {
        bail!("refused");
    }
    Ok(())
}

fn daemon_call(cli: &Cli, request: &Request) -> Result<()> {
    let mut client = Client::connect(&cli.socket)
        .with_context(|| format!("connecting to tilo-authd at {}", cli.socket.display()))?;
    print_reply(&client.call(request)?)
}

fn records(dir: &Path) -> Result<()> {
    let summary: Vec<serde_json::Value> = Store::new(dir)
        .all()?
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "uid": r.uid,
                "name": r.name,
                "display_name": r.display_name,
                "enrolled": r.enrolled(),
                "factors": r.factors(),
                "unlocks_system": r.unlocks_system,
                "system_keyslot": r.system_keyslot,
                "attention": r.attention,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::BootUnlock { .. } => bail!("internal error: boot-unlock is dispatched in main"),
        Cmd::BootPeople { device } => {
            let scratch = tempfile::Builder::new().prefix("tilo-auth.").tempdir()?;
            let runner = Real::new()
                .with_tcti(cli.tcti.as_deref())
                .with_cwd(scratch.path());
            println!(
                "{}",
                serde_json::to_string(&boot::people(&runner, device)?)?
            );
            Ok(())
        }
        Cmd::BootComforts { device } => {
            let scratch = tempfile::Builder::new().prefix("tilo-auth.").tempdir()?;
            let runner = Real::new().with_cwd(scratch.path());
            println!(
                "{}",
                serde_json::to_string(&luks::comforts(&runner, device)?)?
            );
            Ok(())
        }
        Cmd::Verify { user } => {
            let [secret] = <[_; 1]>::try_from(stdin_lines(1)?)
                .map_err(|_| anyhow::anyhow!("reading the secret"))?;
            daemon_call(
                cli,
                &Request::Verify {
                    user: user.clone(),
                    secret,
                },
            )
        }
        Cmd::Status { user } => daemon_call(cli, &Request::Status { user: user.clone() }),
        Cmd::SetSecret { user, recovery } => {
            let [proof, new] = <[_; 2]>::try_from(stdin_lines(2)?)
                .map_err(|_| anyhow::anyhow!("reading the secrets"))?;
            let (current, recovery) = if *recovery {
                (None, Some(proof))
            } else {
                (Some(proof), None)
            };
            daemon_call(
                cli,
                &Request::SetSecret {
                    user: user.clone(),
                    current,
                    recovery,
                    new,
                },
            )
        }
        Cmd::AddRecovery { user } => {
            let [secret, recovery] = <[_; 2]>::try_from(stdin_lines(2)?)
                .map_err(|_| anyhow::anyhow!("reading the secrets"))?;
            daemon_call(
                cli,
                &Request::AddRecovery {
                    user: user.clone(),
                    secret,
                    recovery,
                },
            )
        }
        Cmd::Records { dir } => records(dir),
        Cmd::Sibling(_) => bail!("internal error: sibling is dispatched in main"),
    }
}

fn main() -> ExitCode {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    let cli = Cli::parse();
    if let Cmd::BootUnlock {
        device,
        volume,
        secret_file,
        uid,
        keys_dir,
        handoff,
    } = &cli.cmd
    {
        return boot_unlock(
            &cli,
            &BootUnlockArgs {
                device,
                volume,
                secret_file,
                uid: *uid,
                keys_dir,
                handoff,
            },
        );
    }
    if let Cmd::Sibling(cmd) = &cli.cmd {
        return sibling(&cli, cmd).unwrap_or_else(|e| {
            eprintln!("tilo-auth: {e:#}");
            ExitCode::FAILURE
        });
    }
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tilo-auth: {e:#}");
            ExitCode::FAILURE
        }
    }
}
