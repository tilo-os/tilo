use super::*;
use crate::testing::dispatch;
use automerge::AutoCommit;
use vello::Scene;
use vello_encoding::DrawTag;

fn lay_out(w: &mut dyn Widget, vw: f32, vh: f32) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = w.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(vw),
                height: taffy::AvailableSpace::Definite(vh),
            },
            |k, a, id, _, _| {
                node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(k, a, &mut tr)
                })
            },
        )
        .unwrap();
    w.apply_layout(&taffy, node, Point::ZERO);
}

fn draw(w: &mut dyn Widget) -> Scene {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    w.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        presence.draw_env(),
    ));
    scene
}

fn cursor(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}
use crate::synthetic_input::mouse_press;

fn metrics() -> Metrics {
    Metrics {
        title_h: 24.0,
        msg_h: 18.0,
        quote_h: 0.0,
        hint_h: 0.0,
        documents_h: 0.0,
        action_w: 0.0,

        inner_width: 0.0,
        pad_x: 0.0,
        gap: 16.0,
        text_gap: 12.0,
        btn_h: 34.0,
        hero: 72.0,
    }
}

tilo_test_env::scenario! {
    /// What an empty state shows in the width it has.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_empty_state_lays_out_wraps_and_sheds;

    phase short_and_narrow_panes_shed_decoration_but_keep_the_action;
    {
        /// A zero-state in a short pane (a docked panel body, a couple hundred
        /// pixels tall) must shed elements rather than push the message through
        /// the bottom edge.
        fn a_short_pane_sheds_the_hero_disc_before_it_overflows() {
            let want = Parts {
                icon: true,
                message: true,
                quote: false,
                documents: false,
                action: false,
                hint: false,
            };
            let m = metrics();

            let reserved = Fit::choose(400.0, want, m);
            assert!(
                reserved.icon && reserved.message,
                "a full pane keeps the whole stack"
            );
            assert!(reserved.total <= 400.0);

            let docked = Fit::choose(120.0, want, m);
            assert!(!docked.icon, "the disc is shed first");
            assert!(docked.message, "the explanation is kept while it fits");
            assert!(
                docked.total <= 120.0,
                "and the result fits: {} in 120",
                docked.total
            );

            // Barely any width: the message goes too, but never the title.
            let cramped = Fit::choose(30.0, want, m);
            assert!(!cramped.icon && !cramped.message);
            assert_eq!(cramped.total, m.title_h, "the title always survives");
        }

        fn narrow_large_text_recovery_copy_wraps_inside_the_empty_state() {
            let theme = Theme::dark().with_text_scale(2.0);
            let rect = Rect::new(0.0, 0.0, 390.0, 680.0);
            let gap = f64::from(theme.tokens.spacing.s4);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test viewport is an ordinary logical-pixel dimension"
            )]
            let width = (rect.width() - gap * 2.0).max(1.0) as f32;
            let title = "Couldn’t open this page";
            let message = "The site did not respond. Check your connection and try again.";
            let title_style = theme
                .text(TextRole::Title)
                .with_align(ParagraphAlign::Center);
            let message_style = theme
                .text(TextRole::Caption)
                .with_align(ParagraphAlign::Center);
            let mut renderer = TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let (_, title_one_line_height) = renderer.measure_style(title, None, &title_style);
            let (title_width, title_height) = renderer.measure_style(title, Some(width), &title_style);
            let (message_width, message_height) =
                renderer.measure_style(message, Some(width), &message_style);

            assert!(title_width <= width);
            assert!(message_width <= width);
            assert!(title_height > title_one_line_height);
            assert!(message_height > 0.0);

            let mut state = EmptyState::new(title)
                .with_message(message)
                .with_action("Try again");
            state.apply_layout_external(rect);
            let mut scene = Scene::new();
            state.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut renderer, &theme, PresenceCtx::default().draw_env()));
            let action = state.action_rect();
            assert!(rect.contains(action.center()));
            let action_style = theme
                .text(TextRole::Label)
                .with_wrap_mode(TextWrapMode::WordOrGlyph)
                .with_align(ParagraphAlign::Center);
            let (_, action_line_height) =
                renderer.measure_style("Try again", Some(width), &action_style);
            assert!(
                action.height()
                    >= f64::from(action_line_height) + f64::from(theme.tokens.spacing.s2) * 2.0,
                "large-text action reserves its label's full line height"
            );
        }

        fn long_localized_content_wraps_inside_a_narrow_scaled_pane() {
            for width in [240.0, 480.0] {
                for scale in [1.0, 2.0] {
                    let theme = Theme::dark().with_text_scale(scale);
                    let mut tr = crate::font_catalog::fixture_renderer();
                    tr.set_text_scale(scale);
                    let mut scene = Scene::new();
                    let rect = Rect::new(20.0, 30.0, 20.0 + width, 1330.0);
                    let action = EmptyState::paint(
                        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                        rect,
                        &EmptyStateContent {
                            icon: Some(Icon::Inbox),
                            illustration: None,
                            documents: &[],
                            error: false,
                            title: "Keine synchronisierten Nachrichten in diesem Arbeitsbereich",
                            message: Some(
                        "Sobald eine Verbindung verfügbar ist, erscheinen neue Nachrichten automatisch.",
                    ),
                            action_label: Some(
                        "Verbindungseinstellungen jetzt vollständig überprüfen und sichere Wiederherstellung vorbereiten",
                    ),
                            quote: None,
                            hint: None,
                        },
                        Point::new(-1.0, -1.0),
                    )
                .expect("action remains available");

                    assert!(action.x0 >= rect.x0 && action.x1 <= rect.x1, "{action:?}");
                    assert!(action.height() > 34.0, "wrapped action grows: {action:?}");
                    let encoding = scene.encoding();
                    for run in &encoding.resources.glyph_runs {
                        for glyph in &encoding.resources.glyphs[run.glyphs.clone()] {
                            let x = f64::from(run.transform.translation[0] + glyph.x);
                            assert!(
                                x >= rect.x0 && x <= rect.x1,
                                "{width}px/{scale}x glyph x={x} outside {rect:?}"
                            );
                        }
                    }
                }
            }
        }

        a_short_pane_sheds_the_hero_disc_before_it_overflows();
        narrow_large_text_recovery_copy_wraps_inside_the_empty_state();
        long_localized_content_wraps_inside_a_narrow_scaled_pane();
    }

    phase empty_state_paints_scrolls_and_centres_its_essential_content;
    {
        fn retained_empty_state_has_a_scrollable_route_to_essential_content() {
            use std::cell::Cell;
            let invoked = Rc::new(Cell::new(0));
            let counted = invoked.clone();
            let scroll = EmptyState::new("No messages are available")
                .with_icon(Icon::Inbox)
                .with_message("Reconnect to load messages.")
                .with_action("Review connection settings")
                .with_action_id("empty.review")
                .on_open(move || counted.set(counted.get() + 1))
                .into_scrollable()
                .with_height(50.0);
            let mut harness = crate::app::driver::Driver::for_root(
                crate::State::new(),
                scroll,
                (240, 50),
                Theme::dark(),
            );
            assert!(harness.root::<Scrollable>().vertical_overflows());
            let end = harness.root::<Scrollable>().max_offset_y();
            assert!(end > 0.0);
            harness.root_mut::<Scrollable>().scroll_by(0.0, end);
            harness.relayout();
            let _ = harness.inspection();
            let scroll = harness.root::<Scrollable>();
            let action = scroll.children()[0]
                .synthetic_accessibility_rect(&[0])
                .expect("retained action geometry");
            let node = scroll.children()[0]
                .accessibility()
                .expect("a described state");
            assert_eq!(
                node.children
                    .first()
                    .and_then(|button| button.bounds)
                    .map(crate::a11y::AccessBounds::to_rect),
                Some(action),
                "the action is described where it is drawn"
            );
            assert!(
                scroll.content_viewport_rect().contains(action.center()),
                "the scrolled action is visible: {action:?}"
            );
            harness.click_at(action.center().x, action.center().y);
            assert_eq!(
                invoked.get(),
                1,
                "visible recovery action invokes through the host"
            );
            harness
                .perform_named("empty.review", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                invoked.get(),
                2,
                "accessibility uses the same recovery action"
            );
        }

        fn wide_scrollable_retains_the_icon_and_explanatory_message() {
            let make = |icon| {
                let mut scroll = EmptyState::new("No messages")
                    .with_message("Reconnect to load messages.")
                    .with_action("Review settings");
                if icon {
                    scroll = scroll.with_icon(Icon::Inbox);
                }
                scroll.into_scrollable().with_height(400.0)
            };
            let mut decorated = make(true);
            lay_out(&mut decorated, 480.0, 400.0);
            let decorated_scene = draw(&mut decorated);
            let decorated_encoding = decorated_scene.encoding();
            assert!(
                decorated_encoding.resources.glyph_runs.len() >= 3,
                "title, explanatory message, and action all remain painted"
            );

            let mut plain = make(false);
            lay_out(&mut plain, 480.0, 400.0);
            let plain_scene = draw(&mut plain);
            let decorated_colors = decorated_encoding
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            let plain_colors = plain_scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            assert!(
                decorated_colors > plain_colors,
                "wide scrollable keeps the icon disc and strokes"
            );
        }

        fn wide_manual_pane_keeps_the_essential_block_centered() {
            let theme = Theme::dark();
            let mut tr = crate::font_catalog::fixture_renderer();
            let rect = Rect::new(15.0, 25.0, 495.0, 425.0);
            let minimum = EmptyState::minimum_essential_height(
                &mut tr,
                &theme,
                rect.width(),
                "No messages",
                Some("Compose"),
            );
            let mut scene = Scene::new();
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    illustration: None,
                    documents: &[],
                    error: false,
                    title: "No messages",
                    message: None,
                    action_label: Some("Compose"),
                    quote: None,
                    hint: None,
                },
                Point::new(-1.0, -1.0),
            )
            .expect("wide pane exposes action");
            let top = action.y1 - minimum;
            assert!(((top - rect.y0) - (rect.y1 - action.y1)).abs() < 0.001);

            let mut retained = EmptyState::new("No messages").with_action("Compose");
            lay_out(&mut retained, 480.0, 400.0);
            assert_eq!(retained.get_rect(), Rect::new(0.0, 0.0, 480.0, 400.0));
            let _ = draw(&mut retained);
            let retained_action = retained.action_rect();
            let retained_top = retained_action.y1 - minimum;
            assert!((retained_top - (400.0 - retained_action.y1)).abs() < 0.001);
        }

        retained_empty_state_has_a_scrollable_route_to_essential_content();
        wide_scrollable_retains_the_icon_and_explanatory_message();
        wide_manual_pane_keeps_the_essential_block_centered();
    }

    // In a wide window the message wraps at a readable measure under the
    // title, never one sentence stretched across the whole width.
    phase a_wide_window_wraps_the_message_at_a_readable_measure;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let message = "Bridge a server with the Discord Bridge bot and its channels and \
                   messages show up in this workspace's Chat, in step both ways.";
        let content = EmptyStateContent {
            message: Some(message),
            ..EmptyStateContent::new("Bring a Discord server into this area")
        };
        let style = StackStyle::resolved(&theme, false);
        let wide = style.measure(&mut text, 1600.0, &content, 0.0);
        assert!(f64::from(wide.inner_width) <= f64::from(theme.tokens.metrics.message_measure));
        let one_line = style.measure(
            &mut text,
            1600.0,
            &EmptyStateContent {
                message: Some("Ag"),
                ..content
            },
            0.0,
        );
        assert!(wide.msg_h > one_line.msg_h, "the message wraps");
        let narrow = style.measure(&mut text, 320.0, &content, 0.0);
        assert!(
            narrow.inner_width < wide.inner_width,
            "a narrow window still narrows it"
        );
    }

    // At a phone's width an error says all of what happened and what to
    // do: its message wraps like any other, never cut off with an ellipsis.
    phase errors_use_the_danger_tone_and_wrap_their_whole_message_at_420_px;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let detail = "Couldn't read the selected files. Check permissions and try again.";
        let content = EmptyStateContent {
            icon: Some(Icon::FileText),
            message: Some(detail),
            error: true,
            ..EmptyStateContent::new("Couldn't open the files")
        };
        let style = StackStyle::resolved(&theme, false);
        let error = style.measure(&mut text, 420.0, &content, 0.0);
        let plain = style.measure(
            &mut text,
            420.0,
            &EmptyStateContent {
                error: false,
                ..content
            },
            0.0,
        );
        let (_, line) = text.measure_style("Ag", None, &style.message);
        assert!(
            error.msg_h > f64::from(line) * 1.5,
            "the whole message wraps onto more lines than one"
        );
        assert!(
            (error.msg_h - plain.msg_h).abs() < f64::EPSILON,
            "an error wraps exactly like any other message"
        );
        assert_eq!(error.text_gap, f64::from(theme.tokens.spacing.s3));
        assert_eq!(error.gap, f64::from(theme.tokens.spacing.s4));

        let state = EmptyState::error("Couldn't open the files", detail);
        let node = Widget::accessibility(&state).unwrap();
        assert_eq!(node.description.as_deref(), Some(detail), "full text");

        let mut scene = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 360.0, 400.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        let danger = theme.tokens.colors.semantic.danger.to_rgba8();
        let encoded = scene.encoding();
        let has_danger = encoded.draw_data.chunks(1).any(|word| {
            word.first().is_some_and(|w| {
                let [r, g, b, a] = w.to_le_bytes();
                (r, g, b, a) == (danger.r, danger.g, danger.b, danger.a)
                    || (a, b, g, r) == (danger.r, danger.g, danger.b, danger.a)
            })
        });
        assert!(has_danger, "error glyph is painted in the danger colour");
    }

    phase an_illustration_takes_the_hero_slot_and_sheds_first;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let painted = std::cell::Cell::new(None);
        let paint = |_: &mut PaintCtx<'_>, rect: Rect| painted.set(Some(rect));
        let content = EmptyStateContent {
            illustration: Some(&paint),
            message: Some("Search for a city to see its forecast."),
            ..EmptyStateContent::new("No places yet")
        };
        let mut scene = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 400.0, 500.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        let slot = painted.take().expect("illustration painted");
        assert!((slot.width() - f64::from(theme.tokens.metrics.icon_lg) * 4.0).abs() < 0.5);
        let mut cramped = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut cramped, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 400.0, 80.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        assert!(
            painted.take().is_none(),
            "a short pane sheds the illustration"
        );
    }

    phase an_inline_empty_state_hugs_its_content_and_a_busy_one_keeps_moving;
    {
        let mut inline = EmptyState::new("No servers yet")
            .with_message("Add one to start.")
            .inline();
        let mut full = EmptyState::new("No servers yet").with_message("Add one to start.");
        lay_out(&mut inline, 600.0, 600.0);
        lay_out(&mut full, 600.0, 600.0);
        assert!(
            inline.get_rect().height() < 120.0,
            "inline hugs its content: {:?}",
            inline.get_rect()
        );
        assert!(full.get_rect().height() >= 220.0);

        // The glyph it was given is what tells a person what would be here: a state that
        // hugs its content keeps it rather than shedding it.
        let mut with_icon = EmptyState::new("No archived items")
            .with_message("Archive an item to put it aside.")
            .with_icon(crate::icons::Icon::Archive)
            .inline();
        lay_out(&mut with_icon, 600.0, 600.0);
        let rect = with_icon.get_rect();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let placed = place(&mut tr, &theme, rect, &with_icon.content(), true);
        assert!(placed.hero.is_some(), "an inline state shows its glyph in {rect:?}, natural {}", placed.natural);

        let busy = EmptyState::new("Scanning your disk").busy();
        assert!(busy.needs_animation_frame(), "a busy state keeps moving");
        let node = Widget::accessibility(&busy).unwrap();
        assert!(node.state.busy);
    }
}

