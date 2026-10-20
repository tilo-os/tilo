//! Sealing, and the LUKS2 keyslots that hold the sealed passphrases.
//!
//! # Shape
//!
//! One keyslot per retained generation, with a fresh 32-byte passphrase sealed to that
//! generation's policy and stored in a `tilo-rollback` LUKS2 token next to the slot; unlocking
//! reads the token, unseals the passphrase, and hands it to cryptsetup. This deliberately does
//! **not** touch the keyslots `systemd-cryptenroll` creates or the recovery-key slot — those
//! stay exactly as they are, keeping the recovery key an independent path.
//!
//! # Why not systemd-cryptenroll's own TPM2 token
//!
//! `systemd-cryptenroll --tpm2-device` has no `TPM2_PolicyNV` support, so it can't attach the
//! rollback floor; its `--tpm2-public-key` signed-policy covers firmware updates but not the
//! floor. Since the floor is the point, the seal is ours — at the cost that
//! `systemd-cryptsetup` can't open our token by itself, so a unit in early userspace runs
//! `tilo-rollback unlock` and writes the passphrase to `/run/cryptsetup-keys.d/<volume>.key`,
//! which `systemd-cryptsetup` picks up as an implicit key file. No patched systemd, no
//! cryptsetup plugin, no keyscript.
//!
//! # SAFETY-CRITICAL rules encoded here
//!
//! * [`verify_passphrase`] is the only way anything here concludes a slot works, and it's
//!   called *before* any destructive step.
//! * [`add_generation_slot`] verifies the new slot opens the device before returning; a slot
//!   that can't open is removed again rather than left as a plausible-looking token that
//!   fails at boot.
//! * [`kill_generation_slot`] refuses to run without proof that another path works.
//! * Nothing here calls `luksKillSlot` on a slot it didn't create, or `luksHeaderRestore`.

use crate::argv;
use crate::exec::{Runner, require};
use crate::policy::{self, Prediction, Workspace};
use crate::token::{RollbackToken, SealedObject};
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::Path;
use zeroize::Zeroizing;

/// A freshly generated slot passphrase. Zeroized on drop.
pub type Passphrase = Zeroizing<Vec<u8>>;

/// 32 bytes from the kernel CSPRNG, hex-encoded so it's safe to hand to cryptsetup as a
/// key file and to `tpm2_create -i`.
///
/// # Errors
///
/// Returns an error if the operating system's random source fails.
pub fn new_passphrase() -> Result<Passphrase> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    Ok(Zeroizing::new(hex::encode(bytes).into_bytes()))
}

/// Does `passphrase` open `device`? Optionally restricted to one keyslot. `--test-passphrase`
/// does the full key-derivation and digest check without activating a mapping, so it's safe
/// on an already-open device and needs no device-mapper privileges beyond reading the header.
///
/// # Errors
///
/// Returns an error if writing the scratch key file or running `cryptsetup` fails.
pub fn verify_passphrase(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    passphrase: &[u8],
    keyslot: Option<u32>,
) -> Result<bool> {
    let kf = ws.put("verify.key", passphrase)?;
    let mut args = argv!["open", "--test-passphrase", "--key-file", kf.as_os_str()];
    let slot;
    if let Some(k) = keyslot {
        slot = k.to_string();
        args.push(OsStr::new("--key-slot"));
        args.push(OsStr::new(&slot));
    }
    args.push(device.as_os_str());
    let out = runner.run("cryptsetup", &args, None)?;
    let _ = std::fs::remove_file(&kf);
    Ok(out.ok())
}

/// SAFETY-CRITICAL: prove the recovery key still opens the device. Everything destructive in
/// this module takes the result of this call as a precondition — a separate function so the
/// proof is a value that has to be produced, not a comment claiming it was considered.
///
/// # Errors
///
/// Returns an error if [`verify_passphrase`] fails to run.
pub fn verify_recovery(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    recovery_key: &[u8],
) -> Result<bool> {
    verify_passphrase(runner, ws, device, recovery_key, None)
}

