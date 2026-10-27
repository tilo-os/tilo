//! Signed, single-use approvals for advancing the rollback floor.
//!
//! # The problem this closes
//!
//! An NV counter whose increment needs only an empty authorisation can be incremented by
//! anything running as root. The floor only moves in the safe direction, so this isn't a
//! route to booting an old image — but it is a denial of service: push the floor above the
//! running generation and the machine unlocks by recovery key until re-enrolled.
//!
//! # The shape
//!
//! The index is defined with `policywrite` and no `authwrite`, with `authPolicy`:
//!
//! ```text
//! PolicyOR(
//!     PolicyAuthorize(floor authority key, policyRef = "tilo-rollback-floor"),
//!     PolicyNvWritten(clear) -> PolicyCommandCode(TPM2_NV_Increment),
//! )
//! ```
//!
//! The second branch materialises the counter before anything can be sealed against it (a
//! device holds no signing key); it works exactly once, since writing the index sets
//! `TPMA_NV_WRITTEN` and `PolicyNvWritten(clear)` then fails forever. Every subsequent
//! increment needs the first branch, and the policy the authority signs is
//!
//! ```text
//! PolicyCommandCode(TPM2_NV_Increment) -> PolicyNV(counter == target - 1)
//! ```
//!
//! which gives three properties, all enforced by the TPM rather than this code:
//!
//! * **Single-use.** After the increment `counter == target - 1` no longer holds, so the
//!   same approval is dead — the TPM refuses it, not just a deleted file.
//! * **Bound to one target.** An approval for `target` cannot produce `target + 5`.
//! * **Bound to one command.** It authorises `TPM2_NV_Increment` and nothing else.
//!
//! # What happens when the authority is unavailable
//!
//! **The floor stops advancing. The disk keeps unlocking.** Unsealing only *reads* the
//! counter (`TPM2_PolicyNV` needs no approval), so an unreachable authority cannot stop a
//! machine booting; confirmation still prunes the keyslot window and reports the advance as
//! pending. Losing the authority key permanently just freezes the floor — recoverable by
//! re-enrolling with a new authority key, which defines a fresh index and re-seals tokens.
//!
//! # The bound that keeps a burnt batch harmless
//!
//! Approvals are signatures, not secrets, so a device holding them (or one later compromised)
//! can spend them. The protection is "root cannot increment past a point that strands the
//! machine": issuance refuses any target floor above the newest generation with a sealed
//! keyslot, so burning every held approval lands the floor exactly where a legitimate
//! confirmation would, with the newest enrolled generation still able to unseal. That bound is
//! computed from the device's own request, so a device that lies about its state can be
//! issued approvals that strand it — but it's already compromised if it's lying; the bound
//! exists to stop a *later* compromise turning a stash of approvals into a bricked machine.

use crate::counter;
use crate::exec::Runner;
use crate::policy::Workspace;
use crate::tpm_policy::{self, Digest256};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Policy reference for the floor-advance `PolicyAuthorize`. Domain separation: a signature
/// for a floor advance must not be usable as a PCR prediction approval, and vice versa, even
/// when the same key is used for both.
pub const POLICY_REF: &[u8] = b"tilo-rollback-floor";

pub const SCHEMA_VERSION: u32 = 1;

/// Where a device keeps the approvals it has been issued.
pub const DEFAULT_DIR: &str = "/var/lib/tilo/rollback/approvals";

/// What a device tells the authority so it can issue approvals. Nothing here is secret — the
/// NV Name and counter value are public facts, and the generation numbers are in the image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub version: u32,
    pub nv_index: u32,
    /// base64 TPM Name of the index, in its written state.
    pub nv_name: String,
    /// Counter value right now.
    pub counter: u64,
    /// Counter value corresponding to floor generation 0.
    pub base: i64,
    /// Floor generation right now.
    pub floor_generation: u64,
    /// Generations that currently have a sealed keyslot, newest first.
    pub enrolled_generations: Vec<u64>,
    pub retained_generations: u32,
    /// The highest floor generation reachable without leaving the machine unable to unseal: the
    /// newest generation that has a keyslot.
    pub safe_target_floor: u64,
}

impl Request {
    /// # Errors
    ///
    /// Returns an error if `nv_name` is not valid base64.
    pub fn decoded_nv_name(&self) -> Result<Vec<u8>> {
        B64.decode(&self.nv_name)
            .context("request nv_name is not base64")
    }
}

