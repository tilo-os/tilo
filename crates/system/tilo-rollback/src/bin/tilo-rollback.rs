//! `tilo-rollback` — bounded rollback protection and TPM lockout hardening.
//!
//! Subcommands, in the order a machine meets them:
//!
//! * `tpm-status`  — what this TPM's dictionary-attack lockout actually permits.
//! * `harden`      — set the DA reset authorisation and tighten the parameters.
//! * `enroll`      — define the floor counter and seal a generation's keyslot.
//! * `unlock`      — unseal a generation's passphrase in early userspace.
//! * `confirm`     — after a healthy boot: prove the generation, prune the
//!   window, THEN raise the floor.
//! * `status`      — floor, retained generations, and whether each unseals.
//! * `certify`     — a signed statement of the floor, for a peer to verify.
//! * `sign-prediction` — build-side: sign a PCR prediction for a generation.
//!
//! Secrets never travel on `argv`. The recovery key and the PIN are read from
//! stdin or from a file passed by path.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::io::Read;
use std::path::{Path, PathBuf};
use tilo_rollback::{
    Config, approval, certify, confirm, counter, exec, generation, keyslots, lockout, policy,
    token::RollbackToken,
};
use zeroize::Zeroizing;

/// Where `unlock` leaves the secret it used, for `confirm` to reuse. `/run` is a tmpfs (no
/// reboot survival, never touches disk); written 0600 in a 0700 directory since it holds a
/// working disk passphrase.
const PROOF_FILE: &str = "/run/tilo-rollback/proof.key";

#[derive(Parser)]
#[command(
    name = "tilo-rollback",
    about = "Bounded rollback protection: the last few OS generations may unseal the disk, older ones may not"
)]
struct Cli {
    /// TPM2 TCTI. Omit for the default kernel resource manager at /dev/tpmrm0; set to reach an
    /// emulator, e.g. `swtpm:host=127.0.0.1,port=2321`.
    #[arg(long, global = true)]
    tcti: Option<String>,

    /// NV index holding the rollback floor.
    #[arg(long, global = true)]
    nv_index: Option<String>,

    /// How many generations keep a sealed keyslot, counting the current one.
    #[arg(long, global = true)]
    retain: Option<u32>,

    /// Public half of the PCR prediction signing key.
    #[arg(long, global = true)]
    pcr_public_key: Option<PathBuf>,

    /// PCR bank and list, e.g. `sha256:7,11`.
    #[arg(long, global = true)]
    pcrs: Option<String>,

    /// Bookkeeping file. Non-secret.
    #[arg(long, global = true)]
    state: Option<PathBuf>,

    /// Directory holding signed PCR predictions.
    #[arg(long, global = true)]
    predictions: Option<PathBuf>,

    /// Public half of the key that authorises floor advances.
    #[arg(long, global = true)]
    floor_authority_key: Option<PathBuf>,