/// How the independent unlock path is checked before anything destructive. Enrollment happens
/// with a person present who was just shown the recovery key, so it can be checked properly;
/// confirmation happens unattended after reboot, where nothing knows the recovery key by
/// design (storing it would make it a second copy of the disk key) — so the two get
/// different, honestly-labelled checks.
pub enum RecoveryCheck<'a> {
    /// Open the device with the recovery key.
    Key(&'a [u8]),
    /// Unattended: confirm the keyslots that are *not* ours are still there. Strictly weaker —
    /// it proves the slot exists and we didn't remove it, nothing about whether the recovery key
    /// still derives its key — but it's the strongest check available without the secret, and it
    /// catches the failure that actually happens: a keyslot removed by accident.
    ForeignSlotsPresent { expected: &'a [u32] },
}

/// Keyslot numbers in the header that no `tilo-rollback` token claims: the recovery key's
/// slot and any `systemd-cryptenroll` slots. Nothing in this crate ever writes to them.
///
/// # Errors
///
/// Returns an error if reading the keyslots or tokens in use fails.
pub fn foreign_keyslots(runner: &dyn Runner, device: &Path) -> Result<Vec<u32>> {
    let all = keyslots_in_use(runner, device)?;
    let ours: Vec<u32> = list_tokens(runner, device)?
        .into_iter()
        .filter_map(|(_, t)| t.keyslot().ok())
        .collect();
    Ok(all.into_iter().filter(|s| !ours.contains(s)).collect())
}

/// Run a [`RecoveryCheck`].
///
/// # Errors
///
/// Returns an error if the check fails — the recovery key doesn't open the device, or no
/// independent unlock path remains — or if the underlying keyslot/token lookups fail.
pub fn verify_recovery_path(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    check: &RecoveryCheck<'_>,
) -> Result<()> {
    match check {
        RecoveryCheck::Key(k) => {
            if !verify_recovery(runner, ws, device, k)? {
                bail!(
                    "the supplied recovery key does not open {} — every safeguard here depends on \
                     that key working, so nothing will be changed",
                    device.display()
                );
            }
            Ok(())
        }
        RecoveryCheck::ForeignSlotsPresent { expected } => {
            let present = foreign_keyslots(runner, device)?;
            if present.is_empty() {
                bail!(
                    "{} has no keyslot outside the ones this tool manages: there is no independent \
                     unlock path left, so nothing will be changed",
                    device.display()
                );
            }
            if let Some(missing) = expected.iter().find(|e| !present.contains(e)) {
                bail!(
                    "keyslot {missing} was an independent unlock path at enrollment and is no \
                     longer in {}'s header; refusing to change anything until that is explained",
                    device.display()
                );
            }
            Ok(())
        }
    }
}

/// Read every `tilo-rollback` token in the header, newest generation first.
///
/// # Errors
///
/// Returns an error if running `cryptsetup token export` fails to execute. A token id that
/// simply isn't in use, or a token of a different type, is skipped rather than an error.
pub fn list_tokens(runner: &dyn Runner, device: &Path) -> Result<Vec<(u32, RollbackToken)>> {
    let mut found = Vec::new();
    // LUKS2 allows 32 tokens; no "list tokens" subcommand is stable across cryptsetup versions,
    // so probe each id — `token export` on an empty id fails cheaply.
    for id in 0..32u32 {
        let ids = id.to_string();
        let out = runner.run(
            "cryptsetup",
            &argv!["token", "export", "--token-id", &ids, device.as_os_str()],
            None,
        )?;
        if !out.ok() {
            continue;
        }
        let text = out.stdout_utf8();
        if !text.contains(crate::token::TOKEN_TYPE) {
            continue;
        }
        match RollbackToken::from_json(text.trim()) {
            Ok(t) => found.push((id, t)),
            Err(e) => {
                // A malformed token of our own type is worth surfacing, but it must not stop the machine
                // from using the good ones.
                eprintln!(
                    "tilo-rollback: ignoring token {id} on {}: {e:#}",
                    device.display()
                );
            }
        }
    }
    // Newest generation first: `unlock` tries them in this order, since the newest is most
    // likely the running image.
    found.sort_by_key(|(_, t)| std::cmp::Reverse(t.generation));
    Ok(found)
}

/// Create the sealing parent, deterministic from the owner hierarchy seed and template, so it
/// doesn't need to be persisted and survives a reboot.
///
/// # Errors
///
/// Returns an error if `tpm2_createprimary` fails.
pub fn primary(runner: &dyn Runner, ws: &Workspace) -> Result<std::path::PathBuf> {
    policy::flush(runner);
    let ctx = ws.path("primary.ctx");
    require(
        runner,
        "tpm2_createprimary",
        &argv![
            "-C",
            "o",
            "-g",
            "sha256",
            "-G",
            "ecc",
            "-c",
            ctx.as_os_str()
        ],
        "creating the TPM sealing parent",
    )?;
    Ok(ctx)
}

/// Seal `secret` under a generation's policy. Created **without** `noda`, so failed PIN
/// attempts count against the TPM's dictionary-attack lockout — the entire reason a
/// four-digit PIN is defensible; setting `noda` would silently turn it into four digits of
/// nothing.
///
/// # Errors
///
/// Returns an error if creating the sealing parent, sealing with `tpm2_create`, or reading
/// back the sealed public/private blobs fails.
pub fn seal(
    runner: &dyn Runner,
    ws: &Workspace,
    secret: &[u8],
    policy_digest: &[u8],
    pin: Option<&[u8]>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let parent = primary(runner, ws)?;
    let pol = ws.put("seal.policy", policy_digest)?;
    let sec = ws.put("seal.secret", secret)?;
    let pub_out = ws.path("sealed.pub");
    let priv_out = ws.path("sealed.priv");

    let mut args = argv![
        "-C",
        parent.as_os_str(),
        "-g",
        "sha256",
        "-u",
        pub_out.as_os_str(),
        "-r",
        priv_out.as_os_str(),
        "-L",
        pol.as_os_str(),
        "-i",
        sec.as_os_str(),
        "-a",
        "fixedtpm|fixedparent|adminwithpolicy",
    ];
    // tpm2-tools accepts `file:<path>` for an auth value, so the PIN goes through a scratch file
    // — passing it literally would put it in /proc/<pid>/cmdline for every process to read.
    let pin_spec;
    if let Some(p) = pin {
        let pin_file = ws.put("seal.pin", p)?;
        pin_spec = format!("file:{}", pin_file.display());
        args.push(OsStr::new("-p"));
        args.push(OsStr::new(&pin_spec));
    }
    let r = require(
        runner,
        "tpm2_create",
        &args,
        "sealing the generation secret",
    );
    let _ = std::fs::remove_file(ws.path("seal.secret"));
    let _ = std::fs::remove_file(ws.path("seal.pin"));
    r?;

    let pu = std::fs::read(&pub_out).with_context(|| format!("reading {}", pub_out.display()))?;
    let pr = std::fs::read(&priv_out).with_context(|| format!("reading {}", priv_out.display()))?;
    policy::flush(runner);
    Ok((pu, pr))
}

/// Unseal a token's passphrase, trying each prediction in turn. Returns `Ok(None)` when no
/// prediction fits the current machine state (wrong generation, or the firmware moved); an
/// error means the floor rejected the generation or the mechanism failed, and the caller
/// should not silently move on.
///
/// # Errors
///
/// Returns an error if the token fails to validate, if a PIN is required but not supplied,
/// if unsealing fails for a reason other than a prediction mismatch, or if the rollback floor
/// refuses every prediction that otherwise matched.
pub fn unseal(
    runner: &dyn Runner,
    ws: &Workspace,
    token: &RollbackToken,
    public_key_pem: &Path,
    pin: Option<&[u8]>,
) -> Result<Option<Passphrase>> {
    token.validate()?;
    let key_name = token.decoded_key_name()?;
    let predictions = token.decoded_predictions()?;
    let sealed_policy = token.policy(&key_name);
    let parent = primary(runner, ws)?;
    let pub_file = ws.put("unseal.pub", &token.decoded_public()?)?;
    let priv_file = ws.put("unseal.priv", &token.decoded_private()?)?;

    let mut floor_refusal: Option<anyhow::Error> = None;
    for pred in &predictions {
        let session = match policy::satisfy(runner, ws, public_key_pem, &sealed_policy, pred) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                // Remember the floor refusal but keep trying — a different prediction can't change the
                // floor, this just produces the best diagnostic at the end.
                floor_refusal = Some(e);
                continue;
            }
        };
        // Load the sealed object only now, after the session is satisfied.
        let obj = ws.path("unseal.ctx");
        require(
            runner,
            "tpm2_load",
            &argv![
                "-C",
                parent.as_os_str(),
                "-u",
                pub_file.as_os_str(),
                "-r",
                priv_file.as_os_str(),
                "-c",
                obj.as_os_str(),
            ],
            "loading the sealed generation secret",
        )?;
        let out_file = ws.path("unsealed.bin");
        let out = invoke_unseal(
            runner,
            ws,
            &obj,
            &session.context,
            &out_file,
            token.generation,
            token.pin,
            pin,
        )?;
        let _ = runner.run(
            "tpm2_flushcontext",
            &argv![session.context.as_os_str()],
            None,
        );
        if !out.ok() {
            let msg = out.last_error_line();
            policy::flush(runner);
            if token.pin {
                bail!(
                    "unsealing generation {} failed, most likely a wrong PIN: {msg}",
                    token.generation
                );
            }
            bail!("unsealing generation {} failed: {msg}", token.generation);
        }
        let secret = std::fs::read(&out_file).context("reading the unsealed secret")?;
        let _ = std::fs::remove_file(&out_file);
        policy::flush(runner);
        return Ok(Some(Zeroizing::new(secret)));
    }
    policy::flush(runner);
    if let Some(e) = floor_refusal {
        return Err(e);
    }
    Ok(None)
}

