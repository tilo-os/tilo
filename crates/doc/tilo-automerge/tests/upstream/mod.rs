//! The reference the apply tests hold the fork to: crates.io automerge
//! 0.12.0, fed the same bytes in the same deliveries.

use std::panic::{AssertUnwindSafe, catch_unwind};

use automerge::{AutoCommit, LoadOptions, TextEncoding};
use tilo_automerge::ApplyIncoming;

/// A base document, the changes actors made on top of it, and the
/// hand-built changes among them that no Automerge makes, kept apart for
/// [`refuses_only_what_upstream_corrupts`].
pub struct History {
    pub base: Vec<u8>,
    pub changes: Vec<Vec<u8>>,
    pub forged: Vec<Vec<u8>>,
}

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

/// `sound` and `sound_upstream`, for the fork and for crates.io automerge
/// 0.12.0. The saved document loads. Its change log, replayed into a new
/// document in one batch, as a peer that joins later receives it, makes the
/// same document; and so does it one change at a time, the change whose
/// hash is `first` as early as its dependencies allow, each change offered
/// again while it is refused, as sync offers a peer what it lacks. And a
/// change that writes after the last key of each of its maps, where it
/// follows every op of that map, applies.
macro_rules! soundness {
    ($name:ident, $am:ident, $apply:ident) => {
        pub fn $name(saved: &[u8], first: [u8; 32]) -> bool {
            use std::collections::{HashMap, HashSet};
            use $am::transaction::Transactable as _;
            use $am::{AutoCommit, ObjType, ROOT, ReadDoc as _, Value};
            catch_unwind(AssertUnwindSafe(|| {
                let Ok(mut doc) = AutoCommit::load(saved) else {
                    return false;
                };
                let same = |joined: &mut AutoCommit, doc: &mut AutoCommit| {
                    joined.get_heads() == doc.get_heads()
                        && joined.document().hydrate(None) == doc.document().hydrate(None)
                };
                let log = doc.get_changes(&[]);
                let mut joined = AutoCommit::new();
                let mut replayed =
                    joined.$apply(log.clone()).is_ok() && same(&mut joined, &mut doc);

                let index: HashMap<[u8; 32], usize> = log
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.hash().0, i))
                    .collect();
                let mut early = HashSet::new();
                let mut stack: Vec<usize> = index.get(&first).copied().into_iter().collect();
                while let Some(i) = stack.pop() {
                    if early.insert(i) {
                        stack.extend(log[i].deps().iter().filter_map(|d| index.get(&d.0)));
                    }
                }
                let (mut pending, rest): (Vec<_>, Vec<_>) = log
                    .into_iter()
                    .enumerate()
                    .partition(|(i, _)| early.contains(i));
                pending.extend(rest);
                let mut joined = AutoCommit::new();
                while replayed && !pending.is_empty() {
                    let taken = (0..pending.len()).find(|&i| {
                        let change = &pending[i].1;
                        change
                            .deps()
                            .iter()
                            .all(|d| joined.get_change_by_hash(d).is_some())
                            && joined.$apply([change.clone()]).is_ok()
                    });
                    replayed = taken.map(|i| pending.remove(i)).is_some();
                }
                replayed &= same(&mut joined, &mut doc);

                let (mut objects, mut maps) = (vec![ROOT], Vec::new());
                while let Some(obj) = objects.pop() {
                    let children = match doc.object_type(&obj) {
                        Ok(ObjType::Map) => {
                            maps.push(obj.clone());
                            let keys = doc.keys(&obj).collect::<Vec<_>>();
                            keys.into_iter()
                                .filter_map(|key| doc.get(&obj, key).ok().flatten())
                                .collect::<Vec<_>>()
                        }
                        _ => (0..doc.length(&obj))
                            .filter_map(|i| doc.get(&obj, i).ok().flatten())
                            .collect(),
                    };
                    for (value, id) in children {
                        if let Value::Object(_) = value {
                            objects.push(id);
                        }
                    }
                }
                let mut writer = doc.fork();
                for map in &maps {
                    writer.put(map, "\u{10ffff}", 0).unwrap();
                }
                writer.commit();
                let written = writer.get_changes(&doc.get_heads());
                replayed && doc.$apply(written).is_ok()
            }))
            .unwrap_or(false)
        }
    };
}

soundness!(sound, automerge, apply_incoming);
#[expect(
    clippy::disallowed_methods,
    reason = "upstream automerge is the reference here, called as it ships"
)]
mod upstream_soundness {
    use super::{AssertUnwindSafe, catch_unwind};
    soundness!(sound_upstream, upstream_automerge, apply_changes);
}
use upstream_soundness::sound_upstream;

/// Feeds a history's changes on top of its base to the fork and to crates.io
/// automerge 0.12.0, then each of its forged changes on its own. The fork
/// either takes a forged change as upstream does, into a document that is
/// [`sound`], or refuses it and keeps its document byte for byte, where
/// upstream panics or takes it into a document that is not. Returns how
/// many the fork refused.
#[expect(
    clippy::disallowed_methods,
    reason = "upstream automerge is the reference here, called as it ships"
)]
pub fn refuses_only_what_upstream_corrupts(seed: u64, history: &History) -> usize {
    let History {
        base,
        changes,
        forged,
    } = history;
    let all = changes.concat();
    let mut ours = AutoCommit::load(base).unwrap();
    ours.load_incoming(&all).unwrap();
    let mut theirs = upstream_automerge::AutoCommit::load(base).unwrap();
    theirs.load_incremental(&all).unwrap();
    let before = ours.save();
    assert_eq!(
        before,
        theirs.save(),
        "seed {seed}: document before the forged changes"
    );
    let mut refused = 0;
    for (n, change) in forged.iter().enumerate() {
        let parsed = automerge::Change::try_from(change.as_slice()).unwrap();
        let (hash, decoded) = (parsed.hash().0, parsed.decode());
        let mut taken = ours.clone();
        let mut upstream = theirs.clone();
        let upstream_took = catch_unwind(AssertUnwindSafe(|| {
            upstream.load_incremental(change).map_err(|e| e.to_string())
        }));
        match taken.load_incoming(change) {
            Ok(ops) => {
                let saved = taken.save();
                assert!(
                    sound(&saved, hash),
                    "seed {seed}: took {decoded:?} into an unsound document"
                );
                // Where upstream panics, the fork applies what upstream
                // could not; elsewhere it applies the same.
                if let Ok(upstream_took) = upstream_took {
                    assert_eq!(upstream_took, Ok(ops), "seed {seed}: forged {n}");
                    assert_eq!(saved, upstream.save(), "seed {seed}: forged {n}");
                }
            }
            Err(error) => {
                refused += 1;
                assert_eq!(taken.save(), before, "seed {seed}: forged {n} left a trace");
                let upstream_sound =
                    matches!(upstream_took, Ok(Ok(_))) && sound_upstream(&upstream.save(), hash);
                assert!(
                    !upstream_sound,
                    "seed {seed}: refused ({error}) {decoded:?}, which upstream takes soundly"
                );
            }
        }
    }
    refused
}