tilo_test_env::scenario! {
    /// An empty state's action.
    /// Each phase was a test of its own and builds its own fixture.
    fn the_empty_state_action_is_placed_published_and_fires_once;

    phase exported_action_button_is_placed_where_activation_clicks;
    {
        let mut es = EmptyState::new("No response yet").with_action("Send request");
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        es.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let update = crate::accesskit_bridge::build_tree_update(&es, &node_map, None, 1.0);
        let (_, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Button)
            .expect("the action is exported as a button");
        let bounds = button.bounds().expect("the action button has bounds");
        let action = es.action_rect();
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (action.x0, action.y0, action.x1, action.y1)
        );
    }

    phase empty_state_action_fires_once_through_pointer_keyboard_and_semantics;
    {
        fn action_pill_click_fires_once() {
            let fired = Rc::new(std::cell::Cell::new(0));
            let counted = Rc::clone(&fired);
            let mut es = EmptyState::new("No messages")
                .with_icon(Icon::Inbox)
                .with_message("Your inbox is empty")
                .with_action("Compose")
                .on_open(move || counted.set(counted.get() + 1));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            let target = es.action_rect().center();
            assert!(target.x > 0.0, "action rect must resolve after draw");
            dispatch(&mut es, &cursor(target));
            assert!(
                dispatch(&mut es, &mouse_press()),
                "click on the action pill is consumed"
            );
            assert_eq!(fired.get(), 1, "the action runs once per click");
        }

        fn click_outside_action_does_not_fire() {
            let mut es = EmptyState::new("Nothing here")
                .with_action("Add")
                .on_open(|| panic!("a click away from the pill runs nothing"));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            dispatch(&mut es, &cursor(Point::new(2.0, 2.0)));
            assert!(
                !dispatch(&mut es, &mouse_press()),
                "click away from the pill is not consumed"
            );
        }

        fn action_is_keyboard_reachable_and_exposed_as_a_named_button() {
            let fired = Rc::new(std::cell::Cell::new(false));
            let seen = Rc::clone(&fired);
            let mut es = EmptyState::new("No notes yet")
                .with_message("Create one to get started")
                .with_action("Create note")
                .on_open(move || seen.set(true));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            assert!(Widget::is_focusable(&es));
            Widget::set_focus(&mut es, true);
            let node = Widget::accessibility(&es).expect("empty state is semantic");
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, crate::a11y::Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Create note"));
            assert!(node.children[0].state.focused);
            assert_eq!(
                es.synthetic_accessibility_rect(&[0]),
                Some(es.action_rect())
            );

            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut es,
                &crate::a11y::Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(fired.get(), "Enter/Space/AT Activate reaches the CTA");
        }

        fn action_path_is_shared_by_pointer_and_accessibility_activation() {
            use automerge::ReadDoc as _;
            use std::cell::Cell;
            let invoked = Rc::new(Cell::new(0));
            let counted = invoked.clone();
            let empty = EmptyState::new("Nothing here")
                .with_action("Open")
                .with_action_id("empty.open")
                .on_open(move || counted.set(counted.get() + 1));
            let mut harness = crate::app::driver::Driver::for_root(
                crate::State::new(),
                empty,
                (600, 400),
                Theme::dark(),
            );
            let action = harness.root::<EmptyState>().action_rect();
            assert!(action.area() > 0.0, "the recovery action has a target");
            harness.click_at(action.center().x, action.center().y);
            assert_eq!(
                invoked.get(),
                1,
                "pointer activation invokes the published action"
            );
            harness
                .perform_named("empty.open", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                invoked.get(),
                2,
                "accessibility invokes the same published action"
            );
            assert!(
                harness
                    .state()
                    .doc
                    .get(automerge::ROOT, "empty.open")
                    .unwrap()
                    .is_none(),
                "a view action never writes a click pulse into the resource"
            );
        }

        fn an_action_binding_invokes_the_resource_action() {
            let resource = tilo_actions::ResourceSpec::new("catalog", "catalog", "Catalog")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "refresh",
                        "Refresh catalog",
                        tilo_actions::OperationKind::Effect,
                        "test.refresh",
                        &(),
                    )
                    .unwrap(),
                );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "refresh").unwrap();
            let mut es = EmptyState::new("Unavailable")
                .with_action("Refresh catalog")
                .with_action_id("catalog.empty.refresh")
                .on_action(binding);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            let target = es.action_rect().center();
            dispatch(&mut es, &cursor(target));
            assert!(dispatch(&mut es, &mouse_press()));
            let invocation = crate::widget::controls::take_control_invocation(&mut es)
                .expect("canonical invocation");
            assert_eq!(
                invocation.address,
                tilo_actions::ActionAddress::new("catalog", "refresh")
            );
            assert!(matches!(
                crate::widget::controls::control_action_bindings(&es).as_slice(),
                [crate::widget::ControlBindingMetadata {
                    control_id,
                    kind: crate::widget::ControlBindingKind::Action(address),
                }] if control_id == "catalog.empty.refresh"
                    && address == &tilo_actions::ActionAddress::new("catalog", "refresh")
            ));
        }

        action_pill_click_fires_once();
        click_outside_action_does_not_fire();
        action_is_keyboard_reachable_and_exposed_as_a_named_button();
        action_path_is_shared_by_pointer_and_accessibility_activation();
        an_action_binding_invokes_the_resource_action();
    }

    phase a_handler_publishes_the_action_as_a_certified_bound_button;
    {
        let opened = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = opened.clone();
        let root = EmptyState::new("No places yet")
            .with_action("Add a place")
            .with_action_id("weather.empty.add")
            .on_open(move || seen.set(seen.get() + 1));
        let mut driver =
            crate::app::driver::Driver::new(crate::state::State::new(), Box::new(root), (480, 360));
        let audit = driver.native_app_audit();
        assert!(audit.is_certified(), "{audit:#?}");
        driver
            .perform_named("weather.empty.add", &crate::a11y::Action::Activate)
            .expect("the action is addressable by its id");
        assert_eq!(opened.get(), 1, "the view handler ran once");
    }

    phase layout_reports_the_action_where_paint_puts_it;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 480.0, 400.0);
        let content = EmptyStateContent {
            icon: Some(Icon::Inbox),
            message: Some("Your inbox is empty"),
            action_label: Some("Compose"),
            ..EmptyStateContent::new("No messages")
        };
        let layout = EmptyState::layout(&mut text, &theme, rect, &content);
        let mut scene = Scene::new();
        let painted = EmptyState::paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            rect,
            &content,
            Point::new(-1.0, -1.0),
        );
        assert_eq!(layout.action, painted);
        assert!(layout.natural_height > 0.0 && layout.natural_height < rect.height());
    }

    phase a_painted_state_is_addressed_as_the_controls_beside_it_are;
    {
        use crate::a11y::{AccessNode, Role};
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let content = EmptyStateContent {
            action_label: Some("Choose camera"),
            ..EmptyStateContent::new("Camera unavailable")
        };
        let mut painted = PaintedEmptyStates::default();
        painted.draw(Affine::IDENTITY, || {
            EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                Rect::new(0.0, 0.0, 500.0, 400.0),
                &content,
                Point::new(-1.0, -1.0),
            );
        });
        let host = EmptyState::new("host");
        let control = || AccessNode::new(Role::Button).with_name("Take photo");

        let keyed = AccessNode::new(Role::Window)
            .with_children(vec![control().with_action_target("camera.shutter")]);
        let keyed = painted.describe(Some(keyed), &host).unwrap();
        assert_eq!(
            keyed.children.len(),
            2,
            "the painted state joins the controls"
        );
        assert!(
            keyed.children.iter().all(|child| child.key.is_some()),
            "beside keyed controls it is keyed too"
        );

        let positional = AccessNode::new(Role::Window).with_children(vec![control()]);
        let positional = painted.describe(Some(positional), &host).unwrap();
        assert_eq!(positional.children.len(), 2);
        assert!(
            positional.children.iter().all(|child| child.key.is_none()),
            "beside position-addressed controls it is addressed by position"
        );
    }

    // A control's place is the layout's: a window that has laid out and has not
    // yet painted still finds the empty state's action inside it, never at the
    // window's origin, where a box nothing placed stands.
    phase the_layout_places_the_action_without_a_paint;
    {
        let mut state = EmptyState::new("No notes yet")
            .with_message("Notes you write show up here.")
            .with_action("New note")
            .on_open(|| {});
        lay_out(&mut state, 600.0, 400.0);
        let action = state.action_rect();
        let box_ = Widget::get_rect(&state);
        assert!(
            !action.is_zero_area(),
            "the layout places the action: {action:?}"
        );
        let published = state
            .engine
            .as_ref()
            .map(Widget::get_rect)
            .expect("the action is a published button");
        assert_eq!(
            published, action,
            "the published button stands where the action does"
        );
        assert!(
            box_.contains(action.center()),
            "inside the state that holds it: {action:?} in {box_:?}"
        );

        let mut placed = EmptyState::new("No notes yet").with_action("New note");
        let external = Rect::new(120.0, 60.0, 720.0, 460.0);
        placed.apply_layout_external(external);
        let action = placed.action_rect();
        assert!(
            !action.is_zero_area() && external.contains(action.center()),
            "a host-placed state places its action too: {action:?} in {external:?}"
        );
    }
}