/// The sealed public/private blobs a caller holds outside a LUKS2 token.
#[derive(Debug, Clone, Copy)]
pub struct SealedArtifact<'a> {
    pub public: &'a [u8],
    /// `TPM2B_PRIVATE`.
    pub private: &'a [u8],
    /// Diagnostics only; the authoritative digest is inside `public`.
    pub policy_digest: &'a [u8],
}

/// Unseal an artifact sealed under a signed target boot policy with no NV
/// counter. The predictions are tried in order, exactly like a rollback
/// token's; `Ok(None)` means none matches this machine's live state.
///
/// # Errors
///
/// Returns an error if the mechanism fails or the policy requires a PIN, which
/// this installer handoff never uses.
pub fn unseal_boot(
    runner: &dyn Runner,
    ws: &Workspace,
    artifact: &SealedArtifact<'_>,
    policy: &policy::BootPolicy<'_>,
    predictions: &[Prediction],
    public_key_pem: &Path,
) -> Result<Option<Passphrase>> {
    if policy.pin {
        bail!(
            "a PIN-sealed boot artifact needs the rollback token path, not the installer handoff"
        );
    }
    if predictions.is_empty() {
        bail!(
            "refusing to unseal generation {} with no signed prediction",
            policy.generation
        );
    }
    let parent = primary(runner, ws)?;
    let pub_file = ws.put("unseal.pub", artifact.public)?;
    let priv_file = ws.put("unseal.priv", artifact.private)?;
    let _ = artifact.policy_digest;
    for pred in predictions {
        let session = match policy::satisfy_boot(runner, ws, public_key_pem, policy, pred) {
            Ok(Some(session)) => session,
            Ok(None) => continue,
            Err(error) => {
                policy::flush(runner);
                return Err(error);
            }
        };
        let obj = ws.path("unseal.ctx");
        require(
            runner,
            "tpm2_load",
            &argv![
                "-C",
                parent.as_os_str(),
                "-u",
                pub_file.as_os_str(),
                "-r",
                priv_file.as_os_str(),
                "-c",
                obj.as_os_str(),
            ],
            "loading the sealed preparation secret",
        )?;
        let out_file = ws.path("unsealed.bin");
        let out = invoke_unseal(
            runner,
            ws,
            &obj,
            &session.context,
            &out_file,
            policy.generation,
            false,
            None,
        )?;
        let _ = runner.run(
            "tpm2_flushcontext",
            &argv![session.context.as_os_str()],
            None,
        );
        if !out.ok() {
            let message = out.last_error_line();
            policy::flush(runner);
            bail!(
                "unsealing generation {} failed: {message}",
                policy.generation
            );
        }
        let secret = std::fs::read(&out_file).context("reading the unsealed secret")?;
        let _ = std::fs::remove_file(&out_file);
        policy::flush(runner);
        return Ok(Some(Zeroizing::new(secret)));
    }
    policy::flush(runner);
    Ok(None)
}

