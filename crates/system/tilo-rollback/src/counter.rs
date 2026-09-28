//! The TPM2 NV monotonic counter that holds the rollback floor.
//!
//! # Index choice
//!
//! [`DEFAULT_NV_INDEX`] sits in the TCG registry's platform-owner range, one block away
//! from `0x01D81000` where `systemd-pcrlock` keeps its own index, so the two cannot collide.
//! It's a [`crate::Config`] field rather than a constant so a machine with something already
//! at that index can move ours.
//!
//! # Why a counter and not ordinary NV storage
//!
//! `TPM2_NV_Increment` on an `nt=counter` index is the only write the TPM allows: no "set to
//! value", no decrement, and undefine/redefine does **not** start over — a newly created
//! counter is initialised to the largest value any counter on that TPM has ever held,
//! specifically to stop that rollback trick. Verified against `swtpm` in `tests/mechanism.rs`.
//!
//! # Floor generation versus counter value
//!
//! A freshly created counter starts at that TPM's high-water mark (1 on a fresh TPM, anything
//! on a used one), not zero, so the floor generation is an offset: `floor_generation =
//! counter_value - base`. `base` is signed and routinely negative — enrolling generation 500
//! on a counter reading 1 gives `base = -497` — since an unsigned `base` would force hundreds
//! of NV writes just to enroll a machine. `base` is fixed at first materialisation and
//! recorded in every sealed LUKS2 token and `/var/lib/tilo/rollback/state.json`.
//!
//! `base` is not integrity-critical: the actual gate is the absolute operand baked into each
//! token's policy digest, which cannot be edited, so a wrong recorded `base` can only make a
//! future seal wrong, never make a stale token unseal. It *is* availability-critical though —
//! a tampered-upward `base` would make [`FloorState::increments_to`] demand a huge number of
//! increments and soft-brick the machine into recovery-key-only. Hence
//! [`MAX_INCREMENTS_PER_ADVANCE`] and the cross-check in [`crate::confirm::load_base`].

use crate::argv;
use crate::exec::{Runner, require};
use crate::policy::Workspace;
use crate::tpm_policy;
use anyhow::{Context, Result, bail};
use std::path::Path;

/// NV index holding the rollback floor.
pub const DEFAULT_NV_INDEX: u32 = 0x01D9_1100;

/// Attributes for the floor counter: `authread` with an empty auth value (the floor isn't
/// secret and every unseal reads it), `policywrite` with **no** `authwrite`/`ownerwrite` so
/// an increment must present a signed, single-use approval (see [`crate::approval`]).
pub const NV_ATTRS: &str = "authread|policywrite|nt=counter";

/// [`NV_ATTRS`] as the bitmask the TPM reports, for computing the index's Name.
pub const NV_ATTRS_BITS: u32 = tpm_policy::nv_attr::AUTHREAD
    | tpm_policy::nv_attr::POLICYWRITE
    | tpm_policy::nv_attr::NT_COUNTER;

/// Size of the counter's data area, in bytes.
pub const NV_DATA_SIZE: u16 = 8;

/// The index's `authPolicy`: a signed approval, or the one-time materialise path.
///
/// ```text
/// PolicyOR(
///     PolicyAuthorize(authority, "tilo-rollback-floor"),
///     PolicyNvWritten(clear) -> PolicyCommandCode(TPM2_NV_Increment),
/// )
/// ```
///
/// The second branch lets a device with no signing key bring the counter into existence; it's
/// self-limiting since the write sets `TPMA_NV_WRITTEN`, after which `PolicyNvWritten(clear)`
/// can never be satisfied again. Proven against a TPM in `tests/mechanism.rs`.
pub fn auth_policy(authority_key_name: &[u8]) -> tpm_policy::Digest256 {
    tpm_policy::or(&[signed_branch(authority_key_name), first_write_branch()])
}

/// The `PolicyAuthorize` branch of [`auth_policy`].
pub fn signed_branch(authority_key_name: &[u8]) -> tpm_policy::Digest256 {
    tpm_policy::authorize(authority_key_name, crate::approval::POLICY_REF)
}