tilo_test_env::scenario! {
    /// An empty state's details disclosure.
    /// Each phase was a test of its own and builds its own fixture.
    fn technical_details_wait_behind_their_disclosure;

    phase the_details_disclosure_is_an_inventoried_view_control;
    {
        let es = EmptyState::new("Unavailable")
            .with_details("os error 2")
            .on_click(|| {})
            .with_action("Try again");
        let target = es
            .accessibility()
            .unwrap()
            .children
            .iter()
            .find(|child| child.description.as_deref() == Some("os error 2"))
            .and_then(|child| child.action_target.clone())
            .expect("the disclosure names its control");
        let bindings = crate::widget::controls::control_action_bindings(&es);
        assert!(
            bindings.iter().any(|binding| binding.control_id == target
                && matches!(
                    binding.kind,
                    crate::widget::ControlBindingKind::SemanticAction(_)
                )),
            "{bindings:?}"
        );
        assert!(
            crate::widget::controls::control_action_bindings(
                &EmptyState::new("Empty").with_details("why")
            )
            .iter()
            .any(|binding| binding.control_id.ends_with(":details"))
        );
    }

    phase technical_details_wait_behind_a_closed_disclosure_under_the_action;
    {
        let detail = "command process failed: No such file or directory (os error 2)";
        let mut es = EmptyState::new("Podman isn't available")
            .with_message("Install Podman to manage containers here.")
            .with_action("Try again")
            .on_open(|| panic!("opening details is not the action"))
            .with_details(detail);
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);
        let toggle = es.details_rect;
        assert!(!es.details_open(), "details start closed");
        assert!(
            toggle.y0 >= es.action_rect().y1,
            "the disclosure sits under the action: {toggle:?} {:?}",
            es.action_rect()
        );
        let node = Widget::accessibility(&es).unwrap();
        assert!(
            !node
                .description
                .clone()
                .unwrap_or_default()
                .contains("os error"),
            "the message stays the person's"
        );
        let disclosure = node
            .children
            .iter()
            .find(|child| child.description.as_deref() == Some(detail))
            .expect("the detail is spoken from the disclosure");
        assert_eq!(disclosure.state.expanded, Some(false));

        dispatch(&mut es, &cursor(toggle.center()));
        assert!(dispatch(&mut es, &mouse_press()), "the click opens it");
        assert!(es.details_open());
        let node = Widget::accessibility(&es).unwrap();
        assert!(
            node.children
                .iter()
                .any(|child| child.state.expanded == Some(true))
        );
    }

    phase the_layout_places_the_details_disclosure_without_a_paint;
    {
        let detail = "command process failed: No such file or directory (os error 2)";
        let mut state = EmptyState::new("Podman isn't available")
            .with_message("Install Podman to manage containers here.")
            .with_action("Try again")
            .with_details(detail);
        lay_out(&mut state, 600.0, 400.0);
        let toggle = state.details_rect;
        let box_ = Widget::get_rect(&state);
        assert!(
            !toggle.is_zero_area(),
            "the layout places the disclosure: {toggle:?}"
        );
        assert!(
            box_.contains(toggle.center()),
            "inside the state: {toggle:?} in {box_:?}"
        );
        assert!(
            toggle.y0 >= state.action_rect().y1,
            "under the action: {toggle:?} {:?}",
            state.action_rect()
        );
        dispatch(&mut state, &cursor(toggle.center()));
        assert!(
            dispatch(&mut state, &mouse_press()),
            "the click opens it before anything painted"
        );
        assert!(state.details_open());
    }
}

