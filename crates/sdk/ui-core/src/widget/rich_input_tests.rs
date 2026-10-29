#![allow(clippy::indexing_slicing, reason = "test assertions")]

use super::*;
use crate::component::{Fired, Intent};
use crate::testing::dispatch;
use crate::widget::MentionCandidate;
use crate::widget::completion::{Candidate, CompletionSource, Tier};

fn topic(name: &str) -> InlineView {
    InlineView::Resource {
        title: name.to_owned(),
        icon: None,
        detail: None,
    }
}

/// A composer whose system sources offer two topics for `#`, and the room's Ada for `@`.
fn field(edits: &crate::Intents<Fired>) -> RichInput {
    let props = RichInputProps {
        value: Bound {
            edits: Some(Intent::new(edits, "draft", "value")),
            ..Bound::default()
        },
        placeholder: Some("Message".into()),
        enabled: true,
    };
    let mut input = RichInput::from_props("draft", props);
    input.set_sources(vec![CompletionSource::new(
        '#',
        vec![Tier::new(vec![
            Candidate::reference("release notes", "#release notes", "topic:release notes"),
            Candidate::reference("design", "#design", "topic:design"),
        ])],
    )]);
    input
        .line
        .inner_mut()
        .set_mention_candidates(vec![MentionCandidate::new("u_ada", "Ada Lovelace", false)]);
    Widget::set_focus(&mut input, true);
    input
}

fn type_text(input: &mut RichInput, text: &str) {
    for character in text.chars() {
        dispatch(
            input,
            &crate::synthetic_input::char_key(&character.to_string()),
        );
    }
}

fn last_value(edits: &crate::Intents<Fired>) -> Option<RichText> {
    edits
        .take()
        .into_iter()
        .filter_map(|fired| match fired.value {
            Some(Value::Rich(rich)) => Some(rich),
            _ => None,
        })
        .next_back()
}

#[test]
fn a_picked_reference_goes_in_as_a_reference_with_how_it_reads() {
    let _settled = crate::motion::testing::settled();
    let edits = crate::Intents::default();
    let mut input = field(&edits);
    type_text(&mut input, "see #rel");
    dispatch(
        &mut input,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Tab),
    );
    type_text(&mut input, "now");
    let rich = last_value(&edits).expect("each edit reports the rich text");
    // The run as written: how a reference reads depends on its reader.
    assert_eq!(rich.blocks[0].text, "see #release notes now");
    let block = &rich.blocks[0];
    assert_eq!(block.marks.len(), 1, "{:?}", block.marks);
    let mark = &block.marks[0];
    assert_eq!(mark.name, "mention");
    assert_eq!(mark.value, Attr::Text("topic:release notes".into()));
    assert_eq!(
        (mark.start, mark.end),
        (4, 18),
        "the run of `#release notes`, in characters"
    );
    assert_eq!(rich.embeds.len(), 1);
    assert_eq!(rich.embeds[0].view, topic("release notes"));
}

#[test]
fn a_person_from_the_room_goes_in_as_a_mention_of_their_participant_id() {
    let _settled = crate::motion::testing::settled();
    let edits = crate::Intents::default();
    let mut input = field(&edits);
    type_text(&mut input, "@ad");
    dispatch(
        &mut input,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
    );
    let rich = last_value(&edits).expect("the pick reported the rich text");
    assert_eq!(crate::widget::rich_doc::plain_text(&rich), "@Ada Lovelace ");
    assert_eq!(rich.blocks[0].marks[0].value, Attr::Text("u_ada".into()));
    assert_eq!(
        rich.embeds[0].view,
        InlineView::Person {
            identity: "u_ada".into(),
            name: "Ada Lovelace".into()
        }
    );
}

#[test]
fn a_value_cleared_elsewhere_empties_the_field() {
    let _settled = crate::motion::testing::settled();
    let edits = crate::Intents::default();
    let mut input = field(&edits);
    type_text(&mut input, "hello");
    assert_eq!(crate::widget::rich_doc::plain_text(&input.rich()), "hello");
    input.value.state.set(RichText::default());
    assert!(input.follow(), "the host's empty draft is shown");
    assert_eq!(input.rich(), RichText::default());
}

#[test]
fn rich_text_reads_back_as_the_text_and_marks_it_was_shown_with() {
    let rich = rich_of(
        "ping @Ada\nsecond",
        &[MarkSpan::new(5..9, Mark::Mention("u_ada".into()))],
        &HashMap::from([(
            "u_ada".to_owned(),
            InlineView::Person {
                identity: "u_ada".into(),
                name: "Ada".into(),
            },
        )]),
    );
    assert_eq!(rich.blocks.len(), 2);
    assert_eq!(rich.blocks[0].marks[0].start, 5);
    let (text, marks) = line_of(&rich);
    assert_eq!(text, "ping @Ada\nsecond");
    assert_eq!(marks, [MarkSpan::new(5..9, Mark::Mention("u_ada".into()))]);
}