/// The one-time materialise branch of [`auth_policy`].
pub fn first_write_branch() -> tpm_policy::Digest256 {
    tpm_policy::command_code(
        tpm_policy::nv_written(tpm_policy::ZERO, false),
        tpm_policy::CC_NV_INCREMENT,
    )
}

/// The index's TPM Name once it has been written. The written flag lives in the public area,
/// so this differs from the Name before the first increment — every token's `PolicyNV` and
/// every approval must use this one, which is why the counter is materialised before sealing.
pub fn written_name(index: u32, auth_policy: &tpm_policy::Digest256) -> Vec<u8> {
    tpm_policy::nv_index_name(
        index,
        NV_ATTRS_BITS | tpm_policy::nv_attr::WRITTEN,
        auth_policy,
        NV_DATA_SIZE,
    )
}

/// The index's TPM Name before its first write.
pub fn unwritten_name(index: u32, auth_policy: &tpm_policy::Digest256) -> Vec<u8> {
    tpm_policy::nv_index_name(index, NV_ATTRS_BITS, auth_policy, NV_DATA_SIZE)
}

/// Refuse more than this many increments in one advance without an explicit override — a
/// legitimate advance moves by a handful of generations; thousands means `base` is wrong or
/// tampered, and applying it would lock the machine out of its own keyslots.
pub const MAX_INCREMENTS_PER_ADVANCE: u64 = 64;

/// Current floor, in both representations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorState {
    /// Raw counter value in the TPM.
    pub counter: u64,
    /// Counter value corresponding to floor generation 0. Signed, and usually
    /// negative — see the module notes.
    pub base: i64,
}

impl FloorState {
    pub fn floor_generation(&self) -> u64 {
        // `counter` is a u64 and `base` an i64, both widened into i128, so this subtraction
        // stays far inside i128's range — `checked_sub` here is defense in depth, not a real
        // failure path.
        let f = i128::from(self.counter)
            .checked_sub(i128::from(self.base))
            .unwrap_or(0);
        u64::try_from(f.max(0)).unwrap_or(u64::MAX)
    }

    /// Absolute `operandB` for `TPM2_PolicyNV` when sealing for generation `g`; the policy
    /// asserts `counter <= base + g`, i.e. `floor <= g`.
    ///
    /// # Errors
    ///
    /// Returns an error if `generation` maps to a negative NV counter value, or overflows a
    /// `u64` NV counter value.
    pub fn operand_for(&self, generation: u64) -> Result<u64> {
        operand(self.base, generation)
    }

    /// How many `TPM2_NV_Increment` calls move the floor to `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if `target_floor` maps to a counter value below the current one — NV
    /// counters cannot decrement — or if the target generation doesn't fit the counter space.
    pub fn increments_to(&self, target_floor: u64) -> Result<u64> {
        let target_counter = self.operand_for(target_floor)?;
        if target_counter < self.counter {
            bail!(
                "target floor {target_floor} is below the current floor {} — NV counters do not \
                 decrement",
                self.floor_generation()
            );
        }
        // `target_counter >= self.counter` was just checked, so this cannot underflow.
        target_counter
            .checked_sub(self.counter)
            .context("increment count overflowed")
    }
}

/// The counter value that corresponds to floor generation `generation`, shared with the
/// LUKS2 token so the header and TPM can never drift apart on the arithmetic.
///
/// # Errors
///
/// Returns an error if `base + generation` is negative, or does not fit in a `u64`.
pub fn operand(base: i64, generation: u64) -> Result<u64> {
    let v = i128::from(base)
        .checked_add(i128::from(generation))
        .context("counter base and generation are too far apart to relate")?;
    if v < 0 {
        bail!(
            "generation {generation} maps to NV counter value {v}, below zero: this device's \
             recorded counter base ({base}) does not belong to this generation numbering"
        );
    }
    u64::try_from(v).context("generation number overflows the NV counter space")
}

fn idx(index: u32) -> String {
    format!("0x{index:08X}")
}

/// # Errors
///
/// Returns an error if running `tpm2_nvreadpublic` fails to execute (not if the index simply
/// doesn't exist — that's reported as `Ok(false)`).
pub fn exists(runner: &dyn Runner, index: u32) -> Result<bool> {
    let i = idx(index);
    let out = runner.run("tpm2_nvreadpublic", &argv![&i], None)?;
    Ok(out.ok())
}