#[test]
fn an_empty_state_in_a_workspace_that_never_synced_says_so_and_offers_nothing_of_the_apps() {
    use crate::workspace_sync::{WorkspaceSync, WorkspaceSyncState};
    let theme = crate::theme::Theme::dark();
    let mut text = crate::text_render::TextRenderer::new();
    let mut scene = vello::Scene::new();
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 500.0, 400.0);
    let content = EmptyStateContent {
        action_label: Some("Say hello"),
        ..EmptyStateContent::new("Nothing has been said here yet")
    };
    let mut paint = |scene: &mut vello::Scene| {
        EmptyState::paint(
            &mut PaintCtx::new(scene, vello::kurbo::Affine::IDENTITY, &mut text, &theme),
            rect,
            &content,
            vello::kurbo::Point::new(-1.0, -1.0),
        )
    };
    let told = |state| {
        Some(WorkspaceSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        })
    };

    crate::workspace_sync::set(told(WorkspaceSyncState::Connecting));
    let _ = crate::workspace_sync::take_spinner_damage();
    assert!(
        paint(&mut scene).is_none(),
        "before the first sync the app's own empty state, and its action, wait"
    );
    assert!(
        crate::workspace_sync::take_spinner_damage().is_some_and(|slot| {
            rect.contains(slot.center()) && slot.area() < rect.area() / 4.0
        }),
        "the connecting state turns a spinner and damages its slot alone"
    );

    crate::workspace_sync::set(told(WorkspaceSyncState::Unreachable));
    assert!(
        paint(&mut scene).is_some(),
        "with its host out of reach the person works here: the app's action stays"
    );
    assert!(
        crate::workspace_sync::take_spinner_damage().is_none(),
        "a host out of reach is no wait to spin through"
    );

    crate::workspace_sync::set(told(WorkspaceSyncState::HostNeedsUpdate));
    assert!(paint(&mut scene).is_none());
    assert!(
        crate::workspace_sync::take_spinner_damage().is_none(),
        "a host that has to update is no wait to spin through"
    );

    crate::workspace_sync::set(told(WorkspaceSyncState::Synced));
    assert!(
        paint(&mut scene).is_some(),
        "once the workspace synced, its emptiness is the app's to say"
    );
    crate::workspace_sync::set(None);
    assert!(
        paint(&mut scene).is_some(),
        "a workspace with no sync state is the app's"
    );
}