    /// Directory holding the signed floor-advance approvals this device holds.
    #[arg(long, global = true)]
    approvals: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Report what this TPM's lockout configuration actually allows.
    TpmStatus {
        #[arg(long)]
        json: bool,
    },
    /// Set the dictionary-attack reset authorisation and tighten the parameters.
    Harden {
        /// Where to record the generated reset authorisation. Must be on the encrypted root.
        #[arg(long, default_value = "/var/lib/tilo/rollback/lockout-auth")]
        auth_file: PathBuf,
        #[arg(long, default_value_t = 8)]
        max_tries: u32,
        #[arg(long, default_value_t = 7200)]
        recovery_time: u32,
        #[arg(long, default_value_t = 86400)]
        lockout_recovery: u32,
        /// Proceed even though no recovery key has been confirmed. Refused by default — a set reset
        /// authorisation plus a forgotten PIN and no recovery key means unrecoverable data loss.
        #[arg(long)]
        i_have_a_recovery_key: bool,
    },
    /// Seal a generation's keyslot. Used both for first-time enrollment and, at update-staging
    /// time, for the generation that is about to boot.
    Enroll {
        #[arg(long)]
        device: PathBuf,
        /// Generation to enroll. Defaults to the running one. A generation that is not the running
        /// one is sealed to *its* signed prediction, unsatisfiable until that image boots — the
        /// point: staging an update must enroll the incoming generation's keyslot before reboot, or
        /// its first boot has nothing to unseal with and lands on the recovery key.
        #[arg(long)]
        generation: Option<u64>,
        /// Read the authorising passphrase (normally the recovery key) from this file. `-` reads stdin.
        #[arg(long)]
        recovery_key_file: PathBuf,
        /// Read the unlock PIN from this file.
        #[arg(long)]
        pin_file: Option<PathBuf>,
    },
    /// Unseal a generation's passphrase.
    Unlock {
        #[arg(long)]
        device: PathBuf,
        /// Write the passphrase here instead of stdout — intended target is
        /// `/run/cryptsetup-keys.d/<volume>.key`, which `systemd-cryptsetup` picks up as a key file.
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        pin_file: Option<PathBuf>,
        /// Also keep the unsealed secret here for `confirm` to reuse later in the boot. Must be on a
        /// tmpfs; it holds a working disk passphrase.
        #[arg(long, default_value = PROOF_FILE)]
        proof_file: PathBuf,
        /// Do not write the proof file.
        #[arg(long)]
        no_proof_file: bool,
    },
    /// Confirm a generation, then raise the floor.
    Confirm(ConfirmArgs),
    /// Floor, retained generations, and whether each still unseals.
    Status {
        #[arg(long)]
        device: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Produce a signed statement of the rollback floor.
    Certify {
        /// Verifier's nonce, hex.
        #[arg(long)]
        nonce: String,
        /// Directory to write attestation.bin, signature.bin and ak.pub into.
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Emit the request an authority needs in order to issue floor approvals. Contains nothing
    /// secret: the NV index Name, the counter value, and which generations have keyslots.
    FloorRequest {
        #[arg(long)]
        device: PathBuf,
        /// Write here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Authority side: issue single-use floor-advance approvals for a request.
    ApproveFloor {
        #[arg(long)]
        private_key: PathBuf,
        /// The request file, or `-` for stdin.
        #[arg(long)]
        request: PathBuf,
        /// Highest floor generation to approve. Defaults to the safe bound the request reports: the
        /// newest generation that has a sealed keyslot.
        #[arg(long)]
        up_to_floor: Option<u64>,
        #[arg(long)]
        out_dir: PathBuf,
        /// Issue approvals past the safe bound. This can leave the machine unable to unseal any
        /// generation and dependent on its recovery key.
        #[arg(long)]
        i_accept_stranding_this_machine: bool,
    },
    /// Build side: sign a PCR prediction for a generation.
    SignPrediction {
        #[arg(long)]
        private_key: PathBuf,
        /// `tpm2_pcrread -o` blob of the predicted measurements. Omit to predict the machine's
        /// current state.
        #[arg(long)]
        pcr_values: Option<PathBuf>,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value = "")]
        label: String,
    },
}

#[derive(Args)]
struct ConfirmArgs {
    #[arg(long)]
    device: PathBuf,
    #[arg(long)]
    generation: Option<u64>,
    /// Recovery key, for an operator-driven confirmation.
    #[arg(long)]
    recovery_key_file: Option<PathBuf>,
    /// Unattended: instead of verifying the recovery key, require that the keyslots recorded as
    /// independent unlock paths at enrollment are still present. Weaker, and the only option
    /// after an unattended boot.
    #[arg(long)]
    unattended: bool,
    /// Secret that already opens the device — what `unlock` left in /run.
    #[arg(long, default_value = PROOF_FILE)]
    proof_file: PathBuf,
    #[arg(long)]
    pin_file: Option<PathBuf>,
    /// Raise the floor to at least this generation, so a security update can jump past a
    /// vulnerable one instead of leaving it inside the window.
    #[arg(long)]
    floor_at_least: Option<u64>,
    /// Report what would happen and change nothing.
    #[arg(long)]
    dry_run: bool,
}

/// What every subcommand runs against: the TPM tool runner, the scratch workspace, the
/// resolved configuration and the paths derived from it.
struct Env<'a> {
    runner: &'a exec::Real,
    ws: &'a policy::Workspace,
    cfg: &'a Config,
    state_path: &'a Path,
    pred_dir: &'a Path,
    scratch: &'a Path,
}

fn read_secret(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    if path == Path::new("-") {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("reading a secret from stdin")?;
        let t = line.trim_end_matches(['\n', '\r']).to_string();
        if t.is_empty() {
            bail!("empty secret on stdin");
        }
        return Ok(Zeroizing::new(t.into_bytes()));
    }
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .with_context(|| format!("reading {}", path.display()))?;
    // A trailing newline is near-universal in a human-made file, and a passphrase differing by
    // one byte fails in a way that looks like data loss. Strip it.
    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.is_empty() {
        bail!("{} is empty", path.display());
    }
    Ok(Zeroizing::new(buf))
}

fn parse_pcrs(spec: &str) -> Result<(String, Vec<u32>)> {
    let (bank, list) = spec
        .split_once(':')
        .with_context(|| format!("--pcrs {spec} should look like sha256:7,11"))?;
    let pcrs = list
        .split(',')
        .map(|s| s.trim().parse::<u32>().context("PCR index"))
        .collect::<Result<Vec<_>>>()?;
    Ok((bank.to_string(), pcrs))
}