/// Read the index's TPM Name as the TPM reports it.
///
/// # Errors
///
/// Returns an error if `tpm2_nvreadpublic` fails, its output has no `name:` line, or that
/// line isn't valid hex.
pub fn read_name(runner: &dyn Runner, index: u32) -> Result<Vec<u8>> {
    let i = idx(index);
    let out = require(
        runner,
        "tpm2_nvreadpublic",
        &argv![&i],
        &format!("reading the public area of {i}"),
    )?;
    let text = out.stdout_utf8();
    let hex_name = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("name:"))
        .map(str::trim)
        .next()
        .with_context(|| format!("no name in tpm2_nvreadpublic output for {i}:\n{text}"))?;
    hex::decode(hex_name).with_context(|| format!("NV index name {hex_name:?} is not hex"))
}

/// Define the counter. Idempotent, and verifies that an existing index is really ours.
///
/// SAFETY-CRITICAL, twice over:
///
/// * Never `tpm2_nvundefine` here — undefining the index makes every sealed token
///   permanently unopenable (its policy can't execute at all), turning the recovery key
///   from a backstop into the only path.
/// * An index with different attributes or a different `authPolicy` has a different Name,
///   so every token sealed against the old Name fails to unseal — reported here, by name,
///   rather than discovered at the next boot.
///
/// # Errors
///
/// Returns an error if checking or reading the existing index fails, if an existing index's
/// Name matches neither the expected written nor unwritten Name, or if defining a new index
/// fails.
pub fn define(
    runner: &dyn Runner,
    index: u32,
    auth_policy: &tpm_policy::Digest256,
    ws: &Workspace,
) -> Result<bool> {
    let i = idx(index);
    if exists(runner, index)? {
        let actual = read_name(runner, index)?;
        let expect_written = written_name(index, auth_policy);
        let expect_unwritten = unwritten_name(index, auth_policy);
        if actual != expect_written && actual != expect_unwritten {
            bail!(
                "the NV index at {i} already exists but is not the one this configuration \
                 describes: its Name is {} where {} (unwritten) or {} (written) was expected. \
                 Something else owns that index, or it was created with a different floor authority \
                 key. Tokens sealed against it cannot be unsealed by this build; unlock with the \
                 recovery key and re-enroll, or point --nv-index somewhere else.",
                hex::encode(&actual),
                hex::encode(&expect_unwritten),
                hex::encode(&expect_written),
            );
        }
        return Ok(false);
    }
    let policy_file = ws.put("nv_auth_policy.bin", auth_policy)?;
    require(
        runner,
        "tpm2_nvdefine",
        &argv![&i, "-C", "o", "-a", NV_ATTRS, "-L", policy_file.as_os_str(),],
        &format!("defining the rollback floor counter at {i}"),
    )?;
    Ok(true)
}

/// Read the counter, or `None` if it exists but has never been incremented.
///
/// # Errors
///
/// Returns an error if `tpm2_nvread` fails for a reason other than an unwritten index, if
/// checking whether the index exists fails, if the read output file can't be read back, or
/// if its contents don't decode as a counter value.
pub fn read(runner: &dyn Runner, index: u32, scratch: &Path) -> Result<Option<u64>> {
    let i = idx(index);
    let out_path = scratch.join("nvread.bin");
    let out = runner.run(
        "tpm2_nvread",
        &argv![&i, "-C", &i, "-o", out_path.as_os_str()],
        None,
    )?;
    if !out.ok() {
        // An index that exists but was never written reads as an error; that's the only failure we
        // translate to `None`, and only after confirming the index really is there.
        if exists(runner, index)? {
            return Ok(None);
        }
        bail!(
            "reading the rollback floor counter at {i} failed: {}",
            out.last_error_line()
        );
    }
    let bytes =
        std::fs::read(&out_path).with_context(|| format!("reading {}", out_path.display()))?;
    let _ = std::fs::remove_file(&out_path);
    decode(&bytes)
}

/// Decode the TPM's 8-byte big-endian counter representation.
///
/// # Errors
///
/// Returns an error if `bytes` is shorter than 8 bytes.
pub fn decode(bytes: &[u8]) -> Result<Option<u64>> {
    let head = bytes
        .get(..8)
        .with_context(|| format!("NV counter read returned {} bytes, expected 8", bytes.len()))?;
    let mut v = [0u8; 8];
    v.copy_from_slice(head);
    Ok(Some(u64::from_be_bytes(v)))
}