/// One signed permission to move the counter to exactly `target_counter`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Approval {
    pub version: u32,
    pub nv_index: u32,
    /// base64 TPM Name the approval is bound to.
    pub nv_name: String,
    /// The counter value this permits the counter to reach.
    pub target_counter: u64,
    /// The floor generation that corresponds to, for humans and for `status`.
    pub target_floor: u64,
    /// base64 approved policy digest.
    pub approved_policy: String,
    /// base64 raw PKCS#1 v1.5 signature over `H(approved_policy || policy_ref)`.
    pub signature: String,
}

impl Approval {
    /// # Errors
    ///
    /// Returns an error if `approved_policy` is not valid base64, or does not decode to a
    /// 32-byte digest.
    pub fn decoded_policy(&self) -> Result<Digest256> {
        let v = B64
            .decode(&self.approved_policy)
            .context("approval approved_policy is not base64")?;
        v.try_into()
            .map_err(|_| anyhow::anyhow!("approval approved_policy is not a 32-byte digest"))
    }

    /// # Errors
    ///
    /// Returns an error if `signature` is not valid base64.
    pub fn decoded_signature(&self) -> Result<Vec<u8>> {
        B64.decode(&self.signature)
            .context("approval signature is not base64")
    }

    /// # Errors
    ///
    /// Returns an error if `nv_name` is not valid base64.
    pub fn decoded_nv_name(&self) -> Result<Vec<u8>> {
        B64.decode(&self.nv_name)
            .context("approval nv_name is not base64")
    }

    /// Check everything checkable without a TPM. Deliberately not a security boundary — the TPM
    /// is — this just catches mundane cases (wrong machine, corrupted file) with a diagnostic
    /// instead of an opaque policy failure.
    pub fn validate_for(&self, nv_index: u32, nv_name: &[u8]) -> Result<()> {
        if self.version != SCHEMA_VERSION {
            bail!(
                "approval schema version {} is not the {SCHEMA_VERSION} this build understands",
                self.version
            );
        }
        if self.nv_index != nv_index {
            bail!(
                "approval is for NV index 0x{:08X}, not 0x{nv_index:08X}",
                self.nv_index
            );
        }
        if self.decoded_nv_name()? != nv_name {
            bail!(
                "approval is bound to a different NV index Name — it was issued for another \
                 machine, or for this index before it was redefined"
            );
        }
        if self.target_counter == 0 {
            bail!("approval targets counter 0, which no counter can be incremented to");
        }
        // Guarded by the check above: target_counter is non-zero here.
        let prior_counter = self
            .target_counter
            .checked_sub(1)
            .context("approval target counter underflowed")?;
        let expected = tpm_policy::nv_counter(
            tpm_policy::command_code(tpm_policy::ZERO, tpm_policy::CC_NV_INCREMENT),
            nv_name,
            prior_counter,
            tpm_policy::Operation::Eq,
        );
        if self.decoded_policy()? != expected {
            bail!(
                "approval's policy digest does not match its stated target counter {} — the file \
                 has been altered, and the TPM would refuse it anyway",
                self.target_counter
            );
        }
        Ok(())
    }

    fn file_name(&self) -> String {
        format!("floor-{}.json", self.target_counter)
    }
}

/// The policy an approval for `target_counter` has to authorise.
///
/// # Errors
///
/// Returns an error if `target_counter` is 0 — no counter can be incremented to it.
pub fn approved_policy(nv_name: &[u8], target_counter: u64) -> Result<Digest256> {
    if target_counter == 0 {
        bail!("a counter cannot be incremented to 0");
    }
    // Guarded by the check above: target_counter is non-zero here.
    let prior_counter = target_counter
        .checked_sub(1)
        .context("target counter underflowed")?;
    Ok(tpm_policy::nv_counter(
        tpm_policy::command_code(tpm_policy::ZERO, tpm_policy::CC_NV_INCREMENT),
        nv_name,
        prior_counter,
        tpm_policy::Operation::Eq,
    ))
}

/// Build the request a device sends to the authority.
///
/// # Errors
///
/// Currently infallible; returns `Result` to match the rest of the approval-issuance
/// pipeline, which chains this with `?`.
pub fn build_request(
    nv_index: u32,
    nv_name: &[u8],
    floor: counter::FloorState,
    enrolled_generations: &[u64],
    retained_generations: u32,
) -> Result<Request> {
    let newest = enrolled_generations.iter().copied().max();
    // SAFETY-CRITICAL: the highest floor that still leaves something able to unseal is the
    // newest generation with a keyslot — above that, every token is below the floor.
    let safe_target_floor = newest.unwrap_or_else(|| floor.floor_generation());
    Ok(Request {
        version: SCHEMA_VERSION,
        nv_index,
        nv_name: B64.encode(nv_name),
        counter: floor.counter,
        base: floor.base,
        floor_generation: floor.floor_generation(),
        enrolled_generations: enrolled_generations.to_vec(),
        retained_generations,
        safe_target_floor,
    })
}