fn config(cli: &Cli) -> Result<Config> {
    let mut cfg = Config::default();
    if let Some(i) = &cli.nv_index {
        let s = i.trim_start_matches("0x").trim_start_matches("0X");
        cfg.nv_index = u32::from_str_radix(s, 16)
            .with_context(|| format!("--nv-index {i} should be hex, e.g. 0x01D91100"))?;
    }
    if let Some(r) = cli.retain {
        if r == 0 {
            bail!("--retain must be at least 1");
        }
        cfg.retained_generations = r;
    }
    if let Some(p) = &cli.pcr_public_key {
        cfg.pcr_public_key = p.display().to_string();
        // The floor authority defaults to the prediction key, so an explicit prediction key moves
        // both unless the floor key is named separately.
        cfg.floor_authority_key = p.display().to_string();
    }
    if let Some(p) = &cli.floor_authority_key {
        cfg.floor_authority_key = p.display().to_string();
    }
    if let Some(p) = &cli.approvals {
        cfg.approval_dir = p.display().to_string();
    }
    if let Some(spec) = &cli.pcrs {
        let (bank, pcrs) = parse_pcrs(spec)?;
        cfg.pcr_bank = bank;
        cfg.pcrs = pcrs;
    }
    Ok(cfg)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tilo-rollback: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config(&cli)?;
    let scratch = tempfile::Builder::new()
        .prefix("tilo-rollback.")
        .tempdir()
        .context("creating a scratch directory")?;
    let runner = exec::Real::new()
        .with_tcti(cli.tcti.as_deref())
        .with_cwd(scratch.path());
    let ws = policy::Workspace::new(scratch.path());
    let state_path = cli.state.clone().unwrap_or_else(confirm::state_path);
    let pred_dir = cli
        .predictions
        .clone()
        .unwrap_or_else(|| PathBuf::from(policy::DEFAULT_PREDICTION_DIR));

    // Grouped by the module doc's own command ordering (provisioning / runtime /
    // authority-side); the split exists only to stay under the line-count lint, it carries
    // no behavioural significance, and this outer match stays exhaustive over `Cmd` so a new
    // subcommand fails to compile until it's routed somewhere.
    let env = Env {
        runner: &runner,
        ws: &ws,
        cfg: &cfg,
        state_path: &state_path,
        pred_dir: &pred_dir,
        scratch: scratch.path(),
    };
    match &cli.cmd {
        Cmd::TpmStatus { .. } | Cmd::Harden { .. } | Cmd::Enroll { .. } => {
            dispatch_provisioning(&cli, &env)
        }
        Cmd::Unlock { .. } | Cmd::Confirm(_) | Cmd::Status { .. } | Cmd::Certify { .. } => {
            dispatch_runtime(&cli, &env)
        }
        Cmd::FloorRequest { .. } | Cmd::ApproveFloor { .. } | Cmd::SignPrediction { .. } => {
            dispatch_authority(&cli, &env)
        }
    }
}

/// Provisioning-time commands: check the TPM's lockout posture, harden it, enroll a
/// generation's keyslot.
fn dispatch_provisioning(cli: &Cli, env: &Env<'_>) -> Result<()> {
    let runner = env.runner;
    match &cli.cmd {
        Cmd::TpmStatus { json } => cmd_tpm_status(runner, *json),
        Cmd::Harden {
            auth_file,
            max_tries,
            recovery_time,
            lockout_recovery,
            i_have_a_recovery_key,
        } => cmd_harden(
            runner,
            auth_file,
            lockout::Target {
                max_tries: *max_tries,
                recovery_time: *recovery_time,
                lockout_recovery: *lockout_recovery,
            },
            *i_have_a_recovery_key,
        ),
        Cmd::Enroll {
            device,
            generation: gen_num,
            recovery_key_file,
            pin_file,
        } => cmd_enroll(
            env,
            device,
            *gen_num,
            recovery_key_file,
            pin_file.as_deref(),
        ),
        // Unreachable: `run` only calls this for TpmStatus/Harden/Enroll.
        _ => bail!("internal error: dispatch_provisioning called for a non-provisioning command"),
    }
}

/// Boot/runtime commands: unseal a generation, confirm it, report status, certify the floor.
fn dispatch_runtime(cli: &Cli, env: &Env<'_>) -> Result<()> {
    let Env {
        runner,
        ws,
        cfg,
        state_path,
        scratch,
        ..
    } = *env;
    match &cli.cmd {
        Cmd::Unlock {
            device,
            output,
            pin_file,
            proof_file,
            no_proof_file,
        } => cmd_unlock(
            runner,
            ws,
            cfg,
            device,
            output.as_deref(),
            pin_file.as_deref(),
            if *no_proof_file {
                None
            } else {
                Some(proof_file)
            },
        ),
        Cmd::Confirm(args) => cmd_confirm(env, args),
        Cmd::Status { device, json } => {
            cmd_status(runner, cfg, state_path, scratch, device.as_deref(), *json)
        }
        Cmd::Certify { nonce, out_dir } => {
            cmd_certify(runner, ws, cfg, state_path, scratch, nonce, out_dir)
        }
        // Unreachable: `run` only calls this for Unlock/Confirm/Status/Certify.
        _ => bail!("internal error: dispatch_runtime called for a non-runtime command"),
    }
}