/// Write the two `PolicyOR` branch digests to files, as tpm2-tools wants them.
fn branch_files(
    ws: &Workspace,
    authority_key_name: &[u8],
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let a = ws.put("branch_signed.bin", &signed_branch(authority_key_name))?;
    let b = ws.put("branch_first.bin", &first_write_branch())?;
    Ok((a, b))
}

/// The one increment that needs no approval: the one that brings the counter into existence.
/// Works exactly once per index, enforced by the TPM.
///
/// # Errors
///
/// Returns an error if any of the `tpm2_*` policy or increment commands fail.
pub fn increment_first(
    runner: &dyn Runner,
    ws: &Workspace,
    index: u32,
    authority_key_name: &[u8],
) -> Result<()> {
    let i = idx(index);
    let (a, b) = branch_files(ws, authority_key_name)?;
    let list = format!("sha256:{},{}", a.display(), b.display());
    let sess = ws.path("nv_first.ctx");
    crate::policy::flush(runner);
    require(
        runner,
        "tpm2_startauthsession",
        &argv!["--policy-session", "-S", sess.as_os_str()],
        "starting the counter's policy session",
    )?;
    let run = || -> Result<()> {
        require(
            runner,
            "tpm2_policynvwritten",
            &argv!["-S", sess.as_os_str(), "c"],
            "asserting the counter has never been written",
        )?;
        require(
            runner,
            "tpm2_policycommandcode",
            &argv!["-S", sess.as_os_str(), "TPM2_CC_NV_Increment"],
            "restricting the session to NV_Increment",
        )?;
        require(
            runner,
            "tpm2_policyor",
            &argv!["-S", sess.as_os_str(), "-l", &list],
            "selecting the materialise branch",
        )?;
        let auth = format!("session:{}", sess.display());
        require(
            runner,
            "tpm2_nvincrement",
            &argv![&i, "-C", &i, "-P", &auth],
            &format!("materialising the rollback floor counter at {i}"),
        )?;
        Ok(())
    };
    let r = run();
    let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
    r
}

/// One increment, authorised by a signed single-use approval. The approval is spent by the
/// TPM, not by this function: after the increment the counter no longer equals `target - 1`,
/// so the same approval can't be reused even if the file survives.
///
/// # Errors
///
/// Returns an error if the approval doesn't validate against the index's TPM Name, if
/// verifying its signature against the authority key fails, or if any of the `tpm2_*` policy
/// or increment commands fail.
pub fn increment_approved(
    runner: &dyn Runner,
    ws: &Workspace,
    index: u32,
    authority_public_key: &std::path::Path,
    authority_key_name: &[u8],
    approval: &crate::approval::Approval,
) -> Result<()> {
    let i = idx(index);
    // SAFETY-CRITICAL: validate against the Name the TPM reports for the index, never against
    // the Name the approval claims for itself — trusting the approval's own copy makes the check
    // vacuous. Caught by `an_approval_for_another_machines_index_is_refused`.
    let nv_name = read_name(runner, index)?;
    approval.validate_for(index, &nv_name)?;
    let policy = approval.decoded_policy()?;
    let signature = approval.decoded_signature()?;

    let ticket = verify_approval_signature(
        runner,
        ws,
        authority_public_key,
        &policy,
        &signature,
        approval,
    )?;

    advance_with_approval(
        runner,
        ws,
        &i,
        authority_key_name,
        approval,
        &policy,
        &ticket,
    )
}

