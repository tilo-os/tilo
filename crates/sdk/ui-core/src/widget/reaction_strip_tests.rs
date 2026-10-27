use super::*;
use crate::widget::Widget as _;
use std::collections::HashMap;
use std::rc::Rc;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::NamedKey;

/// Lay `widget` out in a `width` wide column and draw it once.
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
    widget.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &crate::presence::PresenceCtx::default(),
    );
}

fn click(widget: &mut ReactionStripWidget, at: Point) {
    crate::testing::dispatch(widget, &crate::synthetic_input::cursor_moved(at.x, at.y));
    crate::testing::dispatch(widget, &crate::synthetic_input::mouse_press());
    crate::testing::dispatch(widget, &crate::synthetic_input::mouse_release());
}

fn reactor(by: &str) -> Reactor {
    Reactor {
        by: by.to_owned(),
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
    match reactor.by.as_str() {
        "ada" => "Ada".into(),
        "bo" => "Bo".into(),
        other => other.to_uppercase(),
    }
}

#[test]
fn pills_wrap_within_the_width_and_the_add_button_follows_them() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let metrics = StripMetrics::of(&theme);
    let groups: Vec<ReactionGroup> = ["👍", "🎉", "❤️", "😂", "🔥", "👀"]
        .iter()
        .map(|emoji| group(emoji, &["ada"]))
        .collect();
    let wide = layout(
        &groups,
        "me",
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
        "me",
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
        measure_height(&groups, true, narrow_width, &mut text, &theme),
        narrow.height()
    );
    assert_eq!(measure_height(&[], false, 300.0, &mut text, &theme), 0.0);
}

#[test]
fn a_pill_toggles_its_emoji_and_the_add_button_anchors_the_picker() {
    let theme = Theme::light();
    let mut text = TextRenderer::new();
    let groups = vec![group("👍", &["ada", "me"]), group(":tilo:", &["bo"])];
    let laid = layout(
        &groups,
        "me",
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
        "me",
        false,
        (Point::ZERO, 400.0),
        &mut text,
        &StripMetrics::of(&theme),
    );
    assert!(read_only.add_rect().is_none());
    assert!(read_only.hit(read_only.pills()[0].rect.center()).is_none());
    assert!(read_only.targets().is_empty());
}

#[test]
fn the_tooltip_names_who_reacted_you_first() {
    let mut text = TextRenderer::new();
    let mut bridged = group("🙌", &["ada", "bo", "cy", "me", "dee"]);
    bridged.reactors.push(Reactor {
        by: "bridge".into(),
        persona: Some("discord/1".into()),
        name: Some("Eve".into()),
    });
    let groups = vec![group("🎉", &["ada", "bo"]), bridged, group("🔥", &["me"])];
    let laid = layout(
        &groups,
        "me",
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
    assert_eq!(tip(0), "Ada and Bo reacted with 🎉");
    assert_eq!(tip(1), "You, Ada and 4 others reacted with 🙌");
    assert_eq!(tip(2), "You reacted with 🔥");
    let add = laid.add_rect().expect("add");
    assert_eq!(
        laid.tooltip_at(add.center(), &names).map(|(_, tip)| tip),
        Some("Add reaction".into())
    );
}

#[test]
fn every_pill_is_a_toggle_named_by_what_it_does() {
    let mut text = TextRenderer::new();
    let groups = vec![group("👍", &["ada", "me"]), group("🎉", &["bo"])];
    let laid = layout(
        &groups,
        "me",
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

#[test]
fn a_pill_arriving_after_the_first_layout_appears_and_a_count_cross_fades() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut strip = ReactionStrip::new();
    strip.layout(
        &[group("👍", &["ada"])],
        "me",
        true,
        (Point::ZERO, 400.0),
        &mut text,
        &theme,
    );
    assert!(!strip.is_moving(), "what is there at first rests");
    strip.layout(
        &[group("👍", &["ada", "bo"]), group("🎉", &["bo"])],
        "me",
        true,
        (Point::ZERO, 400.0),
        &mut text,
        &theme,
    );
    let now = crate::motion::frame_now();
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
    calm.layout(&[], "me", true, (Point::ZERO, 400.0), &mut text, &reduced);
    calm.layout(
        &[group("🎉", &["bo"])],
        "me",
        true,
        (Point::ZERO, 400.0),
        &mut text,
        &reduced,
    );
    let pose = crate::motion::Appear::pose(0.5, Point::ZERO, reduced.motion());
    assert_eq!(
        pose.transform,
        Affine::IDENTITY,
        "reduced motion fades a pill in without scaling it"
    );
    let _settled = crate::motion::testing::settled();
    assert!(!strip.is_moving(), "and it all comes to rest");
}

#[test]
fn the_widget_toggles_your_reaction_from_a_click_and_the_keyboard() {
    let theme = Theme::dark();
    let toggled = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = Rc::clone(&toggled);
    let mut widget = ReactionStripWidget::new(
        vec![group("👍", &["ada", "me"]), group("🎉", &["bo"])],
        "me",
    )
    .with_names(names)
    .on_toggle(move |toggle| seen.borrow_mut().push(toggle));
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let laid = widget.strip().laid_out().clone();
    assert!(widget.get_rect().height() >= laid.height());
    let first = laid.pills()[0].rect.center();
    let second = laid.pills()[1].rect.center();
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
        crate::emoji_catalogue::recents()
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
        &crate::synthetic_input::named_key(NamedKey::Escape),
    );
    assert!(!widget.is_picking(), "Escape closes it without a pick");
    widget.set_focus(true);
    crate::testing::dispatch(
        &mut widget,
        &crate::synthetic_input::named_key(NamedKey::Home),
    );
    crate::testing::dispatch(
        &mut widget,
        &crate::synthetic_input::named_key(NamedKey::Enter),
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
    assert_eq!(tip.as_deref(), Some("Bo reacted with 🎉"));
}

#[test]
fn a_read_only_strip_shows_the_pills_and_takes_no_reaction() {
    let theme = Theme::dark();
    let mut widget = ReactionStripWidget::new(vec![group("👍", &["ada"])], "me").can_react(false);
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let laid = widget.strip().laid_out().clone();
    assert!(laid.add_rect().is_none());
    click(&mut widget, laid.pills()[0].rect.center());
    assert!(widget.take_toggles().is_empty());
    assert!(!widget.is_focusable());
}

#[test]
fn the_add_button_opens_the_shared_picker_and_a_pick_reacts() {
    let theme = Theme::dark();
    crate::emoji_catalogue::record_recent("🌮");
    let mut widget = ReactionStripWidget::new(vec![group("👍", &["ada"])], "me");
    lay_out_and_draw(&mut widget, 600.0, &theme);
    let add = widget.strip().laid_out().add_rect().expect("add");
    click(&mut widget, add.center());
    assert!(widget.is_picking(), "the add button opens the picker");
    let mut scene = Scene::new();
    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &crate::presence::PresenceCtx::default(),
    );
    let tree = widget.accessibility().expect("a group");
    assert!(
        tree.children.iter().any(|node| node.role == Role::Dialog),
        "the open picker is part of the strip's tree"
    );
    crate::testing::dispatch(
        &mut widget,
        &crate::synthetic_input::named_key(NamedKey::Enter),
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
