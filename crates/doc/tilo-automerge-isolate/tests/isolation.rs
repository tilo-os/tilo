//! Untrusted input that stock Automerge panics, spins or over-allocates on
//! costs a worker, never the caller.
//!
//! The inputs below are checksum-valid mutations of real documents, change
//! chunks and bundles that make automerge 0.12.0 / hexane 1.0.0-alpha.5
//! panic or loop forever. Each must be rejected (or accepted) while this
//! process lives on and keeps admitting good input.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::Write as _;
use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc};
use tilo_automerge_isolate::{Applied, Dependencies, Isolate, Limits, Rejected};

tilo_automerge_isolate::test_worker!();

/// An isolate whose deadline only a hang reaches.
fn isolate() -> Isolate {
    Isolate::new(Limits {
        timeout: tilo_test_env::hang_bound(),
        memory_bytes: 512 * 1024 * 1024,
        ..Limits::default()
    })
}

/// An isolate for inputs that spin: its deadline is what stops them.
fn spin_stopping_isolate() -> Isolate {
    Isolate::new(Limits {
        timeout: Duration::from_secs(2),
        memory_bytes: 512 * 1024 * 1024,
        ..Limits::default()
    })
}

fn seeded_doc() -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x51; 16]));
    doc.put(ROOT, "seed", "resident").unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, 1_i64).unwrap();
    doc.commit();
    doc
}

fn remote_change(doc: &mut AutoCommit, key: &str) -> Vec<u8> {
    let mut remote = doc.fork();
    remote.put(ROOT, key, "remote").unwrap();
    remote.commit();
    remote.save_after(&doc.get_heads())
}