/// Verify the approval's signature against the authority's public key, minting the TPM
/// verification ticket [`advance_with_approval`] spends.
fn verify_approval_signature(
    runner: &dyn Runner,
    ws: &Workspace,
    authority_public_key: &std::path::Path,
    policy: &tpm_policy::Digest256,
    signature: &[u8],
    approval: &crate::approval::Approval,
) -> Result<std::path::PathBuf> {
    // Mint the verification ticket first, with only the authority's public key
    // loaded — the same transient-slot discipline the unseal path uses.
    crate::policy::flush(runner);
    let ak = ws.path("floor_authority.ctx");
    require(
        runner,
        "tpm2_loadexternal",
        &argv![
            "-C",
            "o",
            "-G",
            "rsa",
            "-u",
            authority_public_key.as_os_str(),
            "-c",
            ak.as_os_str(),
        ],
        "loading the floor authority public key",
    )?;
    let message = tpm_policy::authorize_signed_message(policy, crate::approval::POLICY_REF);
    let msg = ws.put("floor_verify_msg.bin", &message)?;
    let sig = ws.put(
        "floor_approval.sig",
        &crate::policy::marshal_rsassa_sha256(signature)?,
    )?;
    let ticket = ws.path("floor_ticket.bin");
    let vs = runner.run(
        "tpm2_verifysignature",
        &argv![
            "-c",
            ak.as_os_str(),
            "-g",
            "sha256",
            "-m",
            msg.as_os_str(),
            "-s",
            sig.as_os_str(),
            "-t",
            ticket.as_os_str(),
        ],
        None,
    )?;
    let _ = runner.run("tpm2_flushcontext", &argv!["-t"], None);
    if !vs.ok() {
        bail!(
            "the floor-advance approval for counter {} is not signed by the configured authority: {}",
            approval.target_counter,
            vs.last_error_line()
        );
    }
    Ok(ticket)
}

/// Run the policy session that spends `ticket` to advance the counter at `i` under `approval`.
fn advance_with_approval(
    runner: &dyn Runner,
    ws: &Workspace,
    i: &str,
    authority_key_name: &[u8],
    approval: &crate::approval::Approval,
    policy: &tpm_policy::Digest256,
    ticket: &Path,
) -> Result<()> {
    let approved_file = ws.put("floor_approved.bin", policy)?;
    let name_file = ws.put("floor_authority.name", authority_key_name)?;
    let ref_file = ws.put("floor_ref.bin", crate::approval::POLICY_REF)?;
    // Guarded by `Approval::validate_for`, called before this by `increment_approved`, which
    // rejects a zero target counter.
    let prior_counter = approval
        .target_counter
        .checked_sub(1)
        .context("approval target counter underflowed")?;
    let operand = ws.put("floor_operand.bin", &prior_counter.to_be_bytes())?;
    let (a, b) = branch_files(ws, authority_key_name)?;
    let list = format!("sha256:{},{}", a.display(), b.display());
    let sess = ws.path("nv_advance.ctx");
    require(
        runner,
        "tpm2_startauthsession",
        &argv!["--policy-session", "-S", sess.as_os_str()],
        "starting the counter's policy session",
    )?;
    let run = || -> Result<()> {
        require(
            runner,
            "tpm2_policycommandcode",
            &argv!["-S", sess.as_os_str(), "TPM2_CC_NV_Increment"],
            "restricting the session to NV_Increment",
        )?;
        let pnv = runner.run(
            "tpm2_policynv",
            &argv![
                "-S",
                sess.as_os_str(),
                "-i",
                operand.as_os_str(),
                i,
                "eq",
                "-C",
                i,
            ],
            None,
        )?;
        if !pnv.ok() {
            bail!(
                "the approval for counter {} does not apply: the counter is not {prior_counter} \
                 right now. An approval authorises exactly one step and is spent once taken, so \
                 this one has already been used or was issued for a different point.",
                approval.target_counter,
            );
        }
        require(
            runner,
            "tpm2_policyauthorize",
            &argv![
                "-S",
                sess.as_os_str(),
                "-i",
                approved_file.as_os_str(),
                "-n",
                name_file.as_os_str(),
                "-q",
                ref_file.as_os_str(),
                "-t",
                ticket.as_os_str(),
            ],
            "applying the signed floor-advance approval",
        )?;
        require(
            runner,
            "tpm2_policyor",
            &argv!["-S", sess.as_os_str(), "-l", &list],
            "selecting the signed-approval branch",
        )?;
        let auth = format!("session:{}", sess.display());
        require(
            runner,
            "tpm2_nvincrement",
            &argv![i, "-C", i, "-P", &auth],
            &format!("advancing the rollback floor counter at {i}"),
        )?;
        Ok(())
    };
    let r = run();
    let _ = runner.run("tpm2_flushcontext", &argv![sess.as_os_str()], None);
    crate::policy::flush(runner);
    r
}

