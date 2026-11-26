use super::*;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui_base::widget::Widget as _;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::keyboard::NamedKey;

fn lay_out_and_draw(widget: &mut ReactionStripWidget, width: f32, theme: &Theme) {
    let mut taffy = taffy::TaffyTree::<()>::new();
    let mut node_map = HashMap::new();
    let mut text = TextRenderer::new();
    let node = widget.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            |known, available, id, _, _| {
                node_map
                    .get(&id)
                    // SAFETY: the map holds `widget`, borrowed for this call.
                    .map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(known, available, &mut text)
                    })
            },
        )
        .expect("layout");
    widget.apply_layout(&taffy, node, Point::new(8.0, 8.0));
    let mut scene = Scene::new();
    widget.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
}

fn click(widget: &mut ReactionStripWidget, at: Point) {
    crate::testing::dispatch(
        widget,
        &tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y),
    );
    crate::testing::dispatch(widget, &tilo_ui_base::synthetic_input::mouse_press());
    crate::testing::dispatch(widget, &tilo_ui_base::synthetic_input::mouse_release());
}

fn reactor(by: &str) -> Reactor {
    Reactor {
        by: tilo_actions::MemberId::person(by),
        persona: None,
        name: None,
    }
}

fn group(emoji: &str, by: &[&str]) -> ReactionGroup {
    ReactionGroup {
        emoji: emoji.to_owned(),
        reactors: by.iter().map(|by| reactor(by)).collect(),
        first_at: 0,
    }
}

fn names(reactor: &Reactor) -> String {
    match reactor.by.id() {
        "ada" => "Ada".into(),
        "bo" => "Bo".into(),
        other => other.to_uppercase(),
    }
}

