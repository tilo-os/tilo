//! TPM 2.0 policy digests, computed without a TPM.
//!
//! A policy digest is a SHA-256 chain: each policy command extends the session's running
//! digest with its own command code and parameters. The rules are in TPM 2.0 Part 3 and are
//! simple enough to implement directly.
//!
//! # Why this exists rather than "just use a trial session"
//!
//! The floor-advance authority holds the signing key and is deliberately not the device. It
//! has to compute the exact policy digest it is about to authorise — signing a digest it
//! cannot compute would mean signing whatever a device asked it to — and requiring a TPM (or
//! emulator) on a build machine to do that is a poor trade, so the digests are computed here.
//!
//! # The obligation that comes with that
//!
//! An implementation of a spec detail that only runs on one side of a protocol is one that
//! drifts. So every function here is checked against a real TPM's own trial-session
//! computation in `tests/mechanism.rs`
//! (`pure_policy_digests_match_the_tpms_own_arithmetic`) — if the TPM and this module ever
//! disagree, that test fails rather than a device silently becoming unable to advance.

use sha2::{Digest, Sha256};

/// Length of a SHA-256 policy digest.
pub const DIGEST_LEN: usize = 32;

pub type Digest256 = [u8; DIGEST_LEN];

/// The starting value of a policy session's digest.
pub const ZERO: Digest256 = [0u8; DIGEST_LEN];

// Command codes (TPM_CC), from TPM 2.0 Part 2.
pub const CC_NV_INCREMENT: u32 = 0x0000_0134;
pub const CC_POLICY_NV: u32 = 0x0000_0149;
pub const CC_POLICY_OR: u32 = 0x0000_0171;
pub const CC_POLICY_COMMAND_CODE: u32 = 0x0000_016C;
pub const CC_POLICY_AUTHORIZE: u32 = 0x0000_016A;
pub const CC_POLICY_AUTH_VALUE: u32 = 0x0000_016B;
pub const CC_POLICY_NV_WRITTEN: u32 = 0x0000_018F;

/// Arithmetic operations for `TPM2_PolicyNV` (`TPM_EO`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Operation {
    Eq = 0x0000,
    Neq = 0x0001,
    UnsignedGt = 0x0003,
    UnsignedLt = 0x0005,
    UnsignedGe = 0x0007,
    UnsignedLe = 0x0009,
}

impl Operation {
    /// The mnemonic tpm2-tools uses on the command line.
    pub fn tool_name(self) -> &'static str {
        match self {
            Operation::Eq => "eq",
            Operation::Neq => "neq",
            Operation::UnsignedGt => "ugt",
            Operation::UnsignedLt => "ult",
            Operation::UnsignedGe => "uge",
            Operation::UnsignedLe => "ule",
        }
    }
}

fn h(parts: &[&[u8]]) -> Digest256 {
    let mut d = Sha256::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().into()
}

/// `TPM2_PolicyCommandCode`: restrict the session to one command.
pub fn command_code(prev: Digest256, code: u32) -> Digest256 {
    h(&[
        &prev,
        &CC_POLICY_COMMAND_CODE.to_be_bytes(),
        &code.to_be_bytes(),
    ])
}

/// `TPM2_PolicyAuthValue`: require the object's authorisation value (the PIN).
pub fn auth_value(prev: Digest256) -> Digest256 {
    h(&[&prev, &CC_POLICY_AUTH_VALUE.to_be_bytes()])
}

/// `TPM2_PolicyNvWritten`: require the NV index's written flag to be set or clear.
pub fn nv_written(prev: Digest256, written: bool) -> Digest256 {
    h(&[
        &prev,
        &CC_POLICY_NV_WRITTEN.to_be_bytes(),
        &[u8::from(written)],
    ])
}

