use super::*;
use tilo_ui_base::{component::Fired, component::Intent};
use ui::testing::dispatch;
use tilo_ui_base::text_render::TextRenderer;
use ui::widget::MentionCandidate;
use ui::widget::completion::{Candidate, CompletionSource, Tier};
use vello::Scene;
use vello::kurbo::Affine;

fn topic(name: &str) -> InlineView {
    InlineView::Resource {
        title: name.to_owned(),
        icon: None,
        detail: None,
    }
}

/// A composer whose system sources offer two topics for `#`, and the workspace's Ada for `@`.
fn field(edits: &tilo_ui_base::Intents<Fired>) -> RichInput {
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
        .set_mention_candidates(vec![MentionCandidate::new(
            MemberId::person("u_ada"),
            "Ada Lovelace",
            tilo_ui_base::tokens::PresenceHue::of_identity("u_ada"),
        )]);
    Widget::set_focus(&mut input, true);
    input
}

fn type_text(input: &mut RichInput, text: &str) {
    for character in text.chars() {
        dispatch(
            input,
            &tilo_ui_base::synthetic_input::char_key(&character.to_string()),
        );
    }
}

fn last_value(edits: &tilo_ui_base::Intents<Fired>) -> Option<RichText> {
    edits
        .take()
        .into_iter()
        .filter_map(|fired| match fired.value {
            Some(Value::Rich(rich)) => Some(rich),
            _ => None,
        })
        .next_back()
}

tilo_test_env::scenario! {
    /// What a rich input takes in, shows and reads back.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_rich_input_takes_references_mentions_values_and_submit;

    phase a_picked_reference_goes_in_as_a_reference_with_how_it_reads;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let edits = tilo_ui_base::Intents::default();
        let mut input = field(&edits);
        type_text(&mut input, "see #rel");
        dispatch(
            &mut input,
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Tab),
        );
        type_text(&mut input, "now");
        let rich = last_value(&edits).expect("each edit reports the rich text");
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

    phase a_person_from_the_workspace_goes_in_as_a_mention_of_their_member_id;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let edits = tilo_ui_base::Intents::default();
        let mut input = field(&edits);
        type_text(&mut input, "@ad");
        dispatch(
            &mut input,
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
        );
        let rich = last_value(&edits).expect("the pick reported the rich text");
        assert_eq!(crate::rich_doc::plain_text(&rich), "@Ada Lovelace ");
        assert_eq!(
            rich.blocks[0].marks[0].value,
            Attr::Text(reference(&MemberId::person("u_ada")))
        );
        assert_eq!(
            rich.embeds[0].view,
            InlineView::Person {
                identity: MemberId::person("u_ada"),
                name: "Ada Lovelace".into()
            }
        );
    }

    phase the_people_the_system_gives_are_the_at_drawer_who_spoke_last_first_beside_the_other_sources;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let edits = tilo_ui_base::Intents::default();
        let mut input = field(&edits);
        let given = RichInputProps {
            value: input.value.clone(),
            placeholder: Some("Message".into()),
            enabled: true,
            speakers: vec![person(&MemberId::person("u_bo"), "Bo")],
            people: vec![
                person(&MemberId::person("u_ada"), "Ada Lovelace"),
                person(&MemberId::person("u_bo"), "Bo"),
                person(&MemberId::agent("planner", "u_bo"), "Planner"),
            ],
            submit: Event::None,
        };
        assert!(input.set_props(given), "the people change in place");
        let presence = PresenceCtx::default();
        Decorator::draw(&mut input, &mut tilo_ui_base::DrawCtx::new(&mut Scene::new(), Affine::IDENTITY, &mut TextRenderer::new(), &Theme::dark(), presence.draw_env()));
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
                            vec![reference(&MemberId::person("u_bo"))]
                        ),
                        (
                            Some("Members".to_owned()),
                            vec![
                                reference(&MemberId::person("u_ada")),
                                reference(&MemberId::agent("planner", "u_ada"))
                            ]
                        ),
                    ]
                ),
            ],
            "the system's `#` stays, and `@` offers who spoke, then the other members"
        );
        type_text(&mut input, "@pla");
        dispatch(
            &mut input,
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
        );
        let rich = last_value(&edits).expect("the pick reported the rich text");
        assert_eq!(
            rich.blocks[0].marks[0].value,
            Attr::Text(reference(&MemberId::agent("planner", "u_ada")))
        );
    }

    phase a_value_cleared_elsewhere_empties_the_field;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let edits = tilo_ui_base::Intents::default();
        let mut input = field(&edits);
        type_text(&mut input, "hello");
        assert_eq!(crate::rich_doc::plain_text(&input.rich()), "hello");
        input.value.state.set(RichText::default());
        assert!(input.follow(), "the host's empty draft is shown");
        assert_eq!(input.rich(), RichText::default());
    }

    phase the_field_answers_to_the_key_its_node_has_in_the_composed_tree;
    {
        // The host finds the field by this id to give it the keyboard and to type what a press
        // left waiting into it: a field with no id is one nothing can reach.
        let edits = tilo_ui_base::Intents::<Fired>::default();
        let input = field(&edits);
        assert_eq!(Widget::test_id(&input), Some("draft"));
    }

    phase rich_text_reads_back_as_the_text_and_marks_it_was_shown_with;
    {
        let rich = rich_of(
            "ping @Ada\nsecond",
            &[MarkSpan::new(5..9, Mark::Mention(MemberId::person("u_ada")))],
            &HashMap::from([(
                reference(&MemberId::person("u_ada")),
                InlineView::Person {
                    identity: MemberId::person("u_ada"),
                    name: "Ada".into(),
                },
            )]),
        );
        assert_eq!(rich.blocks.len(), 2);
        assert_eq!(rich.blocks[0].marks[0].start, 5);
        let (text, marks) = line_of(&rich);
        assert_eq!(text, "ping @Ada\nsecond");
        assert_eq!(marks, [MarkSpan::new(5..9, Mark::Mention(MemberId::person("u_ada")))]);
    }

    phase with_submit_enter_sends_what_was_written_and_breaks_no_line;
    {
        let edits = tilo_ui_base::Intents::<Fired>::default();
        let submits = tilo_ui_base::Intents::<Fired>::default();
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
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
        );
        assert_eq!(submits.take().len(), 1, "Enter sends");
        let written = last_value(&edits).expect("what was typed");
        assert_eq!(line_of(&written).0, "On all three", "no line was broken");
    }
}

fn person(identity: &MemberId, name: &str) -> Person {
    Person::new(identity.clone(), name)
}

/// The value a mention of `member` is stored as.
fn reference(member: &MemberId) -> String {
    tilo_reference::Embed::Member(member.clone()).mark_value()
}