tilo_test_env::scenario! {
    fn a_reaction_strip_wraps_toggles_and_names_its_pills;

    phase pills_wrap_within_the_width_and_the_add_button_follows_them;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let metrics = StripMetrics::of(&theme);
        let groups: Vec<ReactionGroup> = ["👍", "🎉", "❤️", "😂", "🔥", "👀"]
            .iter()
            .map(|emoji| group(emoji, &["ada"]))
            .collect();
        let wide = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::new(10.0, 20.0), 2000.0),
            &mut text,
            &metrics,
        );
        assert_eq!(wide.pills().len(), 6);
        assert!(
            wide.pills().iter().all(|pill| pill.rect.y0 == 20.0),
            "a wide strip is one row"
        );
        assert_eq!(wide.height(), metrics.height());
        let add = wide.add_rect().expect("the person may react");
        assert!(add.x0 > wide.pills()[5].rect.x1, "add comes last");

        let narrow_width = wide.pills()[2].rect.x1 - 10.0;
        let narrow = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::new(10.0, 20.0), narrow_width),
            &mut text,
            &metrics,
        );
        assert!(narrow.height() > metrics.height() * 2.0, "it wrapped");
        for pill in narrow.pills() {
            assert!(pill.rect.x1 <= 10.0 + narrow_width + 0.01, "{pill:?} fits");
            assert!(pill.rect.x0 >= 10.0);
        }
        assert_eq!(
            narrow.pills()[3].rect.x0,
            10.0,
            "a new row starts at the edge"
        );
        assert_eq!(
            measure_height(&groups, &[], true, narrow_width, &mut text, &theme),
            narrow.height()
        );
        assert_eq!(measure_height(&[], &[], false, 300.0, &mut text, &theme), 0.0);
    }

    phase a_pill_toggles_its_emoji_and_the_add_button_anchors_the_picker;
    {
        let theme = Theme::light();
        let mut text = TextRenderer::new();
        let groups = vec![group("👍", &["ada", "me"]), group(":tilo:", &["bo"])];
        let laid = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 400.0),
            &mut text,
            &StripMetrics::of(&theme),
        );
        let [thumbs, tilo] = laid.pills() else {
            panic!("two pills");
        };
        assert!(thumbs.mine && !tilo.mine, "your own reaction is yours");
        assert_eq!(thumbs.count, 2);
        assert_eq!(
            laid.hit(thumbs.rect.center()),
            Some(StripHit::Toggle("👍".into()))
        );
        assert_eq!(
            laid.hit(tilo.rect.center()),
            Some(StripHit::Toggle(":tilo:".into()))
        );
        let add = laid.add_rect().expect("add");
        assert_eq!(laid.hit(add.center()), Some(StripHit::Add { anchor: add }));
        assert_eq!(laid.targets().len(), 3);
        assert!(
            tilo.rect.width() > thumbs.rect.width(),
            "a workspace emoji without a picture shows its :name:"
        );

        let read_only = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            false,
            (Point::ZERO, 400.0),
            &mut text,
            &StripMetrics::of(&theme),
        );
        assert!(read_only.add_rect().is_none());
        assert!(read_only.hit(read_only.pills()[0].rect.center()).is_none());
        assert!(read_only.targets().is_empty());
    }

    phase the_tooltip_names_who_reacted_you_first;
    {
        let mut text = TextRenderer::new();
        let mut bridged = group("🙌", &["ada", "bo", "cy", "me", "dee"]);
        bridged.reactors.push(Reactor {
            by: tilo_actions::MemberId::bot("bridge"),
            persona: Some("discord/1".into()),
            name: Some("Eve".into()),
        });
        let groups = vec![group("🎉", &["ada", "bo"]), bridged, group("🔥", &["me"])];
        let laid = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 600.0),
            &mut text,
            &StripMetrics::of(&Theme::dark()),
        );
        let tip = |index: usize| {
            laid.tooltip_at(laid.pills()[index].rect.center(), &names)
                .expect("a pill names who reacted")
                .1
        };
        assert_eq!(tip(0), "Ada and Bo reacted with party popper");
        assert_eq!(tip(1), "You, Ada and 4 others reacted with raising hands");
        assert_eq!(tip(2), "You reacted with fire");
        let add = laid.add_rect().expect("add");
        assert_eq!(
            laid.tooltip_at(add.center(), &names).map(|(_, tip)| tip),
            Some("Add reaction".into())
        );
    }

    phase every_pill_is_a_toggle_named_by_what_it_does;
    {
        let mut text = TextRenderer::new();
        let groups = vec![group("👍", &["ada", "me"]), group("🎉", &["bo"])];
        let laid = layout(
            &groups,
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 600.0),
            &mut text,
            &StripMetrics::of(&Theme::dark()),
        );
        let nodes = laid.accessibility();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].role, Role::ToggleButton);
        assert_eq!(nodes[0].name.as_deref(), Some("Remove 👍 reaction"));
        assert_eq!(nodes[0].value.as_deref(), Some("2 reactions"));
        assert_eq!(nodes[0].state.checked, Some(true));
        assert_eq!(nodes[1].name.as_deref(), Some("Add 🎉 reaction"));
        assert_eq!(nodes[1].value.as_deref(), Some("One reaction"));
        assert_eq!(nodes[1].state.checked, Some(false));
        assert_eq!(nodes[2].role, Role::Button);
        assert_eq!(nodes[2].name.as_deref(), Some("Add reaction"));
    }

    phase a_read_only_strip_shows_the_pills_and_takes_no_reaction;
    {
        let theme = Theme::dark();
        let mut widget = ReactionStripWidget::new(vec![group("👍", &["ada"])], Some(tilo_actions::MemberId::person("me"))).can_react(false);
        lay_out_and_draw(&mut widget, 600.0, &theme);
        let laid = widget.strip().laid_out().clone();
        assert!(laid.add_rect().is_none());
        click(&mut widget, laid.pills()[0].rect.center());
        assert!(widget.take_toggles().is_empty());
        assert!(laid.hit(laid.pills()[0].rect.center()).is_none());
        assert!(laid.targets().is_empty());
        assert!(!widget.is_focusable());
    }

    phase a_pill_arriving_after_the_first_layout_appears_and_a_count_cross_fades;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut strip = ReactionStrip::new();
        strip.layout(
            &[group("👍", &["ada"])],
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 400.0),
            &mut text,
            &theme,
        );
        assert!(!strip.is_moving(), "what is there at first rests");
        strip.layout(
            &[group("👍", &["ada", "bo"]), group("🎉", &["bo"])],
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 400.0),
            &mut text,
            &theme,
        );
        let now = tilo_ui_base::motion::frame_now();
        let arriving = strip.motion.get("🎉").expect("the new pill moves");
        assert!(arriving.presence.is_moving(now), "the new pill appears");
        assert!(
            strip
                .motion
                .get("👍")
                .is_some_and(|pill| pill.count.is_moving(now)),
            "the changed count cross-fades"
        );
        let reduced = theme.clone().with_reduce_motion(true);
        let mut calm = ReactionStrip::new();
        calm.layout(
            &[],
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 400.0),
            &mut text,
            &reduced,
        );
        calm.layout(
            &[group("🎉", &["bo"])],
            &[],
            Some(&tilo_actions::MemberId::person("me")),
            true,
            (Point::ZERO, 400.0),
            &mut text,
            &reduced,
        );
        let pose = tilo_ui_base::motion::Appear::pose(
            0.5,
            tilo_ui_base::motion::Direction::Enter,
            Rect::new(0.0, 0.0, 40.0, 20.0),
            Point::ZERO,
            reduced.motion(),
        );
        assert_eq!(
            pose.transform,
            Affine::IDENTITY,
            "reduced motion fades a pill in without scaling it"
        );
        let _settled = tilo_ui_base::motion::testing::settled();
        assert!(!strip.is_moving(), "and it all comes to rest");
    }
}

