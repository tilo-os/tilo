//! Property: applying the same edit operations on two forked replicas in any
//! order, then cross-merging, yields identical materialized documents. This is
//! the core CRDT correctness guarantee for real-time multi-user editing.
//!
//! The op set deliberately includes **valued marks** (fontSize / fontFamily /
//! link / color) over overlapping ranges so concurrent valued-mark resolution is
//! exercised — not just boolean strong marks. Convergence is asserted two ways:
//! the materialized `DocSnapshot`s must be equal, AND the `.odt` `content.xml`
//! both replicas export must be byte-identical (the codec is a pure function of
//! the converged model, so any non-deterministic materialization would surface).

use proptest::prelude::*;
use tilo_doc::Document;
use tilo_doc_codecs::package::ZipPackage;
use tilo_doc_codecs::{FormatProvider, OdtCodec};

#[derive(Debug, Clone)]
enum Op {
    InsertPara(u8),
    Type(u8, String),
    Bold(u8),
    /// `fontSize` over a sub-range chosen by `(block, frac)`.
    FontSize(u8, u8, u8),
    /// `fontFamily` (cycles serif/sans/mono) over the block.
    Font(u8, u8),
    /// `link` mark carrying an href over a sub-range.
    Link(u8, u8, String),
    /// `color` mark carrying a hex colour over the block.
    Color(u8, u8),
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        any::<u8>().prop_map(Op::InsertPara),
        (any::<u8>(), "[a-z ]{0,6}").prop_map(|(i, s)| Op::Type(i, s)),
        any::<u8>().prop_map(Op::Bold),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(i, sz, f)| Op::FontSize(i, sz, f)),
        (any::<u8>(), any::<u8>()).prop_map(|(i, f)| Op::Font(i, f)),
        (any::<u8>(), any::<u8>(), "https?://[a-z]{1,5}/[a-z]{0,4}")
            .prop_map(|(i, f, u)| Op::Link(i, f, u)),
        (any::<u8>(), any::<u8>()).prop_map(|(i, c)| Op::Color(i, c)),
    ];
    proptest::collection::vec(op, 0..40)
}

/// A sub-range `[start, end)` of a `len`-char block from two byte knobs.
fn subrange(len: usize, a: u8, b: u8) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let modulus = len.saturating_add(1);
    let p = usize::from(a).checked_rem(modulus).unwrap_or(0);
    let q = usize::from(b).checked_rem(modulus).unwrap_or(0);
    (p.min(q), p.max(q))
}

fn apply(doc: &mut Document, ops: &[Op]) {
    for op in ops {
        let blocks = doc.blocks_in_order();
        let pick = |i: u8| {
            let idx = usize::from(i).checked_rem(blocks.len().max(1)).unwrap_or(0);
            blocks.get(idx).cloned()
        };
        let block_len =
            |doc: &Document, b: &str| doc.block_text(b).map_or(0, |t| t.chars().count());
        match op {
            Op::InsertPara(i) => {
                let after = if blocks.is_empty() { None } else { pick(*i) };
                doc.insert_block(after.as_deref(), "paragraph");
            }
            Op::Type(i, s) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    doc.splice_text(&b, len, 0, s);
                }
            }
            Op::Bold(i) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    if len > 0 {
                        doc.add_mark(&b, 0, len, "strong", true);
                    }
                }
            }
            Op::FontSize(i, sz, f) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    let (s, e) = subrange(len, *sz, *f);
                    if e > s {
                        let pts = 8i64.saturating_add(i64::from(*sz) % 24); // 8..=31pt
                        doc.add_mark(&b, s, e, "fontSize", pts);
                    }
                }
            }
            Op::Font(i, f) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    if len > 0 {
                        let fam = ["serif", "sans", "mono"]
                            .get(usize::from(*f) % 3)
                            .copied()
                            .unwrap_or("sans");
                        doc.add_mark(&b, 0, len, "fontFamily", fam);
                    }
                }
            }
            Op::Link(i, f, url) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    let (s, e) = subrange(len, *f, f.wrapping_mul(7));
                    if e > s {
                        doc.add_mark(&b, s, e, "link", url.as_str());
                    }
                }
            }
            Op::Color(i, c) => {
                if let Some(b) = pick(*i) {
                    let len = block_len(doc, &b);
                    if len > 0 {
                        let hex = format!(
                            "#{:02x}{:02x}{:02x}",
                            c,
                            c.wrapping_mul(3),
                            c.wrapping_mul(7)
                        );
                        doc.add_mark(&b, 0, len, "color", hex.as_str());
                    }
                }
            }
        }
    }
}

/// The `content.xml` an exported `.odt` carries — a pure function of the model, so
/// equal materializations must yield byte-identical content.
#[allow(
    clippy::unwrap_used,
    reason = "test fixture helper: a failure here means the codec under test is broken, and proptest reports the failing input"
)]
fn content_xml(doc: &Document) -> Vec<u8> {
    let odt = OdtCodec
        .export(doc.as_ref(), &tilo_content::NoContent)
        .unwrap();
    ZipPackage::read(&odt)
        .unwrap()
        .get("content.xml")
        .unwrap()
        .to_vec()
}

/// Cases per property: `PROPTEST_CASES` when set (the nightly run raises it),
/// otherwise a count that keeps an ordinary test run quick.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn forked_replicas_converge(a_ops in ops_strategy(), b_ops in ops_strategy()) {
        let mut a = Document::new();
        // shared base so both replicas have a common ancestor
        let base = a.insert_block(None, "paragraph");
        a.splice_text(&base, 0, 0, "base");
        let mut b = a.fork();

        apply(&mut a, &a_ops);
        apply(&mut b, &b_ops);

        // cross-merge both directions → both must reach the same state
        a.merge(&mut b);
        b.merge(&mut a);
        a.merge(&mut b); // settle (merge is monotonic; second pass is a no-op if converged)

        prop_assert_eq!(a.materialize(), b.materialize());
        // Stronger: the codec output is byte-identical across the two replicas.
        prop_assert_eq!(content_xml(&a), content_xml(&b));
    }
}