/// Authority-side commands: build a floor-approval request, issue approvals, sign a PCR
/// prediction. None of these touch a live device's keyslots.
fn dispatch_authority(cli: &Cli, env: &Env<'_>) -> Result<()> {
    let Env {
        runner,
        ws,
        cfg,
        state_path,
        scratch,
        ..
    } = *env;
    match &cli.cmd {
        Cmd::FloorRequest { device, out } => {
            cmd_floor_request(runner, ws, cfg, state_path, scratch, device, out.as_deref())
        }
        Cmd::ApproveFloor {
            private_key,
            request,
            up_to_floor,
            out_dir,
            i_accept_stranding_this_machine,
        } => cmd_approve_floor(
            runner,
            ws,
            private_key,
            request,
            *up_to_floor,
            out_dir,
            *i_accept_stranding_this_machine,
        ),
        Cmd::SignPrediction {
            private_key,
            pcr_values,
            generation: gen_num,
            out_dir,
            label,
        } => cmd_sign_prediction(
            runner,
            ws,
            cfg,
            private_key,
            pcr_values.as_deref(),
            *gen_num,
            out_dir,
            label,
        ),
        // Unreachable: `run` only calls this for FloorRequest/ApproveFloor/SignPrediction.
        _ => bail!("internal error: dispatch_authority called for a non-authority command"),
    }
}

fn cmd_tpm_status(runner: &dyn exec::Runner, json: bool) -> Result<()> {
    let params = lockout::read(runner)?;
    let a = lockout::assess(params, lockout::Target::default());
    if json {
        println!(
            "{}",
            serde_json::json!({
                "max_tries": params.max_tries,
                "recovery_time_s": params.recovery_time,
                "lockout_recovery_s": params.lockout_recovery,
                "failure_counter": params.counter,
                "reset_auth_set": params.lockout_auth_set,
                "in_lockout": params.in_lockout,
                "pin_is_defensible": a.pin_is_defensible(),
                "critical": a.critical,
                "warnings": a.warnings,
            })
        );
        return Ok(());
    }
    println!("TPM dictionary-attack lockout, as this hardware reports it:");
    println!("  maxTries            {}", params.max_tries);
    println!("  recoveryTime        {}s", params.recovery_time);
    println!("  lockoutRecovery     {}s", params.lockout_recovery);
    println!("  failure counter     {}", params.counter);
    println!(
        "  reset authorisation {}",
        if params.lockout_auth_set {
            "set"
        } else {
            "EMPTY"
        }
    );
    println!("  in lockout          {}", params.in_lockout);
    for c in &a.critical {
        println!("\nCRITICAL: {c}");
    }
    for w in &a.warnings {
        println!("warning: {w}");
    }
    if a.pin_is_defensible() {
        println!("\nA short PIN is defensible on this TPM.");
    } else {
        println!("\nA short PIN is NOT defensible on this TPM as configured. Run `harden`.");
    }
    Ok(())
}