#[test]
fn a_screen_reader_hears_what_the_screen_says_before_the_workspace_synced() {
    use crate::workspace_sync::{WorkspaceSync, WorkspaceSyncState};
    let empty = EmptyState::new("No notes yet").with_action("New note");
    let told = |state| {
        Some(WorkspaceSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        })
    };

    crate::workspace_sync::set(told(WorkspaceSyncState::Connecting));
    let waiting = Widget::accessibility(&empty).unwrap();
    assert_eq!(
        waiting.name.as_deref(),
        Some(crate::workspace_sync::waiting().unwrap().title.as_str())
    );
    assert!(waiting.name.as_deref().unwrap().contains("Lounge"));
    assert!(waiting.state.busy, "the connecting state is on its way");
    assert!(waiting.children.is_empty(), "the app's action waits too");

    crate::workspace_sync::set(told(WorkspaceSyncState::Unreachable));
    let unreachable = Widget::accessibility(&empty).unwrap();
    assert!(unreachable.name.as_deref().unwrap().contains("Lounge"));
    assert!(!unreachable.state.busy, "nothing is on its way");
    assert!(
        unreachable.description.is_some(),
        "it says changes stay here"
    );
    assert_eq!(
        unreachable
            .children
            .first()
            .and_then(|action| action.name.as_deref()),
        Some("New note"),
        "the person works on what is here"
    );

    crate::workspace_sync::set(told(WorkspaceSyncState::Synced));
    let empty_node = Widget::accessibility(&empty).unwrap();
    assert_eq!(empty_node.name.as_deref(), Some("No notes yet"));
    assert!(!empty_node.state.busy);
    assert_eq!(
        empty_node
            .children
            .first()
            .and_then(|action| action.name.as_deref()),
        Some("New note")
    );
    crate::workspace_sync::set(None);
}

