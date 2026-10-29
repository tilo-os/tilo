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
        ..RichInputProps::default()
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

fn person(identity: &str, name: &str) -> Person {
    Person {
        identity: identity.into(),
        name: name.into(),
    }
}

#[test]
fn the_people_the_system_gives_are_the_at_drawer_who_spoke_last_first_beside_the_other_sources() {
    let _settled = crate::motion::testing::settled();
    let edits = crate::Intents::default();
    let mut input = field(&edits);
    let given = RichInputProps {
        value: input.value.clone(),
        placeholder: Some("Message".into()),
        enabled: true,
        speakers: vec![person("u_bo", "Bo")],
        people: vec![
            person("u_ada", "Ada Lovelace"),
            person("u_bo", "Bo"),
            person("agent:planner", "Planner"),
        ],
        submit: Event::None,
    };
    assert!(input.set_props(given), "the people change in place");
    let mut presence = PresenceCtx::default();
    presence.set_participant_kind(UserId("agent:planner".into()), ParticipantKind::Agent);
    Decorator::draw(
        &mut input,
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &presence,
    );
    let offered = input
        .offered()
        .into_iter()
        .map(|source| {
            let tiers: Vec<(Option<String>, Vec<String>)> = source
                .tiers
                .iter()
                .map(|tier| {
                    (
                        tier.header.clone(),
                        tier.candidates.iter().map(|c| c.key.clone()).collect(),
                    )
                })
                .collect();
            (source.trigger, tiers)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        offered,
        [
            (
                '#',
                vec![(
                    None,
                    vec!["topic:release notes".to_owned(), "topic:design".to_owned()]
                )]
            ),
            (
                '@',
                vec![
                    (
                        Some("In this conversation".to_owned()),
                        vec!["u_bo".to_owned()]
                    ),
                    (
                        Some("Members".to_owned()),
                        vec!["u_ada".to_owned(), "agent:planner".to_owned()]
                    ),
                ]
            ),
        ],
        "the system's `#` stays, and `@` offers who spoke, then the other members"
    );
    type_text(&mut input, "@pla");
    dispatch(
        &mut input,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
    );
    let rich = last_value(&edits).expect("the pick reported the rich text");
    assert_eq!(
        rich.blocks[0].marks[0].value,
        Attr::Text("agent:planner".into())
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

#[test]
fn with_submit_enter_sends_what_was_written_and_breaks_no_line() {
    let edits = crate::Intents::<Fired>::default();
    let submits = crate::Intents::<Fired>::default();
    let props = RichInputProps {
        value: Bound {
            edits: Some(Intent::new(&edits, "draft", "value")),
            ..Bound::default()
        },
        placeholder: Some("Message".into()),
        submit: Event::View(Intent::new(&submits, "draft", "submit")),
        ..RichInputProps::default()
    };
    let mut input = RichInput::from_props("draft", props);
    Widget::set_focus(&mut input, true);
    type_text(&mut input, "On all three");
    dispatch(
        &mut input,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
    );
    assert_eq!(submits.take().len(), 1, "Enter sends");
    let written = last_value(&edits).expect("what was typed");
    assert_eq!(line_of(&written).0, "On all three", "no line was broken");
}