fn cmd_harden(
    runner: &dyn exec::Runner,
    auth_file: &Path,
    target: lockout::Target,
    have_recovery_key: bool,
) -> Result<()> {
    let before = lockout::read(runner)?;
    println!(
        "before: maxTries={} recoveryTime={}s lockoutRecovery={}s resetAuth={}",
        before.max_tries,
        before.recovery_time,
        before.lockout_recovery,
        if before.lockout_auth_set {
            "set"
        } else {
            "empty"
        }
    );

    if !before.lockout_auth_set && !have_recovery_key {
        bail!(
            "refusing to set the dictionary-attack reset authorisation without \
             --i-have-a-recovery-key.\n\
             Once it is set, a forgotten PIN plus an exhausted lockout can only be resolved with \
             the recovery key or by clearing the TPM (which destroys every sealed keyslot). \
             Confirm the recovery key is written down first."
        );
    }

    if before.lockout_auth_set {
        println!("reset authorisation already set; leaving it alone");
        if !auth_file.is_file() {
            println!(
                "the existing reset authorisation is owned outside Tilo; preserving its policy \
                 because tightening it without that secret would fail and must not replace \
                 operator-managed TPM state"
            );
            return Ok(());
        }
    } else {
        let auth = lockout::new_reset_auth()?;
        // SAFETY-CRITICAL: record it BEFORE setting it. Dying between the two
        // leaves a TPM whose DA counter can never be reset by anyone.
        if let Some(d) = auth_file.parent() {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        tilo_files::native::atomic::write_with_mode(auth_file, &auth, 0o600).with_context(
            || {
                format!(
                    "atomically recording the reset authorisation in {}",
                    auth_file.display()
                )
            },
        )?;
        let scratch = auth_file
            .parent()
            .context("the reset authorisation file has no parent directory")?;
        lockout::set_reset_auth(runner, &auth, None, scratch)?;
        println!(
            "reset authorisation set; recorded in {}",
            auth_file.display()
        );
        println!(
            "note: that file lives on the encrypted root, which is what keeps it away from an \
             attacker holding a powered-off machine — and also means a TPM lockout cannot be \
             cleared until the disk is open. The recovery key is the way out of that."
        );
    }

    let auth = read_secret(auth_file).ok();
    let scratch = auth_file
        .parent()
        .context("the reset authorisation file has no parent directory")?;
    let after = lockout::tighten(runner, target, auth.as_deref().map(|v| &v[..]), scratch)?;
    println!(
        "after:  maxTries={} recoveryTime={}s lockoutRecovery={}s",
        after.max_tries, after.recovery_time, after.lockout_recovery
    );
    let a = lockout::assess(after, target);
    for w in &a.warnings {
        println!("warning: {w}");
    }
    for c in &a.critical {
        println!("CRITICAL: {c}");
    }
    if !a.pin_is_defensible() {
        bail!("the lockout configuration is still not adequate for a short PIN");
    }
    Ok(())
}

fn cmd_enroll(
    env: &Env<'_>,
    device: &Path,
    gen_num: Option<u64>,
    recovery_key_file: &Path,
    pin_file: Option<&Path>,
) -> Result<()> {
    let Env {
        runner,
        ws,
        cfg,
        state_path,
        pred_dir,
        ..
    } = *env;
    let gen_num = match gen_num {
        Some(g) => g,
        None => generation::running()?,
    };
    let recovery = read_secret(recovery_key_file)?;
    let pin = pin_file.map(read_secret).transpose()?;
    let pin_bytes = pin.as_deref().map(|v| &v[..]);

    // SAFETY-CRITICAL: the independent unlock path has to work before we touch anything at all.
    if !keyslots::verify_recovery(runner, ws, device, &recovery)? {
        bail!(
            "the supplied recovery key does not open {} — refusing to enroll, because every \
             safeguard here depends on that key working",
            device.display()
        );
    }
    println!("recovery key verified on {}", device.display());

    let predictions = policy::load_signed_predictions(pred_dir, gen_num)?;
    if predictions.is_empty() {
        bail!(
            "no signed PCR prediction for generation {gen_num} in {} — a token sealed without one can \
             never satisfy PolicyAuthorize, so enrollment would produce a keyslot that fails at boot",
            pred_dir.display()
        );
    }
    println!(
        "{} signed prediction(s) for generation {gen_num}",
        predictions.len()
    );

    let floor = establish_floor(env, device, gen_num)?;

    let (slot, token_id) = keyslots::enroll_generation(
        runner,
        ws,
        device,
        cfg,
        &keyslots::Enrollment {
            generation: gen_num,
            floor,
            predictions: &predictions,
            pin: pin_bytes,
        },
        &recovery,
    )?;
    println!("generation {gen_num}: keyslot {slot}, token {token_id}");

    let running = verify_enrollment_if_running(runner, ws, cfg, device, gen_num, slot, pin_bytes)?;

    confirm::save_state(
        state_path,
        &confirm::State {
            nv_index: cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: running.filter(|g| *g == gen_num),
            foreign_keyslots: keyslots::foreign_keyslots(runner, device)?,
        },
    )?;
    // SAFETY-CRITICAL: last word goes to the recovery key.
    if !keyslots::verify_recovery(runner, ws, device, &recovery)? {
        bail!(
            "the recovery key no longer opens {} after enrollment — this needs attention NOW",
            device.display()
        );
    }
    println!("recovery key still verified. Enrollment complete.");
    if approval::load_all(Path::new(&cfg.approval_dir))?.is_empty() {
        println!();
        println!(
            "The rollback floor cannot advance until this device holds signed approvals. Produce a \
             request with `tilo-rollback floor-request --device {}`, have the authority run \
             `tilo-rollback approve-floor`, and put the results in {}. Until then the floor stays \
             where it is and the disk keeps unlocking normally.",
            device.display(),
            cfg.approval_dir
        );
    }
    Ok(())
}

/// Define the NV counter (idempotent) and work out the floor this enrollment should record —
/// the existing floor if the counter has already been incremented, or the bottom of the
/// retention window if this is the very first enrollment.
fn establish_floor(env: &Env<'_>, device: &Path, gen_num: u64) -> Result<counter::FloorState> {
    let Env {
        runner,
        ws,
        cfg,
        state_path,
        scratch,
        ..
    } = *env;
    let (authority_name, nv_auth_policy) = floor_authority(runner, ws, cfg)?;
    counter::define(runner, cfg.nv_index, &nv_auth_policy, ws)?;
    let existing = keyslots::list_tokens(runner, device)?;
    let base = confirm::load_base(confirm::load_state(state_path)?.as_ref(), &existing)?;
    let floor = if let Some(b) = base {
        let c = counter::read(runner, cfg.nv_index, scratch)?
            .context("the rollback floor counter exists but has never been incremented")?;
        counter::FloorState {
            counter: c,
            base: b,
        }
    } else {
        // First enrollment starts the floor at the bottom of the window, not at this generation —
        // an immediate floor of `gen_num` would leave the very first update nothing to fall back to.
        let window = cfg
            .retained_generations
            .checked_sub(1)
            .context("retained_generations must be at least 1 (the running generation itself)")?;
        let initial = gen_num.saturating_sub(u64::from(window));
        counter::materialise(runner, ws, cfg.nv_index, scratch, initial, &authority_name)?
    };
    println!(
        "rollback floor counter {} (base {}, floor generation {})",
        floor.counter,
        floor.base,
        floor.floor_generation()
    );
    Ok(floor)
}

/// SAFETY-CRITICAL: prove as much of the seal as is provable right now. For the *running*
/// generation the whole round-trip is provable (live measurements match the prediction, so
/// the TPM should release the secret and open the keyslot). For a generation that is not
/// running — staging an update — it can't be proven: the seal is bound to measurements that
/// won't exist until it boots. What is provable is that the keyslot works
/// (`enroll_generation` verified it) and the current generation's token is untouched, so a
/// first boot that can't unseal falls back rather than failing.
///
/// Returns the running generation (if any), for the caller's `last_confirmed` bookkeeping.
fn verify_enrollment_if_running(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
    device: &Path,
    gen_num: u64,
    slot: u32,
    pin: Option<&[u8]>,
) -> Result<Option<u64>> {
    let tokens = keyslots::list_tokens(runner, device)?;
    let (_, t) = tokens
        .iter()
        .find(|(_, t)| t.generation == gen_num)
        .context("the token just written is not readable back")?;
    let running = generation::running().ok();
    if running == Some(gen_num) {
        match keyslots::unseal(runner, ws, t, Path::new(&cfg.pcr_public_key), pin)? {
            Some(secret) => {
                if !keyslots::verify_passphrase(runner, ws, device, &secret, Some(slot))? {
                    bail!(
                        "the unsealed secret does not open keyslot {slot} — enrollment is not usable"
                    );
                }
                println!("generation {gen_num} unseals and opens keyslot {slot}");
            }
            None => bail!(
                "the token for generation {gen_num} was written but does not unseal on this \
                 machine: the signed prediction does not match the live PCRs. The keyslot was left \
                 in place and the recovery key still works; fix the prediction and re-run."
            ),
        }
    } else {
        println!(
            "generation {gen_num} is not the running generation ({}), so its seal cannot be \
             exercised until that image boots. Its keyslot is verified; the running generation's \
             keyslot and the rollback floor are untouched, so a first boot that cannot unseal falls \
             back instead of failing.",
            running.map_or_else(|| "unknown".into(), |g| g.to_string())
        );
    }
    Ok(running)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600)
        .with_context(|| format!("atomically writing {}", path.display()))?;
    Ok(())
}

