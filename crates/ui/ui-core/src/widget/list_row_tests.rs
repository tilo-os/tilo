use super::*;
use std::cell::Cell;
use std::rc::Rc;
use vello::Scene;
use vello::kurbo::Affine;

fn geometry(face: &ListRowFace<'_>, rect: Rect) -> RowGeometry {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    face.geometry(rect, &RowMetrics::new(&theme), &mut text, &theme)
}

/// `row` laid out alone in a window `size` big, as the window's root.
fn laid_out(mut row: ListRow, (width, height): (u32, u32), theme: &Theme) -> ListRow {
    crate::testing::lay_out_in(&mut row, width as f32, height as f32, theme)
        .expect("the row lays out");
    row
}

fn hovered() -> Interaction {
    Interaction {
        hovered: true,
        ..Interaction::default()
    }
}

tilo_test_env::scenario! {
    fn a_rows_actions_are_shown_and_run;

    phase hover_actions_replace_the_meta_but_not_the_value;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let actions = [
            RowAction {
                icon: Icon::Reply,
                label: "Reply",
            },
            RowAction {
                icon: Icon::Trash,
                label: "Delete message",
            },
        ];
        let idle = ListRowFace::new("Ada").meta("09:41").actions(&actions);
        assert!(idle.action_rects(rect, &theme).is_empty(), "hidden at rest");
        let hover = idle.clone().interaction(hovered());
        let rects = hover.action_rects(rect, &theme);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].x1 < rects[1].x0, "in order, not touching");
        for r in &rects {
            assert!(rect.contains(r.origin()) && r.x1 <= rect.x1);
            assert!(
                (r.center().y - rect.center().y).abs() < 0.01,
                "centred on the row"
            );
        }
        let valued = ListRowFace::new("Tokyo")
            .title_value("09:41")
            .actions(&actions)
            .interaction(hovered());
        let geo = geometry(&valued, rect);
        let rects = valued.action_rects(rect, &theme);
        let value_left = geo.value_right
            - f64::from(
                TextRenderer::new()
                    .measure_style("09:41", None, &RowMetrics::new(&theme).value_title)
                    .0,
            );
        assert!(rects.last().unwrap().x1 <= value_left + 0.01);
        let touch = ListRowFace::new("Ada")
            .actions(&actions)
            .reveal_actions(true);
        assert_eq!(touch.action_rects(rect, &theme).len(), 2);
        assert!(
            hover
                .clone()
                .enabled(false)
                .action_rects(rect, &theme)
                .is_empty()
        );
    }

    phase a_rows_actions_say_where_they_stand_without_a_hover;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let mut row = ListRow::new("note-1", "Ada")
            .action(Icon::Plus, "New note under Ada", || {})
            .action(Icon::MoreHorizontal, "More actions for Ada", || {});
        row.metrics = Some(RowMetrics::new(&theme));
        row.theme = Some(theme.clone());
        row.rect = rect;
        let node = Widget::accessibility(&row).unwrap();
        let buttons: Vec<_> = node
            .children
            .iter()
            .filter(|child| {
                child
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.contains("/action/"))
            })
            .collect();
        assert_eq!(buttons.len(), 2, "one button per action");
        assert!(
            row.action_rects().is_empty(),
            "the row is at rest: it shows no action"
        );
        let band = row.text_band();
        let slots = row.with_face(|face| {
            face.clone()
                .interaction(hovered())
                .action_rects(band, &theme)
        });
        let bounds: Vec<Rect> = buttons
            .iter()
            .map(|button| match button.bounds {
                Some(bounds) => bounds.to_rect(),
                None => panic!("an action a reader can press says where it stands"),
            })
            .collect();
        assert_eq!(bounds.len(), slots.len());
        for (bounds, slot) in bounds.iter().zip(&slots) {
            assert!(
                (bounds.x0 - slot.x0).abs() < 0.01 && (bounds.x1 - slot.x1).abs() < 0.01,
                "the hovered row shows the action where its button stands: {bounds:?} vs {slot:?}"
            );
        }
    }

    phase a_row_says_where_its_actions_stand;
    {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let mut row = ListRow::new("note-1", "Ada")
            .selected(true)
            .action(Icon::Trash, "Delete", || {});
        row.set_interaction_environment(tilo_ui_base::interaction::InteractionEnvironment::TOUCH);
        row.metrics = Some(RowMetrics::new(&theme));
        row.theme = Some(theme.clone());
        row.rect = rect;
        let rects = tilo_ui_base::widget::capability::capability::<dyn crate::widget::RowActionRects>(&row)
            .expect("the row answers where its actions stand")
            .row_action_rects();
        assert_eq!(rects.len(), 1, "one action, where the row shows it");
        assert!(rect.contains(rects[0].center()), "{rect:?} vs {rects:?}");
        assert_eq!(rects, row.action_rects(), "the same place it paints");
    }

    phase hover_actions_run_from_assistive_technology_and_the_keyboard;
    {
        let replied = Rc::new(Cell::new(0));
        let deleted = Rc::new(Cell::new(0));
        let (r, d) = (Rc::clone(&replied), Rc::clone(&deleted));
        let mut row = ListRow::new("m", "Ada")
            .on_open(|| {})
            .action(Icon::Reply, "Reply", move || r.set(r.get() + 1))
            .action(Icon::Trash, "Delete message", move || d.set(d.get() + 1));
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut row,
            &tilo_ui_base::a11y::Action::Activate,
            &[1],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!((replied.get(), deleted.get()), (0, 1));
        row.set_focus(true);
        row.armed_action = Some(0);
        assert!(Widget::perform_action(
            &mut row,
            &tilo_ui_base::a11y::Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(replied.get(), 1);
        row.set_focus(false);
        assert_eq!(row.armed_action, None, "leaving the row disarms");
        // A hover action that is only a callback is not a published action, so
        // the inventory reports nothing for it; the row's own activation and
        // binding the row's action are what make it visible to a native audit.
        let bindings = tilo_ui_base::widget::controls::ActionControls::control_action_bindings(&row);
        assert!(
            bindings
                .iter()
                .all(|binding| !matches!(binding.control_id.as_str(), "Reply" | "Delete message")),
            "the row's hover actions are callbacks, not published actions: {bindings:?}"
        );
    }

    phase a_disabled_row_is_inert_and_skipped_by_focus;
    {
        let mut row = ListRow::new("r", "Row").on_open(|| {}).with_enabled(false);
        assert!(!row.is_focusable());
        assert!(!Widget::is_enabled(&row));
        let mut doc = automerge::AutoCommit::new();
        assert!(!Widget::perform_action(
            &mut row,
            &tilo_ui_base::a11y::Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
    }

    phase a_touch_pointer_shows_a_selected_rows_actions;
    {
        let mut row = ListRow::new("m", "Ada")
            .selected(true)
            .action(Icon::Trash, "Delete", || {});
        assert!(!row.reveal_actions());
        row.set_interaction_environment(tilo_ui_base::interaction::InteractionEnvironment::TOUCH);
        assert!(row.reveal_actions());
    }

    phase a_swipe_bound_to_an_action_runs_it_once_as_the_person_swipes_to_it;
    {
        let mut spec = tilo_actions::ResourceSpec::new("res-milk", "test.errand", "Errand");
        spec = spec.with_action(
            tilo_actions::ActionSpec::operation(
                "delete",
                "Delete",
                tilo_actions::OperationKind::Effect,
                "test.errand.effect",
                &(),
            )
            .expect("a test action"),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&spec, "delete").expect("published");
        let address = binding.address().clone();
        let mut row =
            ListRow::new("milk", "Buy milk").swipe_binding(Icon::Trash, "Delete", true, binding);
        row.swipe.as_mut().expect("a swipe").run();
        let taken = tilo_ui_base::widget::controls::take_control_invocation(&mut row);
        assert_eq!(
            taken.as_ref().map(|taken| taken.address.clone()),
            Some(address),
            "one swipe runs the action once"
        );
        assert!(
            tilo_ui_base::widget::controls::take_control_invocation(&mut row).is_none(),
            "the swipe is taken once"
        );
    }
}

tilo_test_env::scenario! {
    fn a_row_paints_and_speaks_its_selection_tones_and_marks;





    phase an_item_nobody_has_named_shows_a_muted_placeholder_and_is_spoken_by_it;
    {
        let untitled = tilo_ui_base::i18n::I18nManager::lookup_plain("list-row-untitled");
        assert_ne!(untitled, "list-row-untitled", "the placeholder is a phrase");
        assert!(
            !untitled.is_empty(),
            "a row of an item nobody has named yet shows words"
        );
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let row = ListRow::new("r", "");
        ListRowFace::new("").paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 320.0, 40.0),
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "an empty title paints its placeholder"
        );
        let node = Widget::accessibility(&row).unwrap();
        assert_eq!(node.name.as_deref(), Some(untitled.as_str()));

        let node = Widget::accessibility(&ListRow::new("r", "  ")).unwrap();
        assert_eq!(node.name.as_deref(), Some(untitled.as_str()));
        let node = Widget::accessibility(&ListRow::new("r", "Groceries")).unwrap();
        assert_eq!(node.name.as_deref(), Some("Groceries"));
    }

    phase the_words_a_search_found_are_marked_at_word_starts_whatever_their_case;
    {
        let line = "The glass rattled; Glassworks and fibreglass.";
        let spans = found_spans(line, &["glass"]);
        let marked: Vec<&str> = spans
            .iter()
            .filter_map(|span| line.get(span.clone()))
            .collect();
        assert_eq!(marked, ["glass", "Glass"]);
        assert!(found_spans(line, &[]).is_empty());
        assert!(found_spans(line, &[""]).is_empty());
        let spans = found_spans("Tideglass letters", &["tide", "tideglass"]);
        assert_eq!(spans, [std::ops::Range { start: 0, end: 9 }]);
        assert_eq!(
            found_spans("İstanbul", &["i̇stan"]),
            [std::ops::Range { start: 0, end: 6 }]
        );
    }

    phase a_toned_meta_is_spoken_once;
    {
        let node = ListRowFace::new("Ring the letting agent")
            .meta("Overdue since Jun 29")
            .meta_spans(&[("Overdue since Jun 29", Tone::Danger)])
            .accessibility(Rect::new(0.0, 0.0, 320.0, 40.0), "late");
        assert_eq!(node.value.as_deref(), Some("Overdue since Jun 29"));
    }

    phase a_rows_secondary_line_is_said_once_as_its_description_or_in_its_name;
    {
        let row = ListRow::new("bounce-alert", "Flash when a key is ignored")
            .secondary("Show the visual alert each time bounce keys drops a press")
            .description("Show the visual alert each time bounce keys drops a press");
        let node = Widget::accessibility(&row).unwrap();
        assert_eq!(node.name.as_deref(), Some("Flash when a key is ignored"));
        assert_eq!(
            node.description.as_deref(),
            Some("Show the visual alert each time bounce keys drops a press")
        );
        assert_eq!(node.value, None, "the explanation is said once");

        // A row's secondary line that explains nothing is said after its name, as its
        // description: a message is its sender, and its subject follows.
        let read = ListRow::new("path", "Home").secondary("/home/quinten");
        let node = Widget::accessibility(&read).unwrap();
        assert_eq!(node.name.as_deref(), Some("Home"), "named by the line it is titled with");
        assert_eq!(node.description.as_deref(), Some("/home/quinten"));
        assert_eq!(node.value, None, "the secondary line is said once, after the name");
        // The title stays a field of its own, the line the row is found by.
        assert_eq!(node.title.as_deref(), Some("Home"));

        // A row titled with nothing is named by its secondary line, which is all it says.
        let untitled = ListRow::new("path", "").secondary("/home/quinten");
        let node = Widget::accessibility(&untitled).unwrap();
        assert_eq!(node.name.as_deref(), Some("/home/quinten"));
        assert_eq!(node.value, None);
    }
}

#[test]
fn accessibility_names_the_badge_and_reads_parts_and_actions() {
    let actions = [RowAction {
        icon: Icon::Copy,
        label: "Copy URL",
    }];
    let face = ListRowFace::new("/users")
        .leading(RowLeading::Badge("GET"))
        .meta("200")
        .tags(&[("Beta", ChipStyle::Neutral)])
        .count(3)
        .checked(true)
        .actions(&actions);
    let node = face.accessibility(Rect::new(0.0, 0.0, 300.0, 40.0), "row");
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    let value = node.value.unwrap();
    assert!(value.contains("Beta") && value.contains("200") && value.contains('3'));
    assert_eq!(node.state.checked, Some(true));
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].role, Role::Button);
    assert_eq!(node.children[0].name.as_deref(), Some("Copy URL"));

    let row = ListRow::new("r", "/users")
        .badge("GET")
        .leading_tooltip("Retrieves the resource");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    assert_eq!(node.description.as_deref(), Some("Retrieves the resource"));

    let row = ListRow::new("r", "Build server")
        .icon(Icon::SquareTerminal)
        .leading_tooltip("SSH");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("Build server"));
    assert_eq!(node.description.as_deref(), Some("SSH"));
}