/// `TPM2_PolicyNV`: compare an NV index's contents against an operand. `nv_name` is the
/// index's TPM Name (`nameAlg` followed by the hash of its marshalled public area), which
/// binds the policy to one specific index on one specific TPM.
pub fn nv(
    prev: Digest256,
    nv_name: &[u8],
    operand: &[u8],
    offset: u16,
    op: Operation,
) -> Digest256 {
    // args = H(operandB.buffer || offset || operation)
    let args = h(&[operand, &offset.to_be_bytes(), &(op as u16).to_be_bytes()]);
    h(&[&prev, &CC_POLICY_NV.to_be_bytes(), &args, nv_name])
}

/// `TPM2_PolicyNV` against a `u64` counter value.
pub fn nv_counter(prev: Digest256, nv_name: &[u8], value: u64, op: Operation) -> Digest256 {
    nv(prev, nv_name, &value.to_be_bytes(), 0, op)
}

/// `TPM2_PolicyAuthorize`: accept any policy the named key has signed. This **discards**
/// whatever digest preceded it — the property that makes a signed policy replaceable, and
/// that requires `policy_ref` to carry anything the resulting authorisation must stay bound
/// to (see crate-level notes). The two-step extend is the spec's `PolicyUpdate` helper:
/// command code + key Name, then the policy reference.
pub fn authorize(key_name: &[u8], policy_ref: &[u8]) -> Digest256 {
    let d1 = h(&[&ZERO, &CC_POLICY_AUTHORIZE.to_be_bytes(), key_name]);
    h(&[&d1, policy_ref])
}

/// What the authority signs for a `TPM2_PolicyAuthorize` approval: `H(approvedPolicy || policyRef)`.
pub fn authorize_signed_message(approved_policy: &Digest256, policy_ref: &[u8]) -> Vec<u8> {
    let mut m = approved_policy.to_vec();
    m.extend_from_slice(policy_ref);
    m
}

/// `TPM2_PolicyOR`: satisfy any one of up to eight branches. Also discards the preceding
/// digest, and the branch digests are concatenated without their length prefixes.
pub fn or(branches: &[Digest256]) -> Digest256 {
    let mut d = Sha256::new();
    d.update(ZERO);
    d.update(CC_POLICY_OR.to_be_bytes());
    for b in branches {
        d.update(b);
    }
    d.finalize().into()
}

// TPMA_NV attribute bits, from TPM 2.0 Part 2.
pub mod nv_attr {
    pub const AUTHWRITE: u32 = 0x0000_0004;
    pub const POLICYWRITE: u32 = 0x0000_0008;
    /// `nt=counter`: the index is a monotonic counter (NT field, bits 4-7).
    pub const NT_COUNTER: u32 = 0x0000_0010;
    pub const AUTHREAD: u32 = 0x0004_0000;
    /// Set by the TPM once the index has been written. This bit is inside the public area, so
    /// **an index's Name changes the first time it is written** and is stable afterwards —
    /// anything computing a policy digest for the index must use the written Name, meaning the
    /// counter must be materialised before the first token is sealed. Verified against a TPM in
    /// `tests/mechanism.rs`.
    pub const WRITTEN: u32 = 0x2000_0000;
}

/// `TPM_ALG_SHA256`.
pub const ALG_SHA256: u16 = 0x000B;