fn cmd_unlock(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
    device: &Path,
    output: Option<&Path>,
    pin_file: Option<&Path>,
    proof_file: Option<&Path>,
) -> Result<()> {
    let pin = pin_file.map(read_secret).transpose()?;
    let tokens = keyslots::list_tokens(runner, device)?;
    if tokens.is_empty() {
        bail!("no tilo-rollback token on {}", device.display());
    }
    let mut floor_refusals = Vec::new();
    for (id, t) in &tokens {
        match keyslots::unseal(
            runner,
            ws,
            t,
            Path::new(&cfg.pcr_public_key),
            pin.as_deref().map(|v| &v[..]),
        ) {
            Ok(Some(secret)) => {
                if let Some(p) = output {
                    write_private(p, &secret)?;
                    eprintln!(
                        "tilo-rollback: generation {} unsealed to {}",
                        t.generation,
                        p.display()
                    );
                } else {
                    use std::io::Write;
                    std::io::stdout().write_all(&secret)?;
                }
                // Keep it for `confirm`, which runs later in the same boot and cannot re-unseal without the PIN.
                if let Some(pf) = proof_file {
                    write_private(pf, &secret)?;
                }
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => {
                floor_refusals.push(format!("token {id} (generation {}): {e:#}", t.generation));
            }
        }
    }
    if !floor_refusals.is_empty() {
        bail!(
            "no generation could unseal. The rollback floor refused:\n  {}\n\
             This machine is running an image older than the oldest generation it still permits. \
             Unlock with the recovery key.",
            floor_refusals.join("\n  ")
        );
    }
    bail!(
        "no generation could unseal: none of the {} token(s) matched a signed prediction for the \
         live measurements. Either the firmware or kernel changed without a new prediction being \
         shipped, or this is not the machine these tokens were sealed on. Unlock with the recovery \
         key.",
        tokens.len()
    )
}

fn cmd_confirm(env: &Env<'_>, args: &ConfirmArgs) -> Result<()> {
    let Env {
        runner,
        ws,
        cfg,
        state_path,
        pred_dir,
        scratch,
    } = *env;
    let ConfirmArgs {
        device,
        generation: gen_num,
        recovery_key_file,
        unattended,
        proof_file,
        pin_file,
        floor_at_least,
        dry_run,
    } = args;
    let (unattended, floor_at_least, dry_run) = (*unattended, *floor_at_least, *dry_run);
    let gen_num = match gen_num {
        Some(g) => *g,
        None => generation::running()?,
    };
    let recovery = recovery_key_file.as_deref().map(read_secret).transpose()?;
    let pin = pin_file.as_deref().map(read_secret).transpose()?;
    let proof = match read_secret(proof_file) {
        Ok(p) => Some(p),
        Err(_) if recovery.is_some() || pin.is_some() => None,
        Err(e) if unattended => {
            return Err(e).context(
                "unattended confirmation needs the secret `unlock` left behind; without it there is                  no way to authorise a keyslot change after the PIN prompt is gone",
            );
        }
        Err(_) => None,
    };
    let recorded_slots = confirm::load_state(state_path)?
        .map(|s| s.foreign_keyslots)
        .unwrap_or_default();
    let check = match (&recovery, unattended) {
        (Some(k), false) => keyslots::RecoveryCheck::Key(k),
        (_, true) => keyslots::RecoveryCheck::ForeignSlotsPresent {
            expected: &recorded_slots,
        },
        (None, false) => bail!(
            "pass --recovery-key-file to confirm with the recovery key verified, or --unattended to              accept the weaker check that the independent keyslots are still present"
        ),
    };
    let outcome = confirm::run(
        runner,
        ws,
        &confirm::Request {
            cfg,
            device,
            generation: gen_num,
            prediction_dir: pred_dir,
            recovery: check,
            pin: pin.as_deref().map(|v| &v[..]),
            proof: proof.as_deref().map(|v| &v[..]),
            floor_at_least,
            state_path,
            scratch,
            dry_run,
        },
    )?;

    if let Some((slot, token_id)) = outcome.enrolled {
        println!("generation {gen_num}: keyslot {slot}, token {token_id}");
    }
    let p = &outcome.plan;
    println!(
        "floor: {} -> {} (window {}{})",
        p.floor.current_floor,
        p.floor.target_floor,
        p.floor.window_floor,
        p.floor
            .declared_floor
            .map(|d| format!(", declared {d}"))
            .unwrap_or_default()
    );
    println!("keep generations {:?}, drop {:?}", p.keep, p.drop);
    if dry_run {
        println!("dry run: nothing changed");
        return Ok(());
    }
    for g in &outcome.dropped {
        println!("dropped generation {g}");
    }
    println!(
        "rollback floor is now generation {} (counter {})",
        outcome.floor.floor_generation(),
        outcome.floor.counter
    );
    println!(
        "independent unlock path re-checked ({}).",
        if unattended {
            "keyslots present"
        } else {
            "recovery key verified"
        }
    );
    Ok(())
}

fn cmd_status(
    runner: &dyn exec::Runner,
    cfg: &Config,
    state_path: &Path,
    scratch: &Path,
    device: Option<&Path>,
    json: bool,
) -> Result<()> {
    let state = confirm::load_state(state_path)?;
    let tokens: Vec<(u32, RollbackToken)> = match device {
        Some(d) => keyslots::list_tokens(runner, d)?,
        None => Vec::new(),
    };
    let base = confirm::load_base(state.as_ref(), &tokens)?;
    let counter_value = counter::read(runner, cfg.nv_index, scratch).ok().flatten();
    let floor = match (base, counter_value) {
        (Some(b), Some(c)) => Some(counter::FloorState {
            counter: c,
            base: b,
        }),
        _ => None,
    };
    let gens: Vec<u64> = tokens.iter().map(|(_, t)| t.generation).collect();
    let running = generation::running().ok();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "nv_index": format!("0x{:08X}", cfg.nv_index),
                "counter": counter_value,
                "base": base,
                "floor_generation": floor.map(|f| f.floor_generation()),
                "running_generation": running,
                "retained_generations": cfg.retained_generations,
                "enrolled_generations": gens,
            })
        );
        return Ok(());
    }
    println!("NV index            0x{:08X}", cfg.nv_index);
    match counter_value {
        Some(c) => println!("counter             {c}"),
        None => println!("counter             not initialised"),
    }
    match floor {
        Some(f) => println!("rollback floor      generation {}", f.floor_generation()),
        None => println!("rollback floor      unknown (no recorded base)"),
    }
    match running {
        Some(g) => println!("running generation  {g}"),
        None => println!("running generation  unknown (no tilo.generation= on the command line)"),
    }
    println!(
        "retention window    {} generations",
        cfg.retained_generations
    );
    if device.is_some() {
        println!("enrolled            {gens:?}");
        if let Some(f) = floor {
            for g in &gens {
                let permitted = *g >= f.floor_generation();
                println!(
                    "  generation {g:<6} {}",
                    if permitted {
                        "permitted by the floor"
                    } else {
                        "BLOCKED by the floor"
                    }
                );
            }
        }
    }
    Ok(())
}

