//! Two people opening "the same" document must end up editing the same document.
//! Every other convergence test starts from `fork()` — one document, copied. The
//! hard half is two replicas that each created a document, as happens whenever two
//! apps import the same file: each writes the schema under its own actor, so
//! `root.blocks` is created twice by ops that never saw each other. Automerge
//! resolves that conflict by keeping one op and shadowing the other, and the
//! shadowed side's whole block tree goes with it, the winner decided by an actor-id
//! comparison.

use tilo_doc::Document;

/// A content-bearing document, built the way an import builds one.
fn imported(paragraphs: &[&str]) -> Document {
    let mut d = Document::new();
    let mut prev: Option<String> = None;
    for p in paragraphs {
        let id = d.insert_block(prev.as_deref(), "paragraph");
        d.splice_text(&id, 0, 0, p);
        prev = Some(id);
    }
    d
}

fn texts(d: &Document) -> Vec<String> {
    d.materialize().blocks.into_iter().map(|b| b.text).collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn independently_created_documents_merge_without_displacing_either_side() {
    // Merging two independently created documents keeps both sides' content, run repeatedly because
    // the outcome is decided by a comparison between random actor ids and a single lucky run would
    // read as a pass.
    {
        const ROUNDS: usize = 64;
        let mut lost = 0usize;

        for _ in 0..ROUNDS {
            let mut mine = imported(&["a line of my own"]);
            let mut theirs = imported(&["their own opening line"]);

            mine.merge(&mut theirs);
            theirs.merge(&mut mine);

            let m = texts(&mine);
            let t = texts(&theirs);
            if !(m.iter().any(|s| s.contains("my own"))
                && m.iter().any(|s| s.contains("their own")))
            {
                lost += 1;
            }
            assert_eq!(m, t, "the two replicas must at least agree with each other");
        }

        assert_eq!(
            lost, 0,
            "{lost}/{ROUNDS} merges of two independently-created documents dropped one \
             side's blocks entirely — whichever replica lost the root-key conflict"
        );
    }
    // The same defect from the direction that made it visible: only one side has the image, so when
    // that side loses there is no image block left to draw.
    {
        const ROUNDS: usize = 64;
        let mut lost = 0usize;

        for _ in 0..ROUNDS {
            let mut mine = Document::new();
            let p = mine.insert_block(None, "paragraph");
            mine.splice_text(&p, 0, 0, "the quarterly figures");
            let hash = mine.attach(
                &[0xff, 0xd8, 0xff, 0xe0, 1, 2, 3, 4],
                &mut tilo_content::ContentInputs::new(),
            );
            let img = mine.insert_block(Some(&p), "image");
            mine.as_mut().set_attr(&img, "blob", hash.as_str());

            let mut theirs = imported(&["nothing here yet"]);

            mine.merge(&mut theirs);
            theirs.merge(&mut mine);

            let snap = theirs.materialize();
            let has_image = snap.blocks.iter().any(|b| b.kind == "image");
            let has_bytes = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "image")
                .filter_map(|b| b.attrs.get("blob"))
                .any(|v| match v {
                    tilo_doc::AttrVal::Str(h) => theirs.referenced_content().contains(h),
                    _ => false,
                });
            if !(has_image && has_bytes) {
                lost += 1;
            }
        }

        assert_eq!(
            lost, 0,
            "{lost}/{ROUNDS} merges left the far side without the image block and its \
             bytes — the image cannot render because it is not in the merged document"
        );
    }
    // A third participant, and a peer that joins after there is already content: order of arrival
    // must not decide whose paragraph survives.
    {
        const ROUNDS: usize = 32;
        let mut lost = 0usize;

        for _ in 0..ROUNDS {
            let mut a = imported(&["from alice"]);
            let mut b = imported(&["from bob"]);
            a.merge(&mut b);
            b.merge(&mut a);

            // A third opens the same file only now, after a and b have content.
            let mut c = imported(&["from carol"]);
            c.merge(&mut a);
            a.merge(&mut c);
            b.merge(&mut a);

            let all = texts(&a);
            let complete = ["alice", "bob", "carol"]
                .iter()
                .all(|who| all.iter().any(|s| s.contains(who)));
            if !complete {
                lost += 1;
            }
            assert_eq!(texts(&a), texts(&b), "a and b converge");
            assert_eq!(texts(&a), texts(&c), "a and c converge");
        }

        assert_eq!(
            lost, 0,
            "{lost}/{ROUNDS} three-way merges dropped at least one participant's content"
        );
    }
    // Adopting the genesis must not cost the caller an edit: `init` is reached from `sync`, where
    // the host may already have ops pending in the open transaction, and applying a change into the
    // document from underneath those has to keep them.
    {
        use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "host_wrote_this", "before the schema existed")
            .unwrap();

        let c = tilo_doc::model::init(&mut doc, &ROOT);
        let id = {
            let mut ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
            let mut dm = tilo_doc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "and after");
            id
        };
        doc.commit();

        assert_eq!(
            doc.get(&ROOT, "host_wrote_this")
                .unwrap()
                .and_then(|(v, _)| v.as_str().map(str::to_string)),
            Some("before the schema existed".to_string()),
            "the pending op survived the schema being adopted underneath it"
        );
        assert_eq!(
            tilo_doc::model::block_text(&doc, &c, &id).as_deref(),
            Some("and after"),
            "and the document is editable afterwards"
        );
    }
}