/// Good input still goes through after `isolate` rejected something.
fn still_admits(isolate: &Isolate) {
    let mut doc = seeded_doc();
    let change = remote_change(&mut doc, "after");
    assert_eq!(
        isolate.apply(&mut doc, &change, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert!(doc.get(ROOT, "after").unwrap().is_some());
}

const DECODER_CASES: &[(&str, &str)] = &[
    (
        "kind 3 byte 144 ^80: truncated boolean run count: hexane BoolDecoder::next spun forever (plain load hang)",
        "856f4a834eed0e0803df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002017e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105817c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
    ),
    (
        "kind 0 insert 87 0x80: document value lengths exceed the value column: hexane RawColumnIter::take past end (op_iter.rs)",
        "856f4a83ff6b7be500f1010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f805708800109810102830103940103a50107020002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
    (
        "kind 0 byte 104 ^80: truncated varint in a change-graph actor column: hexane RleValue::unpack unwrap (lib.rs)",
        "856f4a833d52effc00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107820002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
    (
        "kind 0 pair 115,116 +1: truncated null count: hexane RleDecoder::advance_run unwrap (rle/decoder.rs)",
        "856f4a83a004450d00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e000280000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
    (
        "kind 0 byte 123 ^1: op actor index past the actor table: ActorMapper::process_actor OOB (op_set2/change.rs)",
        "856f4a833d51cb7a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309010003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
    (
        "kind 0 pair 181,182 +1: non-numeric increment in a document: Verified change iter_ops unwrap (storage/change.rs)",
        "856f4a835de5e29a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0103050703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
    (
        "kind 3 pair 147,148 +1: non-numeric increment in a bundle: Verified change iter_ops unwrap (storage/change.rs)",
        "856f4a830ec77b6303df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002017e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0103050702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
    ),
    (
        "kind 0 pair 132,133 +1: null run in a non-nullable change-graph column: hexane get_null panic",
        "856f4a83a9cba9c000ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f010401800002017f02020002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
    ),
    (
        "kind 0 pair 139,140 +1: dep index past the change list: change_graph.rs ChangeGraphCols::load max_ops[dep] OOB",
        "856f4a8398a104ba00ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f0104007f0002017f02030102010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
    ),
    (
        "kind 0 byte 140 ^80: dep index 256 past the change list: same, larger index",
        "856f4a83e18fbf9900ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f0104007f0002017f02028002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
    ),
    (
        "kind 0 byte 119 ^40: extra-bytes lengths exceed the extra column: change_graph.rs change-meta slice OOB",
        "856f4a83b42dd7f000f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000247000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
    ),
];

const SEQUENCE_GAP_CASES: &[(&str, &str)] = &[
    (
        "kind 3 pair 40,41 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert)",
        "856f4a83e50603e203df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020003027e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
    ),
    (
        "kind 3 byte 64 ^1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert)",
        "856f4a83960a9a3f03aa02000210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f037c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
    ),
    (
        "kind 3 pair 3,41 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert)",
        "856f4a83578227cc03df01000110d2d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002027e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
    ),
    (
        "kind 3 pair 3,64 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert)",
        "856f4a83343c8b3403aa02000210a2a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f037c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
    ),
    (
        "kind 3 byte 64 ^40: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert)",
        "856f4a83aa519aed03aa02000210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f427c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
    ),
];

fn reject_or_admit_every(isolate: &Isolate, cases: &[(&str, &str)]) {
    for (what, input) in cases {
        let input = hex::decode(input).unwrap();
        let mut doc = seeded_doc();
        let before = doc.save();
        let outcome = isolate.apply(&mut doc, &input, Dependencies::RetainOrphans);
        if outcome.is_err() {
            assert_eq!(
                doc.save(),
                before,
                "{what}: a rejection changed the document"
            );
        }
        let _ = isolate.load(&input);
        eprintln!("{what}: {outcome:?}");
    }
    still_admits(isolate);
}

#[test]
fn decoder_panics_and_hangs_cost_only_the_worker() {
    reject_or_admit_every(&spin_stopping_isolate(), DECODER_CASES);
}

#[test]
fn change_graph_sequence_gaps_cost_only_the_worker() {
    reject_or_admit_every(&isolate(), SEQUENCE_GAP_CASES);
}

#[test]
fn a_spinning_decoder_is_stopped_by_the_deadline() {
    let isolate = spin_stopping_isolate();
    let spinning = hex::decode(DECODER_CASES[0].1).unwrap();
    assert_eq!(isolate.load(&spinning).err(), Some(Rejected::TimedOut));
    still_admits(&isolate);
}

/// Frame `body` as a chunk of `kind` with a genuine checksum.
fn chunk(kind: u8, body: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    let mut length = Vec::new();
    let mut n = body.len() as u64;
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            length.push(byte);
            break;
        }
        length.push(byte | 0x80);
    }
    let mut hash = sha2::Sha256::new();
    hash.update([kind]);
    hash.update(&length);
    hash.update(body);
    let mut out = vec![0x85, 0x6f, 0x4a, 0x83];
    out.extend(&hash.finalize()[..4]);
    out.push(kind);
    out.extend(length);
    out.extend(body);
    out
}

#[test]
fn a_compression_bomb_is_stopped_by_the_heap_ceiling() {
    // A compressed change chunk whose body inflates to 1 GiB of zeros.
    let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..1024 {
        deflate.write_all(&zeros).unwrap();
    }
    let bomb = chunk(2, &deflate.finish().unwrap());
    let isolate = Isolate::new(Limits {
        memory_bytes: 256 * 1024 * 1024,
        timeout: tilo_test_env::hang_bound(),
        ..Limits::default()
    });
    let mut doc = seeded_doc();
    let outcome = isolate.apply(&mut doc, &bomb, Dependencies::RetainOrphans);
    assert!(
        matches!(outcome, Err(Rejected::Crashed | Rejected::Invalid(_))),
        "{outcome:?}"
    );
    still_admits(&isolate);
}

#[test]
fn admitted_input_leaves_the_same_document_as_a_direct_apply() {
    use tilo_automerge::ApplyIncoming as _;
    let isolate = isolate();
    let mut doc = seeded_doc();
    let mut direct = doc.clone();
    for round in 0..20 {
        let change = remote_change(&mut doc, &format!("k{round}"));
        if round % 3 == 0 {
            doc.put(ROOT, "local", round).unwrap();
            doc.commit();
            let heads = direct.get_heads();
            direct.load_incoming(&doc.save_after(&heads)).unwrap();
        }
        assert_eq!(
            isolate.apply(&mut doc, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        direct.load_incoming(&change).unwrap();
        assert_eq!(doc.get_heads(), direct.get_heads());
    }
    assert_eq!(doc.save(), direct.save());
}

#[test]
fn missing_dependencies_are_refused_without_a_change_or_queued() {
    let isolate = isolate();
    let mut doc = seeded_doc();
    let mut remote = doc.fork();
    remote.put(ROOT, "first", 1).unwrap();
    remote.commit();
    let base = remote.get_heads();
    remote.put(ROOT, "second", 2).unwrap();
    remote.commit();
    let orphan = remote.save_after(&base);
    let before = doc.save();
    assert_eq!(
        isolate.apply(&mut doc, &orphan, Dependencies::RequireComplete),
        Ok(Applied::MissingDependencies)
    );
    assert_eq!(doc.save(), before);
    assert_eq!(
        isolate.apply(&mut doc, &orphan, Dependencies::RetainOrphans),
        Ok(Applied::Changed(false))
    );
    assert_eq!(doc.get_missing_deps(&[]), base);
    still_admits(&isolate);
}

#[test]
fn blank_document_retains_only_strictly_valid_orphans() {
    let isolate = isolate();
    let mut remote = seeded_doc();
    let dependency = remote.get_heads();
    let committed = remote.save();
    remote.put(ROOT, "later", "pending").unwrap();
    remote.commit();
    let orphan = remote.save_after(&dependency);
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x72; 16]));
    let actor = doc.get_actor().clone();
    let before = doc.save();
    assert_eq!(
        isolate.apply(&mut doc, &orphan, Dependencies::RequireComplete),
        Ok(Applied::MissingDependencies)
    );
    assert_eq!(doc.save(), before);
    let mut corrupt = orphan.clone();
    let mut bad = orphan.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0x55;
    corrupt.extend(bad);
    assert!(
        isolate
            .apply(&mut doc, &corrupt, Dependencies::RetainOrphans)
            .is_err()
    );
    assert_eq!(
        doc.save(),
        before,
        "a malformed tail cannot become an orphan"
    );
    assert_eq!(
        isolate.apply(&mut doc, &orphan, Dependencies::RetainOrphans),
        Ok(Applied::Changed(false))
    );
    assert_eq!(doc.get_missing_deps(&[]), dependency);
    assert_eq!(doc.get_actor(), &actor);
    assert_eq!(
        isolate.apply(&mut doc, &committed, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(doc.get_heads(), remote.get_heads());
    assert_eq!(doc.get_actor(), &actor);
    still_admits(&isolate);
}

#[test]
fn distinct_orphan_queues_rebuild_their_exact_state() {
    let isolate = Isolate::new(Limits {
        workers: 1,
        timeout: tilo_test_env::hang_bound(),
        ..Limits::default()
    });
    let mut seed = seeded_doc();
    let dependency = seed.get_heads();
    let committed = seed.save();
    let mut a = seed.fork();
    a.put(ROOT, "only-a", "first").unwrap();
    a.commit();
    let orphan_a = a.save_after(&dependency);
    let a_heads = a.get_heads();
    a.put(ROOT, "a-tail", "second").unwrap();
    a.commit();
    let tail_a = a.save_after(&a_heads);
    let mut b = seed.fork();
    b.put(ROOT, "only-b", "other queue").unwrap();
    b.commit();
    let orphan_b = b.save_after(&dependency);
    let mut local_a = AutoCommit::new();
    let mut local_b = AutoCommit::new();
    for (doc, bytes) in [(&mut local_a, &orphan_a), (&mut local_b, &orphan_b)] {
        assert_eq!(
            isolate.apply(doc, bytes, Dependencies::RetainOrphans),
            Ok(Applied::Changed(false))
        );
        assert_eq!(doc.get_missing_deps(&[]), dependency);
    }
    let mut finish_a = committed.clone();
    finish_a.extend(tail_a);
    assert_eq!(
        isolate.apply(&mut local_a, &finish_a, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(local_a.get_heads(), a.get_heads());
    assert!(local_a.get(ROOT, "only-b").unwrap().is_none());
    assert_eq!(
        isolate.apply(&mut local_b, &committed, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(local_b.get_heads(), b.get_heads());
    assert!(local_b.get(ROOT, "only-a").unwrap().is_none());
}

#[test]
fn a_malformed_tail_refuses_the_whole_input() {
    let isolate = isolate();
    let mut doc = seeded_doc();
    let mut input = remote_change(&mut doc, "good");
    let mut bad = remote_change(&mut doc, "bad");
    let last = bad.len() - 1;
    bad[last] ^= 0x55;
    input.extend(bad);
    let before = doc.save();
    assert!(matches!(
        isolate.apply(&mut doc, &input, Dependencies::RetainOrphans),
        Err(Rejected::Invalid(_))
    ));
    assert_eq!(doc.save(), before);
}

/// Into a document that is not blank, `load_incremental` stops at a chunk
/// whose stored checksum is wrong and keeps what came before it, without an
/// error. A change with a bad checksum, new or already known, plain or
/// compressed, must refuse the whole input instead.
#[test]
fn a_bad_change_checksum_refuses_the_whole_input() {
    let isolate = isolate();
    let mut doc = seeded_doc();
    let good = remote_change(&mut doc, "good");
    let mut remote = doc.fork();
    remote
        .put(ROOT, "large", "compressible ".repeat(512))
        .unwrap();
    remote.commit();
    let mut last = remote.get_last_local_change().unwrap();
    let compressed = last.bytes().to_vec();
    assert_eq!(compressed[8], 2, "fixture must be a compressed change");
    let known = doc.get_last_local_change().unwrap().raw_bytes().to_vec();
    for (what, prefix, mut bad) in [
        ("new change", good.clone(), remote_change(&mut doc, "bad")),
        ("known change", good.clone(), known),
        ("compressed change", good, compressed),
    ] {
        bad[4] ^= 0x01;
        let mut input = prefix;
        input.extend(bad);
        let before = doc.save();
        let outcome = isolate.apply(&mut doc, &input, Dependencies::RetainOrphans);
        assert!(
            matches!(outcome, Err(Rejected::Invalid(_))),
            "{what}: {outcome:?}"
        );
        assert_eq!(doc.save(), before, "{what}: a refusal changed the document");
    }
    still_admits(&isolate);
}

/// A document big enough that the worker's allocator frees and shrinks heap
/// while rebuilding it, which reaches libc paths small inputs never do.
#[test]
fn a_large_document_is_admitted_by_a_confined_worker() {
    let isolate = isolate();
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "messages", ObjType::List).unwrap();
    for i in 0..10_000 {
        let m = doc.insert_object(&list, i, ObjType::Map).unwrap();
        doc.put(&m, "text", format!("message {i} with some text"))
            .unwrap();
        if i % 100 == 99 {
            doc.commit();
        }
    }
    doc.commit();
    for round in 0..3 {
        let change = remote_change(&mut doc, &format!("k{round}"));
        assert_eq!(
            isolate.apply(&mut doc, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
    }
    assert!(isolate.load(&doc.save()).is_ok());
}

/// A whole history of changes into a blank document, larger than the base
/// deadline alone would allow on a slow machine, is admitted: the deadline
/// grows with the input.
#[test]
fn a_long_history_into_a_blank_document_is_admitted() {
    let mut source = AutoCommit::new().with_actor(ActorId::from(vec![0x33; 16]));
    let text = source.put_object(ROOT, "content", ObjType::Text).unwrap();
    source.commit();
    for round in 0..400 {
        let len = source.length(&text);
        source
            .splice_text(&text, len, 0, &format!("paragraph {round} ").repeat(12))
            .unwrap();
        source.commit();
    }
    let history: Vec<u8> = source
        .get_changes(&[])
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect();
    let isolate = Isolate::new(Limits {
        timeout: tilo_test_env::hang_bound(),
        ..Limits::default()
    });
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x44; 16]));
    assert_eq!(
        isolate.apply(&mut doc, &history, Dependencies::RequireComplete),
        Ok(Applied::Changed(true))
    );
    assert_eq!(doc.get_heads(), source.get_heads());
    // Applying into a blank document keeps the caller's actor.
    assert_eq!(doc.get_actor(), &ActorId::from(vec![0x44; 16]));
}

/// Copies of one document each take a change; the worker serves them all.
#[test]
fn copies_of_one_document_are_each_admitted() {
    let isolate = isolate();
    let base = seeded_doc();
    let mut copies: Vec<AutoCommit> = (0..6).map(|_| base.clone()).collect();
    for (n, doc) in copies.iter_mut().enumerate() {
        let change = remote_change(doc, &format!("copy{n}"));
        assert_eq!(
            isolate.apply(doc, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        assert!(doc.get(ROOT, format!("copy{n}")).unwrap().is_some());
    }
    still_admits(&isolate);
}

#[test]
#[ignore = "timing, run by hand"]
fn admission_latency() {
    let isolate = isolate();
    let mut doc = AutoCommit::new();
    let list = doc.put_object(ROOT, "messages", ObjType::List).unwrap();
    for i in 0..10_000 {
        let m = doc.insert_object(&list, i, ObjType::Map).unwrap();
        doc.put(
            &m,
            "text",
            format!("message {i} with a realistic amount of text"),
        )
        .unwrap();
        doc.commit();
    }
    let mut samples = Vec::new();
    for round in 0..200 {
        let change = remote_change(&mut doc, &format!("k{round}"));
        let started = Instant::now();
        isolate
            .apply(&mut doc, &change, Dependencies::RequireComplete)
            .unwrap();
        samples.push(started.elapsed());
    }
    let first = samples[0];
    samples.sort();
    println!(
        "10k-message document: first admission {first:?} (worker start and full rebuild), then median {:?}, p99 {:?}",
        samples[samples.len() / 2],
        samples[samples.len() * 99 / 100]
    );
}

/// A list `[1]` by actor `aa`: the list is op 1, its item op 2.
const LIST_BASE: &str = "856f4a83ae6ec86200760101aa0127bb838ff217b4f57fe5bfffa75af586b3da7e148ef1e872591780a55722fcc2060102030213022302400256020b01040204130415052102230234024203560357018001027f007f017f027f007f007f0700017f0000017f0100017f007f016c00010200020101017e02017e001401020000";
/// A text `"ab"` by actor `aa`: the text is op 1, `a` op 2, `b` op 3.
const TEXT_BASE: &str = "856f4a83ac2187e60080010101aa011a190725c9e01787574caaf60bf64d2dcea17d67eac3bb9aa2c9ca1318e3c015060102030213022302400256020c010402041104130515052102230234024204560457028001027f007f017f037f007f007f07000102000001020100027f0000017e00027f017400020300030101027f0402017f0002166162030000";

/// A change by the new actor `f0 01` with `ops`, on top of `doc`.
fn crafted(doc: &mut AutoCommit, start: u64, ops: Vec<automerge::legacy::Op>) -> Vec<u8> {
    let change = automerge::ExpandedChange {
        operations: ops,
        actor_id: ActorId::from(&[0xf0, 1][..]),
        hash: None,
        seq: 1,
        start_op: std::num::NonZeroU64::new(start).unwrap(),
        time: 0,
        message: None,
        deps: doc.get_heads(),
        extra_bytes: Vec::new(),
        author: None,
    };
    automerge::Change::from(change).raw_bytes().to_vec()
}

#[test]
fn crafted_sequence_changes_are_refused_without_a_crash() {
    use automerge::legacy::{ElementId, Key, ObjectId, Op, OpId, OpType, SortedVec};
    let isolate = isolate();
    let aa = ActorId::from(&[0xaa][..]);
    let list = ObjectId::Id(OpId(1, aa.clone()));
    let put = |key: ElementId, pred: Vec<OpId>, insert: bool| Op {
        action: OpType::Put(5.into()),
        obj: list.clone(),
        key: Key::Seq(key),
        pred: SortedVec::from(pred),
        insert,
    };
    // The list object itself, which is no element of it.
    let ghost = ElementId::Id(OpId(1, aa.clone()));
    let item = ElementId::Id(OpId(2, aa.clone()));
    let base = hex::decode(LIST_BASE).unwrap();
    let mut doc = AutoCommit::load(&base).unwrap();
    let cases = [
        (
            "an update of an unknown element",
            vec![put(ghost.clone(), vec![], false)],
        ),
        (
            "an insert after an unknown element",
            vec![put(ghost.clone(), vec![], true)],
        ),
        (
            "an update of an unknown element beside a valid one",
            vec![
                put(item, vec![OpId(2, aa.clone())], false),
                put(ghost, vec![], false),
            ],
        ),
    ];
    for (what, ops) in cases {
        let change = crafted(&mut doc, 3, ops);
        let outcome = isolate.apply(&mut doc, &change, Dependencies::RetainOrphans);
        assert!(
            matches!(outcome, Err(Rejected::Invalid(_))),
            "{what}: {outcome:?}"
        );
        assert_eq!(doc.save(), base, "{what}: a rejection changed the document");
    }

    let mut doc = AutoCommit::load(&hex::decode(TEXT_BASE).unwrap()).unwrap();
    let forged_value = Op {
        action: OpType::Put("Q".into()),
        obj: ObjectId::Id(OpId(1, aa.clone())),
        key: Key::Seq(ElementId::Id(OpId(2, aa))),
        pred: SortedVec::from(vec![]),
        insert: false,
    };
    let forged = crafted(&mut doc, 4, vec![forged_value]);
    let mut deleter = doc.fork().with_actor(ActorId::from(&[0xf0, 2][..]));
    let heads = deleter.get_heads();
    let (_, text) = deleter.get(ROOT, "t").unwrap().unwrap();
    deleter.delete(&text, 0).unwrap();
    deleter.commit();
    let delete = deleter.save_after(&heads);
    for change in [forged, delete] {
        assert_eq!(
            isolate.apply(&mut doc, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
    }
    assert_eq!(doc.text(&text).unwrap(), "Qb");
    still_admits(&isolate);
}