fn cmd_certify(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
    state_path: &Path,
    scratch: &Path,
    nonce_hex: &str,
    out_dir: &Path,
) -> Result<()> {
    let nonce = hex::decode(nonce_hex).context("--nonce must be hex")?;
    if nonce.is_empty() {
        bail!("--nonce must not be empty: without a verifier-chosen nonce the attestation replays");
    }
    let base = confirm::load_state(state_path)?
        .map(|s| s.nv_base)
        .context("no recorded counter base, so the floor generation cannot be reported")?;
    let att = certify::certify_floor(runner, ws, cfg.nv_index, base, &nonce, scratch)?;
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join("attestation.bin"), &att.attestation)?;
    std::fs::write(out_dir.join("signature.bin"), &att.signature)?;
    std::fs::write(out_dir.join("ak.pub"), &att.key_public)?;
    println!(
        "certified: counter {} = floor generation {} ({} bytes attestation, {} bytes signature)",
        att.counter,
        att.floor_generation,
        att.attestation.len(),
        att.signature.len()
    );
    println!("written to {}", out_dir.display());
    Ok(())
}

fn cmd_sign_prediction(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
    private_key: &Path,
    pcr_values: Option<&Path>,
    gen_num: u64,
    out_dir: &Path,
    label: &str,
) -> Result<()> {
    let selection = policy::pcr_selection(&cfg.pcr_bank, &cfg.pcrs);
    let approved = policy::approved_policy(runner, ws, &selection, pcr_values)?;
    let sig = policy::sign_prediction(runner, ws, private_key, &approved, gen_num)?;
    let path = policy::write_signed_prediction(
        out_dir,
        gen_num,
        &cfg.pcr_bank,
        &cfg.pcrs,
        &policy::Prediction {
            approved_policy: approved,
            signature: sig,
            label: label.to_string(),
        },
    )?;
    println!(
        "signed prediction for generation {gen_num} -> {}",
        path.display()
    );
    Ok(())
}

