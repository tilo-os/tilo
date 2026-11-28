//! [`TabStrip`] is the window bar's strip as a described component: its pills are
//! the bar's, it reports what the person did with them as the view's own events,
//! and it draws nothing of its own.

use tilo_ui_app::widget::{TabItemProps, TabStrip, TabStripProps};
use tilo_ui_base::tokens::PresenceHue;
use tilo_ui_base::{
    component::Described, component::Event, component::Person, component::ResourceId,
};

fn tab(key: &str, title: &str) -> TabItemProps {
    TabItemProps {
        key: key.to_owned(),
        item: Some(ResourceId(format!("@notes.Note:{key}"))),
        title: Some(title.to_owned()),
        name: None,
        icon: Some(tilo_ui_base::icons::Icon::FileText),
        attention: false,
        modified: false,
        busy: false,
        presence: Vec::new(),
        author: None,
    }
}

fn props(tabs: Vec<TabItemProps>, active: usize) -> TabStripProps {
    TabStripProps {
        key: "frame".to_owned(),
        active: tabs.get(active).and_then(|t| t.item.clone()),
        focused: true,
        activate: Event::None,
        close: Event::None,
        reorder: Event::None,
        detach: Event::None,
        opened_by: Event::None,
        tabs,
    }
}

tilo_test_env::scenario! {
    fn a_tab_strip_is_the_window_bars_strip_pill_for_pill;

    // The pills are the bar's: one per tab, in the view's order, with the active tab in front
    // and each carrying the glyph its type names.
    phase the_strip_is_the_window_bars_strip;
    {
        let tabs = vec![tab("a", "Sorting"), tab("b", "Second"), tab("c", "Third")];
        let strip = TabStrip::from_props("frame", props(tabs, 1));
        let bar = strip.bar();
        let titles: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Sorting", "Second", "Third"]);
        let active = bar.active_id();
        assert_eq!(
            active,
            bar.tabs()[1].id,
            "the view's active tab is the bar's"
        );
        assert!(
            bar.tabs().iter().all(|t| t.app_kind.is_some()),
            "each pill carries its type's glyph"
        );
    }

    // A strip of one tab offers no close chip (the frame closes it); of two or more, each tab
    // does.
    phase a_lone_tab_is_closed_with_its_frame;
    {
        let one = TabStrip::from_props("frame", props(vec![tab("a", "Only")], 0));
        assert!(!one.bar().closable_for_tests());
        let two = TabStrip::from_props("frame", props(vec![tab("a", "A"), tab("b", "B")], 0));
        assert!(two.bar().closable_for_tests());
    }

    // Who is at a resource is presence the strip draws live; a face its view declares says
    // who to show, and never marks the tab as held by someone else, which would hide its
    // close chip and its changed dot.
    phase a_declared_face_never_marks_a_tab_held;
    {
        let mut tabs = vec![tab("a", "A"), tab("b", "B")];
        tabs[0].presence = vec![tilo_ui_app::widget::PresenceFaceProps {
            key: "mia".to_owned(),
            who: Some(Person {
                identity: tilo_actions::MemberId::person("mia").into(),
                name: "Mia".to_owned(),
                hue: PresenceHue::of_identity("mia"),
                picture: None,
            }),
            name: None,
            colour: None,
        }];
        tabs[1].modified = true;
        let strip = TabStrip::from_props("frame", props(tabs, 0));
        let id = strip.bar().tabs()[0].id;
        assert!(
            strip.bar().holder(id).is_none(),
            "a declared face is not who holds the tab"
        );
        assert!(
            strip.bar().closable_for_tests(),
            "the pill keeps its close chip"
        );
        let changed = strip.bar().tabs()[1].id;
        assert!(
            strip.bar().is_modified(changed),
            "and the changed tab keeps its dot"
        );
    }

    // A tab its app is still starting turns an arc on its pill, one waiting for the person
    // carries a dot, and one whose document has unsaved changes keeps its close chip as the
    // mark.
    phase busy_attention_and_changes_show_on_the_pill;
    {
        let mut tabs = vec![tab("a", "A"), tab("b", "B")];
        tabs[0].busy = true;
        tabs[0].attention = true;
        tabs[1].modified = true;
        let strip = TabStrip::from_props("frame", props(tabs, 0));
        let starting = strip.bar().tabs()[0].id;
        let changed = strip.bar().tabs()[1].id;
        assert_eq!(
            strip.bar().mark(starting),
            Some(tilo_ui_app::widget::TabMark::Busy),
            "a window still starting turns a mark on its tab"
        );
        assert!(
            strip.bar().tabs()[0].attention,
            "a window waiting for the person wears its dot"
        );
        assert!(
            strip.bar().is_modified(changed),
            "a changed document's tab keeps its mark"
        );
    }

    // Who else is at a window is a face its view names by name and colour, without a person:
    // presence read from the session's pointer feed.
    phase a_face_named_by_colour_is_drawn_on_the_pill;
    {
        let mut tabs = vec![tab("a", "A")];
        tabs[0].presence = vec![tilo_ui_app::widget::PresenceFaceProps {
            key: "bo".to_owned(),
            who: None,
            name: Some("Bo".to_owned()),
            colour: Some("#3366cc".to_owned()),
        }];
        let strip = TabStrip::from_props("frame", props(tabs, 0));
        let id = strip.bar().tabs()[0].id;
        let faces = strip
            .bar()
            .given_faces(id)
            .expect("a face named by colour reaches the pill");
        assert_eq!(faces.len(), 1, "one person is one face");
        assert_eq!(faces[0].name, "Bo");
        assert_eq!(
            faces[0].rgb,
            Some((0x33, 0x66, 0xcc)),
            "the face wears the colour the view gave it"
        );
    }

    // A window someone else opened leads its tab with their face, in the colour they wear
    // everywhere, and only once even while they are also at it; an agent's face is the agent's
    // mark. A window of the person's own wears no author.
    phase whoever_opened_a_window_leads_its_tab;
    {
        let mut tabs = vec![tab("a", "A"), tab("b", "B"), tab("c", "C")];
        tabs[0].author = Some(tilo_ui_app::widget::TabAuthorProps {
            name: Some("Scribe".to_owned()),
            colour: Some("#2a9d8f".to_owned()),
            agent: true,
            ..Default::default()
        });
        tabs[1].author = Some(tilo_ui_app::widget::TabAuthorProps {
            name: Some("Ada".to_owned()),
            colour: Some("#c85028".to_owned()),
            ..Default::default()
        });
        tabs[1].presence = vec![
            tilo_ui_app::widget::PresenceFaceProps {
                key: "ada".to_owned(),
                who: None,
                name: Some("Ada".to_owned()),
                colour: Some("#c85028".to_owned()),
            },
            tilo_ui_app::widget::PresenceFaceProps {
                key: "bo".to_owned(),
                who: None,
                name: Some("Bo".to_owned()),
                colour: Some("#3366cc".to_owned()),
            },
        ];
        let strip = TabStrip::from_props("frame", props(tabs, 0));
        let agents = strip.bar().tabs()[0].id;
        let adas = strip.bar().tabs()[1].id;
        let owns = strip.bar().tabs()[2].id;
        let ada = strip.bar().author(adas).expect("Ada's face leads her tab");
        assert_eq!(ada.name, "Ada");
        assert_eq!(ada.rgb, Some((0xc8, 0x50, 0x28)), "in her own colour");
        let faces = strip.bar().given_faces(adas).expect("who else is there trails it");
        let names: Vec<&str> = faces.iter().map(|face| face.name.as_str()).collect();
        assert_eq!(names, ["Bo"], "the author once, not again among who is there");
        assert!(strip.bar().author(owns).is_none(), "a window of one's own wears no author");
        assert!(
            strip.bar().holder(adas).is_some_and(|holder| !holder.keeps),
            "she opened it, and it stays this person's to close and move"
        );
        assert!(strip.bar().closable_for_tests(), "its close chip stays");
        let agent = strip.bar().author(agents).expect("the agent's face leads its tab");
        assert!(agent.agent && agent.name == "Scribe", "drawn as the agent: {agent:?}");
        assert_eq!(
            strip.bar().opened_by(agents).map(|mark| (mark.label.as_str(), mark.rgb)),
            Some(("Scribe", (0x2a, 0x9d, 0x8f))),
            "the agent's mark, in its colour"
        );
    }
}

