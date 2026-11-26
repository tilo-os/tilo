//! `outline`: what an app's window shows now, as the lines an agent reads.
//!
//! The lines are the view's own semantic tree ([`ui_core::semantic_outline`]), read from the
//! widgets the window shows now, so a change the person makes — or an agent's own action —
//! shows in the next outline. Every host answers it: an app running in a page shows its
//! lines as one running on a computer does. A picture of the view is drawn where the host
//! can draw one ([`PICTURE_ASKED`]); a page cannot read a frame back, so it says so instead
//! of leaving the caller wondering.

use tilo_actions::ActionOutput;
use ui_core::automerge::AutoCommit;
use ui_core::automerge::transaction::Transactable as _;
use ui_core::widget::Widget;

/// What `content` shows now, as the operation answers it, with no picture of it.
pub(crate) fn answer(
    content: &dyn Widget,
    doc: &AutoCommit,
    theme: &ui_core::Theme,
) -> ActionOutput {
    let outline = ui_core::semantic_outline::outline(content);
    let rect = content.get_rect();
    let value = serde_json::json!({
        "outline": outline.text,
        "tree": outline.tree,
        "nodes": outline.nodes,
        "omitted": outline.omitted,
        "viewport": [rect.width().round(), rect.height().round()],
        // Where the view sits in the app's window, in window pixels: a point of
        // a line's `rect` plus this is where it stands for a person.
        "window_origin": [rect.x0.round(), rect.y0.round()],
        "theme": theme.name,
        "revision": revision(doc),
    });
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

/// The document's heads, as a revision a caller compares: the version of what the view shows.
pub(crate) fn revision(doc: &AutoCommit) -> String {
    let mut heads: Vec<String> = doc
        .base_heads()
        .iter()
        .map(|head| head.to_string().chars().take(12).collect())
        .collect();
    heads.sort();
    heads.join("+")
}
