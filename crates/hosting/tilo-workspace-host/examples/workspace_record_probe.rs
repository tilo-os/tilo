//! Read a workspace record a relay wrote (version 4), field by field, to say
//! exactly which field a build's decoder stops at.
//!
//! usage: `workspace_record_probe FILE`
#![allow(clippy::expect_used, reason = "an example fails loudly")]

use serde::Deserialize;
use std::path::PathBuf;
use tilo_peer_state::join::invitations::IssuedInvitation;
use tilo_peer_state::join::{InviteGrant, JoinBootstrap, JoinChallenge, MembershipChallenge};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct Record {
    version: u32,
    workspace: String,
    workspace_id: WorkspaceId,
    name: String,
    authority: DeviceCert,
    peer: Vec<Part>,
    generation: u64,
    outgoing: Vec<Publication>,
    published: usize,
    collaboration: Ledger,
    /// How many publications and join receipts the files beside the record
    /// hold (`<record>.published`, `<record>.joins`): the record holds those
    /// after them.
    filed_publications: u64,
    filed_receipts: u64,
}

#[derive(Deserialize)]
struct Ledger {
    invitations: Vec<InvitationRecord>,
    join_challenges: Vec<JoinChallengeRecord>,
    join_completions: Vec<JoinCompletionRecord>,
    membership_challenges: Vec<MembershipChallengeRecord>,
    administration: Vec<AdministrationRecord>,
}

#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
enum Part {
    Inline(Vec<u8>),
    Stored { digest: [u8; 32], len: u64 },
}

#[derive(Debug, Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct Publication {
    bytes: Vec<u8>,
    checkpoint: bool,
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct CancellationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct InvitationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    issued: IssuedInvitation,
    grant: InviteGrant,
    cancellation: Option<CancellationRecord>,
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct JoinChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: JoinChallenge,
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct JoinCompletionRecord {
    remote_endpoint: [u8; 32],
    request_id: [u8; 32],
    proof_digest: [u8; 32],
    bootstrap: JoinBootstrap,
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct MembershipChallengeRecord {
    remote_endpoint: [u8; 32],
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: MembershipChallenge,
}

#[derive(Deserialize)]
#[expect(dead_code, reason = "read to prove the bytes decode")]
struct AdministrationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    generation: u64,
}

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: probe FILE"));
    let bytes = std::fs::read(&path).expect("read the record");
    println!("{}: {} bytes", path.display(), bytes.len());
    match postcard::from_bytes::<Record>(&bytes) {
        Ok(record) => println!(
            "the whole record reads: version {}, workspace {}, {} publications, {} peer parts, \
             {} invitations, {} join challenges, {} completions, {} membership challenges, {} administration; \
             {} publications and {} completions in the files beside it",
            record.version,
            record.workspace,
            record.outgoing.len(),
            record.peer.len(),
            record.collaboration.invitations.len(),
            record.collaboration.join_challenges.len(),
            record.collaboration.join_completions.len(),
            record.collaboration.membership_challenges.len(),
            record.collaboration.administration.len(),
            record.filed_publications,
            record.filed_receipts
        ),
        Err(error) => println!("the whole record FAILS: {error}"),
    }
    let mut rest: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    macro_rules! step {
        ($ty:ty, $name:literal) => {
            match postcard::take_from_bytes::<$ty>(rest) {
                Ok((_value, tail)) => {
                    println!("  {:<28} ok ({} bytes left)", $name, tail.len());
                    rest = tail;
                }
                Err(error) => {
                    println!("  {:<28} FAILS: {error}", $name);
                    return;
                }
            }
        };
    }
    step!(u32, "version");
    step!(String, "workspace");
    step!(u64, "workspace_id");
    step!(String, "name");
    step!(DeviceCert, "authority");
    step!(Vec<Part>, "peer");
    step!(u64, "generation");
    step!(Vec<Publication>, "outgoing");
    step!(usize, "published");
    // The invitations, one field at a time: the count, then the first
    // record's fields, then the rest as a whole.
    let before = rest.len();
    let count = match postcard::take_from_bytes::<usize>(rest) {
        Ok((count, tail)) => {
            rest = tail;
            println!(
                "  {:<28} ok, {count} invitation(s)",
                "ledger.invitations.len"
            );
            count
        }
        Err(error) => {
            println!("  {:<28} FAILS: {error}", "ledger.invitations.len");
            return;
        }
    };
    for index in 0..count {
        println!("  invitation {index}:");
        step!([u8; 32], "request_id");
        step!([u8; 32], "request_digest");
        step!(IssuedInvitation, "issued");
        step!(InviteGrant, "grant");
        step!(Option<CancellationRecord>, "cancellation");
    }
    if before == rest.len() {
        println!("  ({count} invitations took no bytes?)");
    }
    step!(Vec<JoinChallengeRecord>, "ledger.join_challenges");
    step!(Vec<JoinCompletionRecord>, "ledger.join_completions");
    step!(
        Vec<MembershipChallengeRecord>,
        "ledger.membership_challenges"
    );
    step!(Vec<AdministrationRecord>, "ledger.administration");
    step!(u64, "filed_publications");
    step!(u64, "filed_receipts");
    println!("  every field reads; {} bytes left", rest.len());
}