/// Materialise a freshly defined counter and pick `base` so that the floor
/// starts at `initial_floor`.
///
/// # Errors
///
/// Returns an error if materialising or reading the counter fails, or if `counter` and
/// `initial_floor` are too far apart to fit the resulting `base` in an `i64`.
pub fn materialise(
    runner: &dyn Runner,
    ws: &Workspace,
    index: u32,
    scratch: &Path,
    initial_floor: u64,
    authority_key_name: &[u8],
) -> Result<FloorState> {
    if read(runner, index, scratch)?.is_none() {
        increment_first(runner, ws, index, authority_key_name)?;
    }
    let counter = read(runner, index, scratch)?
        .context("the rollback floor counter is still unreadable after an increment")?;
    // `base` is deliberately allowed to go negative: a fresh TPM's counter reads
    // 1, and the generation being enrolled is usually much larger than that.
    let diff = i128::from(counter)
        .checked_sub(i128::from(initial_floor))
        .with_context(|| {
            format!("counter {counter} and initial floor {initial_floor} are too far apart")
        })?;
    let base = i64::try_from(diff).with_context(|| {
        format!(
            "counter {counter} and initial floor {initial_floor} are too far apart to record a \
             counter base"
        )
    })?;
    Ok(FloorState { counter, base })
}

/// Outcome of an attempt to raise the floor.
#[derive(Debug, Clone)]
pub struct AdvanceOutcome {
    pub floor: FloorState,
    /// Steps actually taken.
    pub steps: u64,
    /// Steps that could not be taken because no approval covered them.
    pub unapproved: Vec<u64>,
}

impl AdvanceOutcome {
    pub fn reached_target(&self, target_floor: u64) -> bool {
        self.floor.floor_generation() >= target_floor
    }
}

/// The floor authority's key and the single-use approvals it has signed.
#[derive(Debug, Clone, Copy)]
pub struct FloorApprovals<'a> {
    pub authority_public_key: &'a Path,
    pub authority_key_name: &'a [u8],
    pub approvals: &'a [crate::approval::Approval],
    /// Where spent approvals are discarded from, when they came from disk.
    pub dir: Option<&'a Path>,
}

