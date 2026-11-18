//! Following an agent that writes in a long document: the view goes to the words it wrote.

use tilo_rich_edit::BlockContent;
use tilo_ui::State;
use tilo_ui::headless::{FollowedView, FrameHarness};
use vello::kurbo::Rect;

/// An agent's edit of `resource`, a second ago.
fn agent_acting(frame: &mut FrameHarness, resource: &str) -> tilo_actions::SemanticInteraction {
    let mut event = tilo_ui::headless::agent_acting(resource, "edit");
    event.actor = tilo_actions::Acting::agent("scribe");
    event.action.label = "Edit".into();
    event.resource.kind = "example".into();
    event.resource.label = "Same label".into();
    let at = tilo_ui::time::now()
        .checked_sub(tilo_ui::time::Duration::from_secs(1))
        .expect("the clock is later than a second ago");
    frame.update_presence(|presence| presence.apply_interaction_at(event.clone(), at).is_ok());
    event
}

fn long_note() -> tilo_rich_edit::BlockEditor {
    let mut blocks = vec![BlockContent::paragraph("p-0", "A note about the move.")];
    for index in 1..40 {
        blocks.push(BlockContent::paragraph(
            format!("p-{index}"),
            format!("Paragraph {index} of the note."),
        ));
    }
    let mut editor = tilo_rich_edit::BlockEditor::new();
    editor.present_blocks(&blocks);
    editor
}

/// An agent editing text further down a long note is there for every viewer — its cursor rests on
/// the words it wrote, not on whatever control the action is bound to — and a follower's view
/// scrolls to them, once per action.
#[test]
fn following_an_agents_edit_in_text_scrolls_to_the_range_it_wrote() {
    let root = tilo_ui::widget::Scrollable::new(tilo_ui::widget::ScrollAxis::Vertical)
        .with_child(Box::new(long_note()));
    let mut frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
    frame.relayout();
    let event = agent_acting(&mut frame, "notes.document:root");
    let written = 13;
    let block = tilo_rich_edit::block_editor::block_presence_key("p-33");
    frame.update_presence(|presence| {
        presence.set_caret(
            &block.clone(),
            tilo_ui::permissions::MemberId::agent("scribe"),
            written,
        );
        true
    });
    let viewport = Rect::new(0.0, 0.0, 400.0, 240.0);
    let edit = frame
        .semantic_locus(&event)
        .expect("the agent's place in the note");
    assert!(
        edit.y0 > viewport.y1,
        "the edited paragraph starts out of view: {edit:?}"
    );
    assert!(
        edit.height() < 40.0,
        "the cursor is on the line, not the whole note: {edit:?}"
    );
    assert!(!frame.follow_view(
        &tilo_ui::permissions::MemberId::person("someone-else"),
        &mut FollowedView::default()
    ));
    assert_eq!(
        frame.semantic_locus(&event),
        Some(edit),
        "not following moves nothing"
    );
    let mut followed = FollowedView::default();
    let scribe = tilo_ui::permissions::MemberId::agent("scribe");
    assert!(frame.follow_view(&scribe, &mut followed));
    let shown = frame
        .semantic_locus(&event)
        .expect("the agent's place in view");
    assert!(
        viewport.contains(shown.origin()) && shown.y1 <= viewport.y1,
        "the words it wrote are in view: {shown:?}"
    );
    assert!(
        !frame.follow_view(&scribe, &mut followed),
        "each action is gone to once"
    );
}

/// A follower whose view shows an agent's caret in text settles: frame after frame of it goes to
/// the words once, and says nothing of the follower's own presence.
#[test]
fn following_an_agents_caret_in_text_settles() {
    let root = tilo_ui::widget::Scrollable::new(tilo_ui::widget::ScrollAxis::Vertical)
        .with_child(Box::new(long_note()));
    let mut frame = FrameHarness::new(State::new(), Box::new(root), (400, 240));
    frame.relayout();
    let event = agent_acting(&mut frame, "notes.document:root");
    frame.update_presence(|presence| {
        let block = tilo_rich_edit::block_editor::block_presence_key("p-33");
        presence.set_caret(&block, tilo_ui::permissions::MemberId::agent("scribe"), 13);
        true
    });
    let said = tilo_ui::presence::capability::subtree_presence(frame.root_widget());
    let mut followed = FollowedView::default();
    let mut moved = 0;
    for _ in 0..60 {
        moved += usize::from(frame.follow_view(
            &tilo_ui::permissions::MemberId::agent("scribe"),
            &mut followed,
        ));
        frame.relayout();
    }
    assert_eq!(moved, 1, "the view goes to the words once");
    assert!(frame.semantic_locus(&event).is_some());
    assert_eq!(frame.focus_presence(), None, "the follower says nothing");
    assert_eq!(
        tilo_ui::presence::capability::subtree_presence(frame.root_widget()),
        said,
        "showing someone's caret says nothing of the follower's own"
    );
}