/// Open Details read in the order they were said, one line each, from one left edge; a command to
/// run stands on a line of its own, and its Copy action, pressed or called, copies exactly it.
#[test]
fn details_keep_their_lines_and_copy_the_command_they_name() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
    let clipboard = std::sync::Arc::new(InProcessClipboard::default());
    let services = crate::process::ProcessServices::null().with_clipboard(clipboard.clone());
    crate::process::scoped(&services, || {
        let mut es = EmptyState::new("Settings could not open")
            .with_action("Try again")
            .on_open(|| {})
            .with_details(
                "kernel.x turns off user namespaces, which every app's sandbox is made in.\n\
                 Turn them on:\n    sudo sysctl kernel.x=1\n\
                 Keep them on after a restart with this line in a file under /etc/sysctl.d/:\n    \
                 kernel.x = 1\n\
                 so.tilo.settings ended: exit status: 1",
            );
        lay_out(&mut es, 600.0, 520.0);
        es.toggle_details();
        lay_out(&mut es, 600.0, 520.0);
        let lines = es.details_lines.clone();
        let said: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            said,
            [
                "kernel.x turns off user namespaces, which every app's sandbox is made in.",
                "Turn them on:",
                "sudo sysctl kernel.x=1",
                "Keep them on after a restart with this line in a file under /etc/sysctl.d/:",
                "kernel.x = 1",
                "so.tilo.settings ended: exit status: 1",
            ]
        );
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[0].rect.y1 <= pair[1].rect.y0
                    && (pair[0].rect.x0 - pair[1].rect.x0).abs() < f64::EPSILON),
            "one line under the other, from one left edge: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| es.get_rect().contains(line.rect.center())),
            "inside the state: {lines:?}"
        );
        let command = lines
            .iter()
            .position(|line| line.text == "sudo sysctl kernel.x=1")
            .unwrap();
        assert_eq!(lines[command].kind, details::Kind::Verbatim);
        assert!(lines.iter().filter(|line| line.copy.is_some()).count() == 2);

        let copy = lines[command].copy.unwrap();
        dispatch(&mut es, &cursor(copy.center()));
        assert!(dispatch(&mut es, &mouse_press()), "the press copies");
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("sudo sysctl kernel.x=1")
        );
        assert!(es.details_open(), "copying leaves the Details open");

        let node = Widget::accessibility(&es).unwrap();
        let file_line = node
            .children
            .iter()
            .find(|child| child.value.as_deref() == Some("kernel.x = 1"))
            .expect("the line for the file is read exactly");
        assert!(file_line.state.read_only);
        let target = file_line.children[0].action_target.clone().unwrap();
        assert!(
            crate::widget::controls::control_action_bindings(&es)
                .iter()
                .any(|binding| binding.control_id == target),
            "copying it is a control agents find"
        );
        assert!(Widget::perform_semantic_action(
            &mut es,
            &target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("kernel.x = 1")
        );
    });
}