/// What stands under a row's text starts in the text's column, under the title and its lines,
/// and runs to the row's edge; the row grows by its height, the text keeps the band it had, and
/// a control beside the text stays beside it.
#[test]
fn a_part_below_stands_in_the_texts_column_under_the_text() {
    let theme = Theme::light();
    let m = RowMetrics::new(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let person = || {
        ListRow::new("ada", "Ada")
            .secondary("In Writer")
            .icon(Icon::User)
    };
    for with_control in [false, true] {
        let wave = || {
            Box::new(
                crate::widget::Button::new("wave", "Wave")
                    .control_size(crate::widget::ControlSize::Small),
            ) as Element
        };
        let plain = if with_control {
            person().trailing(wave())
        } else {
            person()
        };
        let plain = laid_out(plain, (720, 200), &theme);
        let plain = &plain;
        let alone = plain.with_face(|face| face.height(&mut text, &theme));
        let row = person().below(Box::new(crate::widget::Text::new(
            "Keeps their place private",
        )));
        let row = if with_control {
            row.trailing(wave())
        } else {
            row
        };
        let row = &laid_out(row, (720, 200), &theme);
        let band = row.text_band();
        let line = row.below.as_ref().unwrap().element.get_rect();
        let text_edge = row.with_face(|face| face.text_x(band, &m, &mut text, &theme));
        assert!(
            (line.x0 - text_edge).abs() < 0.5,
            "the line starts in the text's column (control: {with_control}): {line:?}, text at \
             {text_edge}"
        );
        assert!(
            line.x1 <= row.get_rect().x1 + 0.5 && line.x1 >= row.get_rect().x1 - 0.5,
            "the line runs to the row's edge (control: {with_control}): {line:?} in {:?}",
            row.get_rect()
        );
        assert!(
            line.y0 >= band.y1 - 0.5,
            "the line stands under the text (control: {with_control}): {line:?} under {band:?}"
        );
        assert!(
            (band.height() - alone).abs() < 0.5,
            "the text keeps the band it has without the line (control: {with_control}): {band:?} \
             vs {alone}"
        );
        assert!(
            row.get_rect().y1 >= line.y1 - 0.5,
            "the row holds the line (control: {with_control}): {line:?} in {:?}",
            row.get_rect()
        );
        if with_control {
            let button = row.trailing.as_ref().unwrap().element.get_rect();
            assert!(
                !row.trailing_wrapped() && button.y1 <= band.y1 + 0.5,
                "the control stays beside the text: {button:?} in {band:?}"
            );
        }
    }
}

/// A note too long for the text's column stands whole on a line of its own under the row, from
/// the text's edge to the row's, and wraps there rather than being cut where even that line is
/// too short; one that fits stays in the column. Either way the row says it.
#[test]
fn a_note_too_long_for_its_column_stands_whole_under_the_row() {
    let theme = Theme::dark();
    let m = RowMetrics::new(&theme);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let doing = "Reading Launch notes in Notes";
    let needs = f64::from(text.measure_style(doing, None, &m.secondary).0);
    let agent = || {
        ListRow::new("fieldwork", "Fieldwork")
            .note(doing)
            .icon(Icon::User)
            .trailing(Box::new(
                crate::widget::Button::new("follow", "Follow")
                    .control_size(crate::widget::ControlSize::Small),
            ))
    };
    // Wherever the note stands, it is because of the room the drawn face leaves it: a note that
    // fits the column beside the control stays in it.
    for width in (200_u32..=720).step_by(8) {
        let row = &laid_out(agent(), (width, 300), &theme);
        let band = row.text_band();
        let room = row.with_face(|face| {
            let geo = face.geometry(band, &m, &mut text, &theme);
            geo.second_right - face.text_x(band, &m, &mut text, &theme)
        });
        let under = row.note_line.as_ref().is_some_and(|line| line.under);
        assert_eq!(
            under,
            needs > room + 0.5,
            "at {width} the note needs {needs} of the {room} its column leaves"
        );
    }
    for (width, under) in [(720_u32, false), (260, true), (200, true)] {
        let row = &laid_out(agent(), (width, 300), &theme);
        let line = row
            .note_line
            .as_ref()
            .expect("a row with a note holds its line");
        assert_eq!(
            line.under,
            under,
            "at {width} the note stands {}",
            if under {
                "under the row"
            } else {
                "in its column"
            }
        );
        let node = row.accessibility().expect("the row says itself");
        assert!(
            node.value
                .as_deref()
                .is_some_and(|value| value.contains(doing)),
            "the row says the note whole at {width}: {:?}",
            node.value
        );
        if under {
            let drawn = line.text.get_rect();
            let band = row.text_band();
            let line_height = RowMetrics::line(&mut text, &m.secondary);
            if drawn.width() >= needs - 0.5 {
                assert!(
                    drawn.height() < 1.5 * line_height,
                    "words that fit the line stand on one: {drawn:?}"
                );
            } else {
                assert!(
                    drawn.height() >= 2.0 * line_height - 0.5,
                    "words longer than the line wrap onto the next, whole: {drawn:?}"
                );
            }
            if width == 260 {
                assert!(
                    drawn.width() >= needs - 0.5,
                    "a phone's row has the room the words need under it: {drawn:?} for {needs}"
                );
            }
            assert!(
                drawn.y0 >= band.y1 - 0.5 && row.get_rect().y1 >= drawn.y1 - 0.5,
                "the line stands under the text, inside the row: {drawn:?} under {band:?} in {:?}",
                row.get_rect()
            );
            let text_edge = row.with_face(|face| face.text_x(band, &m, &mut text, &theme));
            assert!(
                (drawn.x0 - text_edge).abs() < 0.5,
                "the line starts in the text's column: {drawn:?} at {text_edge}"
            );
        } else {
            assert!(
                line.text.get_rect().height() < 0.5,
                "a note that fits takes no line of its own: {:?}",
                line.text.get_rect()
            );
        }
    }
}

#[test]
fn a_leading_checkbox_toggles_apart_from_activation() {
    let toggled = Rc::new(Cell::new(None));
    let activated = Rc::new(Cell::new(0));
    let (t, a) = (Rc::clone(&toggled), Rc::clone(&activated));
    let mut row = ListRow::new("m", "Ada")
        .checkbox(false, move |on| t.set(Some(on)))
        .on_open(move || a.set(a.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &tilo_ui_base::a11y::Action::ToggleCheck,
        &[],
        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(toggled.get(), Some(true));
    assert_eq!(activated.get(), 0);
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.state.checked, Some(true));
}

fn press_leading_checkbox(row: &mut ListRow) {
    if let Some(Leading::Checkbox { checked, .. }) = &mut row.leading {
        *checked = !*checked;
        row.dirty = true;
    } else {
        panic!("the row has no leading checkbox");
    }
}

#[test]
fn a_checked_rows_title_is_struck_through_and_muted() {
    let face_strikes = |row: &ListRow| row.with_face(|face| face.struck);
    let mut row = ListRow::new("milk", "Buy oat milk").checkbox(false, |_| {});
    assert!(!face_strikes(&row), "an unchecked row's title is plain");
    press_leading_checkbox(&mut row);
    assert!(
        face_strikes(&row),
        "a checked row's title is struck through and drawn in the weaker ink"
    );
    press_leading_checkbox(&mut row);
    assert!(!face_strikes(&row), "a reopened row's title is plain again");
    assert!(
        face_strikes(&ListRow::new("stale", "Old errand").struck()),
        "a row of a removed item is struck the same way"
    );
}

#[test]
fn a_row_holds_the_name_and_description_of_the_control_it_carries() {
    let row = ListRow::new("sdr-brightness", "Brightness")
        .subtitle("External display")
        .secondary("How bright standard content is")
        .description("How bright standard content is")
        .trailing(Box::new(crate::widget::Toggle::bound("sdr", "sdr")));
    assert_eq!(
        Widget::accessibility_child_name(&row, 1).as_deref(),
        Some("Brightness")
    );
    assert_eq!(
        Widget::accessibility_child_description(&row, 1).as_deref(),
        Some("How bright standard content is, External display")
    );
    assert_eq!(Widget::accessibility_child_name(&row, 0), None);

    let plain = ListRow::new("path", "Home");
    assert_eq!(Widget::accessibility_child_name(&plain, 1), None);
    assert_eq!(Widget::accessibility_child_description(&plain, 1), None);
}

#[test]
fn an_author_cell_holds_the_person_and_the_writer_beside_them() {
    let face = ListRowFace::new("terms.txt")
        .meta("Edited")
        .author(RowAuthor {
            person: "Ada",
            identity: "ada@example.com",
            image: None,
            writer: Some(RowWriter {
                label: "Claude Code",
                hue: PresenceHue::Violet,
            }),
        });
    let rect = Rect::new(0.0, 0.0, 520.0, 36.0);
    let placed = geometry(&face, rect);
    let slot = placed
        .author
        .expect("the author cell has a slot of its own");
    assert!(
        slot.width() > 100.0,
        "the cell holds the person and the writer: {slot:?}"
    );
    assert!(
        slot.x1 <= rect.x1,
        "the cell stays inside the row: {slot:?}"
    );
    assert!(
        placed.avatar.is_none(),
        "the cell is the face, never a second one beside it"
    );
    assert!(
        placed.text_right < slot.x0,
        "the title ends before the cell: text to {}, cell at {}",
        placed.text_right,
        slot.x0
    );

    // The writer's chip makes the cell wider than the person alone.
    let alone = ListRowFace::new("terms.txt").author(RowAuthor {
        person: "Ada",
        identity: "ada@example.com",
        image: None,
        writer: None,
    });
    let mine = geometry(&alone, rect)
        .author
        .expect("a person alone still has the cell");
    assert!(
        mine.width() < slot.width(),
        "the writer's chip adds width: {mine:?} against {slot:?}"
    );
}

/// A person's check fills the box over several frames, whether the row is plain or bound (and
/// a bound row reports the check once, as its check event); a row that says it is still marks
/// at once.
/// A row's tooltip answers a pointer anywhere over its text.
/// In every theme a selected row stands off the ground while an idle one is the ground, and
/// toned facts stay legible on both.
#[test]
fn a_selection_shows_and_toned_facts_stay_legible_in_every_theme() {
    use tilo_ui_base::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let ground = theme.tokens.colors.surfaces.content;
        let fill = row_fill(&theme, true, false).expect("a selected row has a lift");
        assert!(
            tilo_ui_base::surface::edge_delta(fill, ground)
                >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
            "{}: the selection shows",
            theme.name
        );
        assert!(
            row_fill(&theme, false, false).is_none(),
            "{}: idle is the ground",
            theme.name
        );
        let lift = tilo_ui_base::tokens::colors::composite(fill, ground);
        for tone in [Tone::Success, Tone::Danger, Tone::Warning] {
            let ink = theme.tokens.colors.ink_on_any(
                &[ground, lift],
                tone.colour(&theme),
                SMALL_TEXT_MIN_CONTRAST,
            );
            for under in [ground, lift] {
                assert!(
                    tilo_ui_base::a11y::contrast_ratio(ink, under) >= SMALL_TEXT_MIN_CONTRAST,
                    "{}: {tone:?} reads on the ground and the lift",
                    theme.name
                );
            }
        }
    }
}