/// Issue approvals covering every step from the current counter up to `up_to_floor`.
/// Requires the authority's private key.
///
/// SAFETY-CRITICAL: refuses to issue past the request's `safe_target_floor` unless
/// explicitly overridden — approvals a device holds are approvals a compromised device
/// can spend.
///
/// # Errors
///
/// Returns an error if the request's schema version doesn't match, if `up_to_floor` is
/// above the request's `safe_target_floor` and `allow_past_safe_bound` is false, if the
/// number of increments needed would exceed [`counter::MAX_INCREMENTS_PER_ADVANCE`], if
/// reading or using the signing key fails, or if a counter/floor value overflows during
/// the arithmetic that relates them.
pub fn issue(
    _runner: &dyn Runner,
    _ws: &Workspace,
    request: &Request,
    private_key_pem: &Path,
    up_to_floor: u64,
    allow_past_safe_bound: bool,
) -> Result<Vec<Approval>> {
    if request.version != SCHEMA_VERSION {
        bail!(
            "request schema version {} is not the {SCHEMA_VERSION} this build understands",
            request.version
        );
    }
    if up_to_floor <= request.floor_generation {
        return Ok(Vec::new());
    }
    if up_to_floor > request.safe_target_floor && !allow_past_safe_bound {
        bail!(
            "refusing to approve floor generation {up_to_floor}: the requesting device reports that \
             {} is the newest generation with a sealed keyslot, so a floor above it would leave \
             nothing able to unseal. Enroll the newer generation first, or pass the override if you \
             intend to force this machine onto its recovery key.",
            request.safe_target_floor
        );
    }
    let nv_name = request.decoded_nv_name()?;
    let next_floor_generation = request
        .floor_generation
        .checked_add(1)
        .context("request floor generation overflowed")?;
    let first = counter::operand(request.base, next_floor_generation)?;
    let last = counter::operand(request.base, up_to_floor)?;
    if last < first {
        return Ok(Vec::new());
    }
    // `last >= first` was just checked, so this subtraction cannot underflow.
    let count = last
        .checked_sub(first)
        .and_then(|span| span.checked_add(1))
        .context("approval batch size overflowed")?;
    if count > counter::MAX_INCREMENTS_PER_ADVANCE {
        bail!(
            "that would be {count} approvals; more than {} in one batch means the request's counter \
             base is wrong",
            counter::MAX_INCREMENTS_PER_ADVANCE
        );
    }

    let key = Zeroizing::new(
        std::fs::read_to_string(private_key_pem)
            .with_context(|| format!("reading {}", private_key_pem.display()))?,
    );
    let mut out = Vec::new();
    for target_counter in first..=last {
        let policy = approved_policy(&nv_name, target_counter)?;
        let message = tpm_policy::authorize_signed_message(&policy, POLICY_REF);
        let sig = tilo_crypto::sign_rsa_sha256(&key, &message).map_err(anyhow::Error::msg)?;
        let target_floor_signed = i128::from(target_counter)
            .checked_sub(i128::from(request.base))
            .context("target counter and request base are too far apart to relate")?;
        let target_floor = u64::try_from(target_floor_signed)
            .context("target counter maps to a negative floor generation")?;
        out.push(Approval {
            version: SCHEMA_VERSION,
            nv_index: request.nv_index,
            nv_name: request.nv_name.clone(),
            target_counter,
            target_floor,
            approved_policy: B64.encode(policy),
            signature: B64.encode(&sig),
        });
    }
    Ok(out)
}

/// Write approvals into a directory.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, or if any approval fails to
/// serialise or write.
pub fn store(dir: &Path, approvals: &[Approval]) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut paths = Vec::new();
    for a in approvals {
        let p = dir.join(a.file_name());
        let json = serde_json::to_string_pretty(a)?;
        tilo_files::native::atomic::write_with_mode(&p, json.as_bytes(), 0o644)
            .with_context(|| format!("atomically writing {}", p.display()))?;
        paths.push(p);
    }
    Ok(paths)
}

