//! Property: applying the same edit operations on two forked replicas in any
//! order, then cross-merging, yields identical materialized documents. This is
//! the core CRDT correctness guarantee for real-time multi-user editing.

use proptest::prelude::*;
use qdoc::Document;

#[derive(Debug, Clone)]
enum Op {
    InsertPara(u8),
    Type(u8, String),
    Bold(u8),
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        any::<u8>().prop_map(Op::InsertPara),
        (any::<u8>(), "[a-z ]{0,6}").prop_map(|(i, s)| Op::Type(i, s)),
        any::<u8>().prop_map(Op::Bold),
    ];
    proptest::collection::vec(op, 0..40)
}

fn apply(doc: &mut Document, ops: &[Op]) {
    for op in ops {
        let blocks = doc.blocks_in_order();
        match op {
            Op::InsertPara(i) => {
                let after = if blocks.is_empty() {
                    None
                } else {
                    Some(blocks[(*i as usize) % blocks.len()].clone())
                };
                doc.insert_block(after.as_deref(), "paragraph");
            }
            Op::Type(i, s) => {
                if !blocks.is_empty() {
                    let b = &blocks[(*i as usize) % blocks.len()];
                    let len = doc.block_text(b).map(|t| t.chars().count()).unwrap_or(0);
                    doc.splice_text(b, len, 0, s);
                }
            }
            Op::Bold(i) => {
                if !blocks.is_empty() {
                    let b = &blocks[(*i as usize) % blocks.len()];
                    let len = doc.block_text(b).map(|t| t.chars().count()).unwrap_or(0);
                    if len > 0 {
                        doc.add_mark(b, 0, len, "strong", true);
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

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
    }
}