#[test]
fn the_widget_toggles_your_reaction_from_a_click_and_the_keyboard() {
    let theme = Theme::dark();
    let toggled = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = Rc::clone(&toggled);
    let mut widget = ReactionStripWidget::new(
        vec![group("👍", &["ada", "me"]), group("🎉", &["bo"])],
        Some(tilo_actions::MemberId::person("me")),
    )
    .with_names(names)
    .on_toggle(move |toggle| seen.borrow_mut().push(toggle));
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let laid = widget.strip().laid_out().clone();
    assert!(widget.get_rect().height() >= laid.height());
    // A pill is pressed on its emoji: the faces at its end are the reactors' own.
    let first = laid.pills()[0].glyph.center();
    let second = laid.pills()[1].glyph.center();
    click(&mut widget, first);
    click(&mut widget, second);
    assert_eq!(
        *toggled.borrow(),
        [
            ReactionToggle {
                emoji: "👍".into(),
                react: false
            },
            ReactionToggle {
                emoji: "🎉".into(),
                react: true
            }
        ]
    );
    assert_eq!(
        tilo_ui_base::emoji_catalogue::frequent()
            .first()
            .map(String::as_str),
        Some("🎉"),
        "reacting remembers the emoji"
    );
    let add = laid.add_rect().expect("add");
    click(&mut widget, add.center());
    assert_eq!(widget.take_add_request(), Some(add));
    assert_eq!(widget.take_toggles().len(), 2);
    assert!(widget.is_picking(), "the add button opens the picker");
    crate::testing::dispatch(
        &mut widget,
        &tilo_ui_base::synthetic_input::named_key(NamedKey::Escape),
    );
    assert!(!widget.is_picking(), "Escape closes it without a pick");
    widget.set_focus(true);
    crate::testing::dispatch(
        &mut widget,
        &tilo_ui_base::synthetic_input::named_key(NamedKey::Home),
    );
    crate::testing::dispatch(
        &mut widget,
        &tilo_ui_base::synthetic_input::named_key(NamedKey::Enter),
    );
    assert_eq!(
        widget.take_toggles(),
        [ReactionToggle {
            emoji: "👍".into(),
            react: false
        }],
        "Enter toggles the pill the keyboard is on"
    );
    let tip = widget.tooltip_target_at(second).map(|(_, tip)| tip);
    assert_eq!(tip.as_deref(), Some("Bo reacted with party popper"));

    // The same add button opens the shared picker, and a pick reacts.
    tilo_ui_base::emoji_catalogue::record_used("🌮");
    click(&mut widget, add.center());
    assert!(widget.is_picking(), "the add button opens the picker");
    let mut scene = Scene::new();
    widget.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
    let tree = widget.accessibility().expect("a group");
    assert!(
        tree.children.iter().any(|node| node.role == Role::Dialog),
        "the open picker is part of the strip's tree"
    );
    crate::testing::dispatch(
        &mut widget,
        &tilo_ui_base::synthetic_input::named_key(NamedKey::Enter),
    );
    assert!(!widget.is_picking(), "a pick closes it");
    assert_eq!(
        widget.take_toggles(),
        [ReactionToggle {
            emoji: "🌮".into(),
            react: true
        }],
        "the picked emoji is my reaction"
    );
}

