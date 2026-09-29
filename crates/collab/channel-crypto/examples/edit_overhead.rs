//! Per-edit cost of channel encryption: time to seal an op on the sending
//! endpoint and open it on a receiving one, and the bytes it adds.
//!
//! ```bash
//! cargo run --locked --release -p channel-crypto --example edit_overhead
//! ```

#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "a measurement tool that prints its results"
)]

use std::time::{Duration, Instant};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::{ArtifactId, ChannelId, ContentRetention};

const ROUNDS: u32 = 20_000;

fn main() {
    let channel = ChannelId::new();
    let group = ChannelGroup::create(channel, MemberKey::generate("owner").unwrap()).unwrap();
    let keys: ChannelContentKeys = group.content_keys().unwrap();
    let artifact = ArtifactId::new();
    println!("payload_bytes,sealed_bytes,added_bytes,seal_us,open_us");
    for size in [16_usize, 64, 256, 1024, 4096, 65_536] {
        let payload = vec![0x5a_u8; size];
        let (metadata, sealed) = keys
            .seal_content(artifact, ContentRetention::Delta, &payload)
            .unwrap();
        let rounds = if size > 4096 { ROUNDS / 20 } else { ROUNDS };
        let seal = time(rounds, || {
            keys.seal_content(artifact, ContentRetention::Delta, &payload)
                .unwrap();
        });
        let open = time(rounds, || {
            keys.open_content(artifact, &metadata, &sealed).unwrap();
        });
        println!(
            "{size},{},{},{:.2},{:.2}",
            sealed.len(),
            sealed.len() - size,
            micros(seal),
            micros(open)
        );
    }
    // The whole envelope on the wire: a signed op carrying a 64-byte edit,
    // in the clear and sealed.
    let edit = vec![0x5a_u8; 64];
    let author = collab_types::Ed25519PublicKey::from_bytes([7; 32]);
    let plain = collab_types::SignedOp::new_unsigned(
        edit.clone(),
        artifact,
        author,
        collab_types::AttestationId::default(),
        collab_types::OpClass::Splice,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    );
    let (metadata, sealed) = keys
        .seal_content(artifact, ContentRetention::Delta, &edit)
        .unwrap();
    let encrypted = collab_types::SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        author,
        collab_types::AttestationId::default(),
        collab_types::OpClass::Splice,
        std::time::SystemTime::UNIX_EPOCH,
        1,
        metadata,
    );
    let plain = postcard::to_allocvec(&plain).unwrap().len();
    let encrypted = postcard::to_allocvec(&encrypted).unwrap().len();
    println!(
        "signed_op_64b_edit_bytes,plain={plain},encrypted={encrypted},added={}",
        encrypted - plain
    );
}

fn time(rounds: u32, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..rounds {
        f();
    }
    start.elapsed() / rounds
}

fn micros(duration: Duration) -> f64 {
    duration.as_nanos() as f64 / 1000.0
}