/// A quote of the thing's own words stays in view under the message, is read out as its words,
/// and a 10 KB line of output is cut to a few lines rather than pushing the card apart.
#[test]
fn a_quote_of_the_things_own_words_stays_in_view_and_bounded() {
    let theme = Theme::dark();
    let style = StackStyle::resolved(&theme, false);
    let said = "called `Option::unwrap()` on a `None` value";
    let content = EmptyStateContent {
        message: Some("It stopped before its window appeared."),
        quote: Some(said),
        ..EmptyStateContent::new("Notes could not open")
    };
    let mut text = TextRenderer::new();
    let measured = style.measure_with(&mut text, &theme, 600.0, &content);
    assert!(measured.quote_h > 0.0, "the quote takes a line of its own");
    let one_line = f64::from(style.quote.line_height_px_at(theme.text_scale));
    assert!(
        measured.quote_h <= one_line * 3.0 + style.quote_pad_y * 2.0 + 0.5,
        "a short quote is one line in its well: {}",
        measured.quote_h
    );

    // Ten kilobytes of one line: the state shows its first lines, never all of it.
    let long = "the database went away and the request could not be retried ".repeat(200);
    let mut text = TextRenderer::new();
    let long_measured = style.measure_with(
        &mut text,
        &theme,
        400.0,
        &EmptyStateContent {
            quote: Some(&long),
            ..EmptyStateContent::new("Photos could not open")
        },
    );
    assert!(
        long_measured.quote_h <= one_line * 3.0 + style.quote_pad_y * 2.0 + 0.5,
        "a long line is cut to the lines the state can show: {}",
        long_measured.quote_h
    );

    // What is drawn is also what assistive technology reads: the app's words, named as its.
    let state = EmptyState::new("Notes could not open").with_quote(said);
    let node = Widget::accessibility(&state).expect("the state is described");
    assert_eq!(
        node.description.as_deref(),
        Some("It said: called `Option::unwrap()` on a `None` value")
    );
}