/// On touch a pill's face stays compact while its target grows about its
/// centre to the touch minimum: assistive technology and layout audits see
/// the target, and a press just above the face toggles it.
#[test]
fn on_touch_a_pill_is_pressed_within_its_grown_target_and_its_face_stays_compact() {
    let theme = Theme::dark();
    let face_height = StripMetrics::of(&theme).height;
    let side = f64::from(tilo_ui_base::interaction::InteractionEnvironment::TOUCH.minimum_target());
    assert!(face_height < side, "the face is compact: {face_height}");
    let mut widget = ReactionStripWidget::new(
        vec![group("👍", &["bo"])],
        Some(tilo_actions::MemberId::person("ada")),
    );
    widget.set_interaction_environment(tilo_ui_base::interaction::InteractionEnvironment::TOUCH);
    lay_out_and_draw(&mut widget, 320.0, &theme);
    assert!(
        (widget.get_rect().height() - face_height).abs() < 0.5,
        "the strip is as tall as its faces: {:?}",
        widget.get_rect()
    );

    let access = widget.accessibility().expect("the strip's node");
    let pill = access.children.first().expect("the pill's node");
    let target = pill.bounds.expect("its bounds").to_rect();
    assert!(
        target.width() >= side - 0.5 && target.height() >= side - 0.5,
        "{target:?}"
    );

    let above_the_face = Point::new(target.center().x, target.center().y - side / 2.0 + 2.0);
    click(&mut widget, above_the_face);
    let toggles = widget.take_toggles();
    assert_eq!(toggles.len(), 1, "a press within the target toggles");
    assert_eq!(toggles[0].emoji, "👍");
}

#[test]
fn a_pill_counts_what_was_given_elsewhere_and_names_those_who_gave_it() {
    let theme = Theme::light();
    let mut text = TextRenderer::new();
    let groups = vec![group("👍", &["ada"])];
    let relayed = vec![
        ("👍".to_owned(), vec!["Bo".to_owned(), "Cy".to_owned()]),
        (":party:".to_owned(), vec!["Dee".to_owned()]),
    ];
    let laid = layout(
        &groups,
        &relayed,
        None,
        false,
        (Point::ZERO, 400.0),
        &mut text,
        &StripMetrics::of(&theme),
    );
    let [thumbs, party] = laid.pills() else {
        panic!("a pill for each emoji, the one given only elsewhere included");
    };
    assert_eq!(thumbs.count, 3, "Ada's and the two given on Discord");
    assert_eq!(party.count, 1);
    let (_, said) = laid
        .tooltip_at(thumbs.rect.center(), &names)
        .expect("a tooltip names who reacted");
    assert!(
        said.contains("Ada") && said.contains("Bo") && said.contains("Cy"),
        "{said}"
    );
}

/// Choosing a reactor's face says who they are: the face is theirs, apart from the pill it ends,
/// which still toggles the reaction, and it is theirs to choose for someone who may not react too.
#[test]
fn choosing_a_reactors_face_says_who_they_are_and_the_pill_still_toggles() {
    let theme = Theme::dark();
    let chosen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let toggled = Rc::new(std::cell::RefCell::new(Vec::new()));
    let (seen, pressed) = (Rc::clone(&chosen), Rc::clone(&toggled));
    let mut widget = ReactionStripWidget::new(
        vec![group("👍", &["ada", "bo"])],
        Some(tilo_actions::MemberId::person("me")),
    )
    .with_names(names)
    .on_person(move |who| seen.borrow_mut().push(who.id().to_owned()))
    .on_toggle(move |toggle| pressed.borrow_mut().push(toggle.emoji));
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let laid = widget.strip().laid_out().clone();
    let faces = laid.member_faces();
    assert_eq!(
        faces.iter().map(|(_, who)| who.id()).collect::<Vec<_>>(),
        ["ada", "bo"],
        "each member's face, in the order they reacted"
    );
    let pill = laid.pills()[0].rect;
    for (face, _) in &faces {
        assert!(pill.contains(face.center()), "a face stands on its pill");
    }
    click(&mut widget, faces[1].0.center());
    assert_eq!(*chosen.borrow(), ["bo"], "the face chosen is Bo's");
    assert!(
        toggled.borrow().is_empty(),
        "choosing a face reacts with nothing"
    );
    click(&mut widget, laid.pills()[0].glyph.center());
    assert_eq!(*toggled.borrow(), ["👍"], "the pill's emoji still toggles");

    // Someone who may not react still chooses who reacted.
    let watching = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = Rc::clone(&watching);
    let mut widget = ReactionStripWidget::new(vec![group("👍", &["ada"])], None)
        .can_react(false)
        .on_person(move |who| seen.borrow_mut().push(who.id().to_owned()));
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let face = widget.strip().laid_out().member_faces()[0].0;
    click(&mut widget, face.center());
    assert_eq!(*watching.borrow(), ["ada"]);
}
