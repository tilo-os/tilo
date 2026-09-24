//! Local-only diagnostic: print admitted Writer heads/text, never key material.
use anyhow::{Context, Result};
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().context("state directory required")?;
    let machine = args.next().context("machine id required")?;
    let store = qos_cloud_host::Store::open(std::path::Path::new(&root), &machine)?;
    let peer = store.peer()?;
    let bytes = peer
        .document("dev.qos.word-processor", "tab:0000000000000001")
        .context("Writer record missing")?;
    let doc = automerge::AutoCommit::load(&bytes)?;
    let model = qdoc::model::open(&doc, &automerge::ROOT)?;
    let text = qdoc::model::blocks_in_order(&doc, &model)
        .iter()
        .filter_map(|id| qdoc::model::block_text(&doc, &model, id))
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::json!({"room":peer.room(),"records":peer.rows(),"writer_text":text})
    );
    Ok(())
}