/// A press on a pill is the view's own event in the pass that took it: the strip reports what
/// its bar marked, so a view hears the pill it was pressed for without asking for reports.
#[test]
fn a_press_on_a_pill_is_the_views_event_in_the_same_pass() {
    use tilo_ui_app::widget::Column;
    use tilo_ui_base::widget::Element;
    use tilo_ui_base::{component::Fired, component::Intent, component::Value};

    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut given = props(vec![tab("a", "A"), tab("b", "B")], 0);
    given.activate = Event::View(Intent::new(&queue, "frame", "activate"));
    let strip = TabStrip::from_props("frame", given);
    let column = Column::new().push(Box::new(strip) as Element);
    let mut harness = tilo_ui_app::headless::Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(column),
        (400, 200),
        tilo_ui_base::Theme::light(),
    );
    harness.relayout();
    let pill = {
        let strip = harness.root_widget().children()[0];
        let mut names = Vec::new();
        let found = strip
            .test_rects()
            .into_iter()
            .find(|(name, rect)| {
                names.push(name.clone());
                name == "tab:B" && rect.area() > 0.0
            })
            .map(|(_, rect)| rect);
        found.unwrap_or_else(|| panic!("the second pill is a target; the bar answers to {names:?}"))
    };
    let device_id = tilo_ui_app::winit::event::DeviceId::dummy();
    harness.send_host_event(&tilo_ui_app::winit::event::WindowEvent::CursorMoved {
        device_id,
        position: tilo_ui_app::winit::dpi::PhysicalPosition::new(pill.center().x, pill.center().y),
    });
    for state in [
        tilo_ui_app::winit::event::ElementState::Pressed,
        tilo_ui_app::winit::event::ElementState::Released,
    ] {
        harness.send_host_event(&tilo_ui_app::winit::event::WindowEvent::MouseInput {
            device_id,
            state,
            button: tilo_ui_app::winit::event::MouseButton::Left,
        });
    }
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "one press is one event: {fired:?}");
    assert_eq!(fired[0].event, "activate");
    assert_eq!(
        fired[0].value,
        Some(Value::Resource("@notes.Note:b".to_owned())),
        "the event carries the resource of the pill pressed"
    );
}