/// Build tpm2-tools' compound policy-session authorization without putting a
/// PIN in argv. Its `file:` form is read by the child after it starts.
fn unseal_auth(session: &Path, pin_file: Option<&Path>) -> String {
    match pin_file {
        Some(pin) => format!("session:{}+file:{}", session.display(), pin.display()),
        None => format!("session:{}", session.display()),
    }
}

fn invoke_unseal(
    runner: &dyn Runner,
    ws: &Workspace,
    object: &Path,
    session: &Path,
    output: &Path,
    generation: u64,
    pin_required: bool,
    pin: Option<&[u8]>,
) -> Result<crate::exec::Output> {
    let pin_path = match (pin_required, pin) {
        (true, Some(value)) => Some(ws.put("unseal.pin", value)?),
        (true, None) => bail!("generation {generation} is sealed with a PIN but none was supplied"),
        (false, _) => None,
    };
    let auth = unseal_auth(session, pin_path.as_deref());
    let result = runner.run(
        "tpm2_unseal",
        &argv![
            "-c",
            object.as_os_str(),
            "-p",
            &auth,
            "-o",
            output.as_os_str()
        ],
        None,
    );
    if let Some(path) = pin_path {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Add a keyslot for `passphrase`, authorised by `authorising`, and prove it works before
/// returning.
///
/// SAFETY-CRITICAL: if the new slot can't open the device, it's removed again and the call
/// fails — leaving a non-working slot with a plausible token would produce a machine that
/// looks enrolled and asks for the recovery key at boot.
///
/// # Errors
///
/// Returns an error if `luksAddKey` fails, if it doesn't produce exactly one new keyslot, or
/// if the new keyslot fails to open the device (in which case it is removed before returning).
pub fn add_generation_slot(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    authorising: &[u8],
    passphrase: &[u8],
    pbkdf: Option<&crate::Pbkdf>,
) -> Result<u32> {
    let before = keyslots_in_use(runner, device)?;
    let auth_file = ws.put("auth.key", authorising)?;
    let new_file = ws.put("new.key", passphrase)?;
    let mut args = argv![
        "luksAddKey",
        "--batch-mode",
        "--key-file",
        auth_file.as_os_str(),
    ];
    let iters;
    if let Some(p) = pbkdf {
        iters = p.iterations.to_string();
        args.push(OsStr::new("--pbkdf"));
        args.push(OsStr::new(&p.algorithm));
        args.push(OsStr::new("--pbkdf-force-iterations"));
        args.push(OsStr::new(&iters));
    }
    args.push(device.as_os_str());
    args.push(new_file.as_os_str());
    let r = require(runner, "cryptsetup", &args, "adding a generation keyslot");
    let _ = std::fs::remove_file(&auth_file);
    let _ = std::fs::remove_file(&new_file);
    r?;

    let after = keyslots_in_use(runner, device)?;
    let mut added: Vec<u32> = after.into_iter().filter(|s| !before.contains(s)).collect();
    if added.len() != 1 {
        bail!(
            "expected exactly one new keyslot after luksAddKey, found {added:?} — refusing to guess \
             which one to bind a token to"
        );
    }
    let slot = added.remove(0);

    if !verify_passphrase(runner, ws, device, passphrase, Some(slot))? {
        // Undo, using the passphrase we just added (the least privileged authorisation available
        // for its own removal).
        let kf = ws.put("undo.key", passphrase)?;
        let slot_s = slot.to_string();
        let _ = runner.run(
            "cryptsetup",
            &argv![
                "luksKillSlot",
                "--batch-mode",
                "--key-file",
                kf.as_os_str(),
                device.as_os_str(),
                &slot_s,
            ],
            None,
        );
        let _ = std::fs::remove_file(&kf);
        bail!(
            "the new keyslot {slot} does not open {} — removed it again rather than leaving a \
             keyslot that fails at boot",
            device.display()
        );
    }
    Ok(slot)
}

/// Keyslot numbers currently in use.
///
/// # Errors
///
/// Returns an error if reading the LUKS2 header fails.
pub fn keyslots_in_use(runner: &dyn Runner, device: &Path) -> Result<Vec<u32>> {
    let out = require(
        runner,
        "cryptsetup",
        &argv!["luksDump", device.as_os_str()],
        "reading the LUKS2 header",
    )?;
    Ok(parse_keyslots(&out.stdout_utf8()))
}

/// Parse the keyslot numbers out of `cryptsetup luksDump` output.
pub fn parse_keyslots(dump: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut in_keyslots = false;
    for line in dump.lines() {
        if line.starts_with("Keyslots:") {
            in_keyslots = true;
            continue;
        }
        if in_keyslots {
            // A new top-level section ends the list.
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
            let t = line.trim_start();
            // "  0: luks2"
            if let Some((num, rest)) = t.split_once(':')
                && !rest.trim().is_empty()
                && let Ok(n) = num.trim().parse::<u32>()
                && line.starts_with("  ")
                && !line.starts_with("   ")
            {
                out.push(n);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Attach a token to a keyslot.
///
/// # Errors
///
/// Returns an error if writing the scratch token file or `cryptsetup token import` fails.
pub fn import_token(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    token_id: u32,
    token: &RollbackToken,
) -> Result<()> {
    let json = ws.put("token.json", token.to_json()?.as_bytes())?;
    let id_s = token_id.to_string();
    require(
        runner,
        "cryptsetup",
        &argv![
            "token",
            "import",
            "--token-id",
            &id_s,
            "--json-file",
            json.as_os_str(),
            device.as_os_str(),
        ],
        "importing the rollback token",
    )?;
    let _ = std::fs::remove_file(&json);
    Ok(())
}

/// First token id not currently occupied.
///
/// # Errors
///
/// Returns an error if all 32 LUKS2 token slots are occupied, or if probing a slot fails.
pub fn free_token_id(runner: &dyn Runner, device: &Path) -> Result<u32> {
    for id in 0..32u32 {
        let id_s = id.to_string();
        let out = runner.run(
            "cryptsetup",
            &argv!["token", "export", "--token-id", &id_s, device.as_os_str()],
            None,
        )?;
        if !out.ok() {
            return Ok(id);
        }
    }
    bail!(
        "all 32 LUKS2 token slots are occupied on {}",
        device.display()
    )
}

/// SAFETY-CRITICAL: remove one generation's token and keyslot. `proof` must carry a
/// passphrase verified against the device this run, and **not** the one being removed. The
/// token is removed before the keyslot, so an interruption leaves an orphan keyslot
/// (harmless, reclaimable) rather than a token pointing at a slot that no longer exists
/// (which would make `unlock` try to use it).
///
/// # Errors
///
/// Returns an error if `proof` doesn't open `device`, if `proof` is bound to the very slot
/// being removed, if removing the token or keyslot fails, or — the case that most needs
/// shouting about — if `proof` no longer opens the device after the keyslot is gone.
pub fn kill_generation_slot(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    token_id: u32,
    keyslot: u32,
    proof: &[u8],
) -> Result<()> {
    if !verify_passphrase(runner, ws, device, proof, None)? {
        bail!(
            "refusing to remove keyslot {keyslot}: the authorising passphrase does not open {} — \
             removing a slot without a proven alternative is how a machine loses its data",
            device.display()
        );
    }
    // Make sure the proof is not the slot we are about to remove.
    if verify_passphrase(runner, ws, device, proof, Some(keyslot))? {
        bail!(
            "refusing to remove keyslot {keyslot}: the authorising passphrase is the one held in \
             that very slot"
        );
    }
    let id_s = token_id.to_string();
    require(
        runner,
        "cryptsetup",
        &argv!["token", "remove", "--token-id", &id_s, device.as_os_str(),],
        "removing the rollback token",
    )?;
    let kf = ws.put("kill.key", proof)?;
    let slot_s = keyslot.to_string();
    let r = require(
        runner,
        "cryptsetup",
        &argv![
            "luksKillSlot",
            "--batch-mode",
            "--key-file",
            kf.as_os_str(),
            device.as_os_str(),
            &slot_s,
        ],
        "removing the generation keyslot",
    );
    let _ = std::fs::remove_file(&kf);
    r?;
    // SAFETY-CRITICAL: the proof path must still work afterwards.
    if !verify_passphrase(runner, ws, device, proof, None)? {
        bail!(
            "keyslot {keyslot} was removed but the authorising passphrase no longer opens {} — \
             this device needs the recovery key and re-enrollment NOW",
            device.display()
        );
    }
    Ok(())
}

/// One generation to enroll.
#[derive(Debug, Clone, Copy)]
pub struct Enrollment<'a> {
    pub generation: u64,
    /// The floor the generation's `PolicyNV` operand is derived from.
    pub floor: crate::counter::FloorState,
    /// Signed predictions the token carries; at least one is required.
    pub predictions: &'a [Prediction],
    /// PIN to seal with, if any.
    pub pin: Option<&'a [u8]>,
}

/// Build the whole seal + slot + token sequence for one generation. `authorising` is a
/// secret that already opens `device`, used to add the new keyslot.
///
/// # Errors
///
/// Returns an error if `enrollment.predictions` is empty, or if sealing, adding the keyslot,
/// or importing the token fails.
pub fn enroll_generation(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    cfg: &crate::Config,
    enrollment: &Enrollment<'_>,
    authorising: &[u8],
) -> Result<(u32, u32)> {
    let Enrollment {
        generation,
        floor,
        predictions,
        pin,
    } = *enrollment;
    if predictions.is_empty() {
        bail!("refusing to enroll generation {generation} with no signed PCR prediction");
    }
    let key_name = policy::key_name(runner, ws, Path::new(&cfg.pcr_public_key))?;
    let sealed_policy = policy::GenerationPolicy {
        generation,
        key_name: &key_name,
        pcr_bank: &cfg.pcr_bank,
        pcrs: &cfg.pcrs,
        platform_pcrs: &cfg.platform_pcrs,
        nv: policy::NvConstraint {
            index: cfg.nv_index,
            operand: floor.operand_for(generation)?,
        },
        pin: pin.is_some(),
    };
    let digest = policy::seal_policy(runner, ws, &sealed_policy)?;
    let pass = new_passphrase()?;
    let (pu, pr) = seal(runner, ws, &pass, &digest, pin)?;

    let slot = add_generation_slot(runner, ws, device, authorising, &pass, cfg.pbkdf.as_ref())?;
    let token = RollbackToken::new(
        slot,
        &sealed_policy,
        floor.base,
        &SealedObject {
            public: &pu,
            private: &pr,
            policy_digest: &digest,
        },
        predictions,
    );
    let token_id = free_token_id(runner, device)?;
    import_token(runner, ws, device, token_id, &token)?;
    Ok((slot, token_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "\
LUKS header information
Version:       \t2
Epoch:         \t6
Keyslots area: \t16744448 [bytes]
UUID:          \tfd535b11
Data segments:
  0: crypt
\toffset: 16777216 [bytes]
Keyslots:
  0: luks2
\tKey:        512 bits
\tPBKDF:      pbkdf2
  2: luks2
\tKey:        512 bits
Tokens:
  1: tilo-rollback
\tKeyslot:    2
Digests:
  0: pbkdf2
";

    #[test]
    fn keyslot_dumps_parse_only_keyslots_and_secrets_stay_off_argv() {
        // Phase `keyslots_are_parsed_from_a_real_dump`.
        {
            assert_eq!(parse_keyslots(DUMP), vec![0, 2]);
        }
        // Phase `token_and_digest_sections_are_not_mistaken_for_keyslots`.
        {
            // The Tokens section also uses "  N: name"; the parser must stop at it.
            let ks = parse_keyslots(DUMP);
            assert!(
                !ks.contains(&1),
                "token id 1 leaked into the keyslot list: {ks:?}"
            );
        }
        // Phase `an_empty_header_yields_no_keyslots`.
        {
            assert!(parse_keyslots("LUKS header information\nVersion: 2\n").is_empty());
        }
        // Phase `passphrases_are_hex_and_long_enough`.
        {
            let p = new_passphrase().unwrap();
            assert_eq!(p.len(), 64);
            assert!(p.iter().all(u8::is_ascii_hexdigit));
            // Two calls must not agree.
            assert_ne!(*p, *new_passphrase().unwrap());
        }
        // Phase `unseal_pin_auth_references_a_file_instead_of_exposing_the_pin`.
        {
            let auth = unseal_auth(
                Path::new("/run/work/policy.ctx"),
                Some(Path::new("/run/work/pin")),
            );
            assert_eq!(auth, "session:/run/work/policy.ctx+file:/run/work/pin");
            assert!(!auth.contains("246810"));
        }
    }
}