/// The TPM Name of an NV index, computed from its public area: `Name = nameAlg ||
/// H_nameAlg(marshal(TPMS_NV_PUBLIC))`, where the marshalled area is `nvIndex ‖ nameAlg ‖
/// attributes ‖ authPolicy (as a TPM2B) ‖ dataSize`. Computing this rather than only reading
/// it back from the TPM lets a mismatch be reported as "this is not the index your tokens
/// were sealed against" instead of surfacing later as an unexplained failure to unseal.
pub fn nv_index_name(index: u32, attributes: u32, auth_policy: &[u8], data_size: u16) -> Vec<u8> {
    let mut marshalled = Vec::new();
    marshalled.extend_from_slice(&index.to_be_bytes());
    marshalled.extend_from_slice(&ALG_SHA256.to_be_bytes());
    marshalled.extend_from_slice(&attributes.to_be_bytes());
    let policy_len = u16::try_from(auth_policy.len()).unwrap_or(u16::MAX);
    marshalled.extend_from_slice(&policy_len.to_be_bytes());
    marshalled.extend_from_slice(auth_policy);
    marshalled.extend_from_slice(&data_size.to_be_bytes());

    let mut name = ALG_SHA256.to_be_bytes().to_vec();
    name.extend_from_slice(&h(&[&marshalled]));
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_digests_bind_every_operation_operand_and_index() {
        {
            assert_eq!(Operation::Eq as u16, 0x0000);
            assert_eq!(Operation::UnsignedLe as u16, 0x0009);
            assert_eq!(Operation::UnsignedLe.tool_name(), "ule");
            assert_eq!(Operation::Eq.tool_name(), "eq");
        }
        {
            let a = command_code(ZERO, CC_NV_INCREMENT);
            assert_ne!(a, ZERO);
            let b = nv_counter(a, &[0u8; 34], 7, Operation::Eq);
            assert_ne!(b, a);
            let c = auth_value(b);
            assert_ne!(c, b);
        }
        {
            let name = [1u8; 34];
            let a = nv_counter(ZERO, &name, 7, Operation::Eq);
            let b = nv_counter(ZERO, &name, 8, Operation::Eq);
            let c = nv_counter(ZERO, &name, 7, Operation::UnsignedLe);
            assert_ne!(a, b, "a different target must give a different policy");
            assert_ne!(a, c, "a different comparison must give a different policy");
        }
        {
            assert_ne!(
                nv_counter(ZERO, &[1u8; 34], 7, Operation::Eq),
                nv_counter(ZERO, &[2u8; 34], 7, Operation::Eq)
            );
        }
        {
            let k1 = [1u8; 34];
            let k2 = [2u8; 34];
            assert_ne!(authorize(&k1, b""), authorize(&k2, b""));
            assert_ne!(authorize(&k1, b"a"), authorize(&k1, b"b"));
            // And it does not depend on anything before it — the whole point, and why the generation
            // lives in the reference.
            assert_eq!(authorize(&k1, b"x"), authorize(&k1, b"x"));
        }
        {
            let a = [1u8; DIGEST_LEN];
            let b = [2u8; DIGEST_LEN];
            assert_ne!(or(&[a, b]), or(&[b, a]));
        }
    }

    #[test]
    fn nv_names_and_signed_messages_match_tpm2_tools() {
        // Phase `nv_attribute_bits_combine_to_the_values_tpm2_tools_reports`.
        {
            use nv_attr::*;
            // `authread|authwrite|nt=counter` reads as 0x40014.
            assert_eq!(AUTHREAD | AUTHWRITE | NT_COUNTER, 0x0004_0014);
            // `authread|policywrite|nt=counter` reads as 0x40018.
            assert_eq!(AUTHREAD | POLICYWRITE | NT_COUNTER, 0x0004_0018);
        }
        {
            use nv_attr::*;
            let attrs = AUTHREAD | POLICYWRITE | NT_COUNTER;
            let unwritten = nv_index_name(0x01D9_1100, attrs, &[0u8; 32], 8);
            let written = nv_index_name(0x01D9_1100, attrs | WRITTEN, &[0u8; 32], 8);
            assert_ne!(unwritten, written);
            assert_eq!(unwritten.len(), 34);
            assert_eq!(&written[..2], &ALG_SHA256.to_be_bytes());
        }
        {
            let p = [7u8; DIGEST_LEN];
            let m = authorize_signed_message(&p, b"ref");
            assert_eq!(m.len(), DIGEST_LEN + 3);
            assert_eq!(&m[..DIGEST_LEN], &p);
            assert_eq!(&m[DIGEST_LEN..], b"ref");
        }
    }
}