/// Load every approval in a directory, oldest target first. A file that doesn't parse is
/// reported and skipped rather than aborting — one corrupt approval must not stop the others.
///
/// # Errors
///
/// Returns an error if the directory exists but cannot be read; a missing directory is not
/// an error and yields an empty list.
pub fn load_all(dir: &Path) -> Result<Vec<Approval>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match std::fs::read_to_string(&p)
            .map_err(anyhow::Error::from)
            .and_then(|t| serde_json::from_str::<Approval>(&t).map_err(anyhow::Error::from))
        {
            Ok(a) => out.push(a),
            Err(e) => eprintln!("tilo-rollback: ignoring {}: {e:#}", p.display()),
        }
    }
    out.sort_by_key(|a| a.target_counter);
    Ok(out)
}

/// The approval for one specific counter value, if the device holds it.
pub fn find(approvals: &[Approval], target_counter: u64) -> Option<&Approval> {
    approvals
        .iter()
        .find(|a| a.target_counter == target_counter)
}

/// Remove a spent approval's file. Best effort — the TPM has already made it useless, so a
/// failure here is untidy rather than dangerous.
pub fn discard(dir: &Path, approval: &Approval) {
    let _ = std::fs::remove_file(dir.join(approval.file_name()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> Vec<u8> {
        let mut v = vec![0x00, 0x0B];
        v.extend_from_slice(&[7u8; 32]);
        v
    }

    fn approval_for(target: u64) -> Approval {
        let p = approved_policy(&name(), target).unwrap();
        Approval {
            version: SCHEMA_VERSION,
            nv_index: 0x01D9_1100,
            nv_name: B64.encode(name()),
            target_counter: target,
            target_floor: target,
            approved_policy: B64.encode(p),
            signature: B64.encode([1u8; 256]),
        }
    }

    #[test]
    fn approvals_bind_one_target_index_and_machine() {
        {
            let a = approved_policy(&name(), 5).unwrap();
            let b = approved_policy(&name(), 6).unwrap();
            assert_ne!(a, b, "each step must need its own approval");
        }
        {
            let other = {
                let mut v = vec![0x00, 0x0B];
                v.extend_from_slice(&[9u8; 32]);
                v
            };
            assert_ne!(
                approved_policy(&name(), 5).unwrap(),
                approved_policy(&other, 5).unwrap()
            );
        }
        {
            assert!(approved_policy(&name(), 0).is_err());
        }
        {
            let a = approval_for(5);
            assert!(a.validate_for(0x01D9_1100, &name()).is_ok());
            let other = {
                let mut v = vec![0x00, 0x0B];
                v.extend_from_slice(&[9u8; 32]);
                v
            };
            let e = a.validate_for(0x01D9_1100, &other).unwrap_err().to_string();
            assert!(e.contains("different NV index Name"), "{e}");
            let e = a
                .validate_for(0x01D9_1200, &name())
                .unwrap_err()
                .to_string();
            assert!(e.contains("NV index"), "{e}");
        }
        {
            let mut a = approval_for(5);
            a.target_counter = 9;
            let e = a
                .validate_for(0x01D9_1100, &name())
                .unwrap_err()
                .to_string();
            assert!(e.contains("does not match its stated target"), "{e}");
        }
    }

    #[test]
    fn approval_bounds_and_directories_follow_enrolled_generations() {
        {
            let floor = counter::FloorState {
                counter: 10,
                base: 5,
            };
            let r = build_request(1, &name(), floor, &[8, 7, 6], 3).unwrap();
            assert_eq!(r.floor_generation, 5);
            assert_eq!(r.safe_target_floor, 8);
        }
        {
            let floor = counter::FloorState {
                counter: 10,
                base: 5,
            };
            let r = build_request(1, &name(), floor, &[], 3).unwrap();
            assert_eq!(r.safe_target_floor, 5);
        }
        {
            let list = vec![approval_for(3), approval_for(4)];
            assert_eq!(find(&list, 4).unwrap().target_counter, 4);
            assert!(find(&list, 9).is_none());
        }
        {
            let d = tempfile::tempdir().unwrap();
            let list = vec![approval_for(4), approval_for(3)];
            store(d.path(), &list).unwrap();
            let back = load_all(d.path()).unwrap();
            assert_eq!(back.len(), 2);
            assert_eq!(back[0].target_counter, 3, "loaded oldest target first");
            discard(d.path(), &back[0]);
            assert_eq!(load_all(d.path()).unwrap().len(), 1);
        }
        {
            let d = tempfile::tempdir().unwrap();
            assert!(load_all(&d.path().join("nope")).unwrap().is_empty());
        }
        {
            let d = tempfile::tempdir().unwrap();
            store(d.path(), &[approval_for(3)]).unwrap();
            std::fs::write(d.path().join("floor-9.json"), b"not json").unwrap();
            let back = load_all(d.path()).unwrap();
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].target_counter, 3);
        }
    }
}
