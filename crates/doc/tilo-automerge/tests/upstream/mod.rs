//! The reference the apply tests hold the fork to: crates.io automerge
//! 0.12.0, fed the same bytes in the same deliveries.

use automerge::{AutoCommit, LoadOptions, TextEncoding};
use tilo_automerge::ApplyIncoming;

/// xorshift64*: a reproducible stream per seed.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Deliveries of the changes: mostly one to three at a time, sometimes a
/// burst, sometimes two neighbours swapped so one arrives before its
/// dependency and waits in the queue.
pub fn deliveries(changes: &[Vec<u8>], rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut order: Vec<&Vec<u8>> = changes.iter().collect();
    for i in 1..order.len() {
        if rng.below(12) == 0 {
            order.swap(i - 1, i);
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < order.len() {
        let n = if rng.below(25) == 0 {
            40
        } else {
            1 + rng.below(3) as usize
        };
        out.push(
            order[i..(i + n).min(order.len())]
                .iter()
                .copied()
                .flatten()
                .copied()
                .collect(),
        );
        i += n;
    }
    out
}

fn heads_hex(heads: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut heads: Vec<String> = heads.into_iter().collect();
    heads.sort();
    heads
}

fn upstream_encoding(encoding: TextEncoding) -> upstream_automerge::TextEncoding {
    match encoding {
        TextEncoding::UnicodeCodePoint => upstream_automerge::TextEncoding::UnicodeCodePoint,
        TextEncoding::Utf8CodeUnit => upstream_automerge::TextEncoding::Utf8CodeUnit,
        TextEncoding::Utf16CodeUnit => upstream_automerge::TextEncoding::Utf16CodeUnit,
        TextEncoding::GraphemeCluster => upstream_automerge::TextEncoding::GraphemeCluster,
    }
}

/// How a replica reads the document it is fed.
#[derive(Clone, Copy)]
pub struct Replica {
    /// Arm a diff cursor and compare the patches of every delivery.
    pub diff: bool,
    /// The text encoding patches count indexes in.
    pub encoding: TextEncoding,
}

/// Feeds `changes`, delivered as `seed` shuffles them, on top of `base` to
/// the fork and to crates.io automerge 0.12.0, and asserts they agree after
/// every delivery.
#[expect(
    clippy::disallowed_methods,
    reason = "upstream automerge is the reference here, called as it ships"
)]
pub fn agrees_with_upstream(seed: u64, base: &[u8], changes: &[Vec<u8>], replica: Replica) {
    let mut rng = Rng(seed ^ 0x5555);
    let mut ours =
        AutoCommit::load_with_options(base, LoadOptions::new().text_encoding(replica.encoding))
            .unwrap();
    let mut theirs = upstream_automerge::AutoCommit::load_with_options(
        base,
        upstream_automerge::LoadOptions::new().text_encoding(upstream_encoding(replica.encoding)),
    )
    .unwrap();
    if replica.diff {
        ours.update_diff_cursor();
        theirs.update_diff_cursor();
    }
    for (n, delivery) in deliveries(changes, &mut rng).iter().enumerate() {
        let ours_ok = ours.load_incoming(delivery).map_err(|e| e.to_string());
        let theirs_ok = theirs.load_incremental(delivery).map_err(|e| e.to_string());
        assert_eq!(ours_ok, theirs_ok, "seed {seed}: delivery {n} result");
        assert_eq!(
            heads_hex(ours.get_heads().iter().map(ToString::to_string)),
            heads_hex(theirs.get_heads().iter().map(ToString::to_string)),
            "seed {seed}: heads after delivery {n}"
        );
        if replica.diff {
            assert_eq!(
                format!("{:?}", ours.diff_incremental()),
                format!("{:?}", theirs.diff_incremental()),
                "seed {seed}: patches of delivery {n}"
            );
        }
        if n % 16 == 0 {
            assert_eq!(
                ours.save(),
                theirs.save(),
                "seed {seed}: document after delivery {n}"
            );
        }
    }
    assert_eq!(ours.save(), theirs.save(), "seed {seed}: final document");
    assert_eq!(
        ours.save_nocompress(),
        theirs.save_nocompress(),
        "seed {seed}: final document, uncompressed"
    );
}