/// Shared setup: the floor authority's key Name and the index's `authPolicy`.
fn floor_authority(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
) -> Result<(Vec<u8>, tilo_rollback::tpm_policy::Digest256)> {
    let name = policy::key_name(runner, ws, Path::new(&cfg.floor_authority_key))?;
    let policy_digest = counter::auth_policy(&name);
    Ok((name, policy_digest))
}

fn cmd_floor_request(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    cfg: &Config,
    state_path: &Path,
    scratch: &Path,
    device: &Path,
    out: Option<&Path>,
) -> Result<()> {
    let tokens = keyslots::list_tokens(runner, device)?;
    let state = confirm::load_state(state_path)?;
    let base = confirm::load_base(state.as_ref(), &tokens)?
        .context("no rollback enrollment on this device yet: run `tilo-rollback enroll` first")?;
    let counter_value = counter::read(runner, cfg.nv_index, scratch)?
        .context("the rollback floor counter has never been incremented")?;
    let floor = counter::FloorState {
        counter: counter_value,
        base,
    };
    let nv_name = counter::read_name(runner, cfg.nv_index)?;
    let generations: Vec<u64> = tokens.iter().map(|(_, t)| t.generation).collect();
    let req = approval::build_request(
        cfg.nv_index,
        &nv_name,
        floor,
        &generations,
        cfg.retained_generations,
    )?;
    let json = serde_json::to_string_pretty(&req)?;
    match out {
        Some(p) => {
            std::fs::write(p, &json).with_context(|| format!("writing {}", p.display()))?;
            println!(
                "floor request for counter {} (floor generation {}) -> {}",
                req.counter,
                req.floor_generation,
                p.display()
            );
            println!(
                "safe to approve up to floor generation {} (newest generation with a keyslot)",
                req.safe_target_floor
            );
        }
        None => println!("{json}"),
    }
    let _ = ws;
    Ok(())
}

fn cmd_approve_floor(
    runner: &dyn exec::Runner,
    ws: &policy::Workspace,
    private_key: &Path,
    request_path: &Path,
    up_to_floor: Option<u64>,
    out_dir: &Path,
    allow_past_safe_bound: bool,
) -> Result<()> {
    let text = if request_path == Path::new("-") {
        use std::io::Read as _;
        let mut b = String::new();
        std::io::stdin().read_to_string(&mut b)?;
        b
    } else {
        std::fs::read_to_string(request_path)
            .with_context(|| format!("reading {}", request_path.display()))?
    };
    let req: approval::Request =
        serde_json::from_str(&text).context("parsing the floor request")?;
    let up_to = up_to_floor.unwrap_or(req.safe_target_floor);
    let approvals = approval::issue(runner, ws, &req, private_key, up_to, allow_past_safe_bound)?;
    if approvals.is_empty() {
        println!(
            "nothing to approve: the floor is already at generation {}",
            req.floor_generation
        );
        return Ok(());
    }
    let paths = approval::store(out_dir, &approvals)?;
    println!(
        "issued {} single-use approval(s), floor generation {} -> {}",
        approvals.len(),
        req.floor_generation,
        up_to
    );
    for p in paths {
        println!("  {}", p.display());
    }
    if allow_past_safe_bound && up_to > req.safe_target_floor {
        println!(
            "WARNING: {up_to} is above the safe bound of {} that the request reported. Applying \
             these can leave the machine unable to unseal any generation.",
            req.safe_target_floor
        );
    }
    Ok(())
}