/// Raise the floor towards `target_floor`, spending one signed approval per step. Advances
/// as far as available approvals allow and reports the rest — a partial advance is safe (the
/// counter only ever moves in the permitted direction), and better than refusing outright,
/// which would leave the floor pinned forever after one missing approval.
///
/// SAFETY-CRITICAL: callers must have already proved that a token at or above `target_floor`
/// unseals. This function does not and cannot check that — see
/// [`crate::confirm::advance_floor`], the only intended caller.
///
/// # Errors
///
/// Returns an error if the number of increments needed to reach `target_floor` exceeds
/// [`MAX_INCREMENTS_PER_ADVANCE`], if an approved increment fails, or if the counter doesn't
/// move after an approved increment.
pub fn advance(
    runner: &dyn Runner,
    ws: &Workspace,
    index: u32,
    scratch: &Path,
    state: FloorState,
    target_floor: u64,
    approvals: &FloorApprovals<'_>,
) -> Result<AdvanceOutcome> {
    let max_increments = MAX_INCREMENTS_PER_ADVANCE;
    let n = state.increments_to(target_floor)?;
    if n == 0 {
        return Ok(AdvanceOutcome {
            floor: state,
            steps: 0,
            unapproved: Vec::new(),
        });
    }
    if n > max_increments {
        bail!(
            "advancing the floor to {target_floor} needs {n} increments, more than the {max_increments} \
             allowed in one step; the recorded counter base ({}) is probably wrong — refusing rather \
             than locking the machine out of its own keyslots",
            state.base
        );
    }
    let mut current = state;
    let mut steps: u64 = 0;
    let mut unapproved = Vec::new();
    for _ in 0..n {
        let next = current
            .counter
            .checked_add(1)
            .context("rollback floor counter is at its maximum value")?;
        let Some(approval) = crate::approval::find(approvals.approvals, next) else {
            unapproved.push(next);
            break;
        };
        increment_approved(
            runner,
            ws,
            index,
            approvals.authority_public_key,
            approvals.authority_key_name,
            approval,
        )?;
        if let Some(d) = approvals.dir {
            crate::approval::discard(d, approval);
        }
        let counter = read(runner, index, scratch)?
            .context("the rollback floor counter became unreadable mid-advance")?;
        if counter <= current.counter {
            bail!(
                "the counter did not move after an approved increment (still {counter}); refusing to \
                 continue"
            );
        }
        current = FloorState {
            counter,
            base: current.base,
        };
        steps = steps.saturating_add(1);
    }
    // Anything still between here and the target had no approval.
    let remaining = current.increments_to(target_floor).unwrap_or(0);
    for k in 0..remaining {
        let want = current
            .counter
            .checked_add(1)
            .and_then(|v| v.checked_add(k))
            .context("rollback floor counter overflowed while listing unapproved steps")?;
        if !unapproved.contains(&want) {
            unapproved.push(want);
        }
    }
    Ok(AdvanceOutcome {
        floor: current,
        steps,
        unapproved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floors_map_counters_through_the_base_without_wrapping() {
        // Phase `floor_is_the_counter_minus_the_base`.
        {
            let s = FloorState {
                counter: 105,
                base: 100,
            };
            assert_eq!(s.floor_generation(), 5);
        }
        // Phase `a_negative_base_is_the_normal_case_on_a_fresh_tpm`.
        {
            // Enrolling generation 500 on a TPM whose counter reads 1.
            let s = FloorState {
                counter: 1,
                base: 1 - 498,
            };
            assert_eq!(s.floor_generation(), 498);
            assert_eq!(s.operand_for(500).unwrap(), 3);
            assert_eq!(s.operand_for(498).unwrap(), 1);
        }
        // Phase `a_base_above_the_counter_reads_as_floor_zero_not_a_wrap`.
        {
            let s = FloorState {
                counter: 3,
                base: 100,
            };
            assert_eq!(s.floor_generation(), 0);
        }
        // Phase `operand_is_absolute`.
        {
            let s = FloorState {
                counter: 105,
                base: 100,
            };
            assert_eq!(s.operand_for(7).unwrap(), 107);
            assert_eq!(s.operand_for(0).unwrap(), 100);
        }
        // Phase `a_generation_that_maps_below_zero_is_refused`.
        {
            let s = FloorState {
                counter: 1,
                base: -400,
            };
            let e = s.operand_for(399).unwrap_err().to_string();
            assert!(e.contains("below zero"), "{e}");
        }
        // Phase `decode_is_big_endian`.
        {
            assert_eq!(decode(&[0, 0, 0, 0, 0, 0, 0, 7]).unwrap(), Some(7));
            assert_eq!(decode(&[1, 0, 0, 0, 0, 0, 0, 0]).unwrap(), Some(1 << 56));
            assert!(decode(&[0, 0]).is_err());
        }
        // Phase `index_is_formatted_the_way_tpm2_tools_expects`.
        {
            assert_eq!(idx(DEFAULT_NV_INDEX), "0x01D91100");
        }
    }

    #[test]
    fn increments_count_the_gap_forward_only_and_never_overflow() {
        // Phase `increments_to_counts_the_gap`.
        {
            let s = FloorState {
                counter: 105,
                base: 100,
            };
            assert_eq!(s.increments_to(5).unwrap(), 0);
            assert_eq!(s.increments_to(8).unwrap(), 3);
        }
        // Phase `increments_to_counts_the_gap_with_a_negative_base`.
        {
            let s = FloorState {
                counter: 1,
                base: -497,
            };
            assert_eq!(s.floor_generation(), 498);
            assert_eq!(s.increments_to(498).unwrap(), 0);
            assert_eq!(s.increments_to(500).unwrap(), 2);
        }
        // Phase `increments_to_refuses_to_go_backwards`.
        {
            let s = FloorState {
                counter: 105,
                base: 100,
            };
            let e = s.increments_to(4).unwrap_err().to_string();
            assert!(e.contains("do not decrement"), "{e}");
        }
        // Phase `operand_overflow_is_an_error`.
        {
            let s = FloorState {
                counter: u64::MAX,
                base: i64::MAX,
            };
            assert!(s.operand_for(u64::MAX).is_err());
        }
    }
}
