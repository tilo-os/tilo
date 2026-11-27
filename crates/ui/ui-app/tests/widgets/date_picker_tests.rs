use automerge::AutoCommit;
use automerge::ROOT;
use jiff::civil::Date;
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::prelude::TaffyMaxContent;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::date_picker::WEEKDAY_LABELS;
use tilo_ui::widget::date_picker::{HEADER_HEIGHT, WEEKDAY_HEIGHT};
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    /// A date picker: its month, selection, semantics and bindings.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_date_picker_binds_navigates_and_keeps_its_month;

    // A month the app feeds hands what is chosen to the app, so the
    // native app contract finds it bound, addressed by its name.
    phase a_fed_month_is_a_bound_control;
    {
        let feed = DateFeed::new();
        let day = Date::new(2024, 6, 1).unwrap();
        feed.show(DateShown {
            selected: day,
            today: day,
            marked: Vec::new(),
        });
        let host = tilo_ui::app::driver::Driver::for_root(
            tilo_ui::State::new(),
            DatePicker::new(day)
                .navigator()
                .with_accessible_name("Month")
                .fed_by(&feed),
            (400, 400),
            Theme::dark(),
        );
        let audit = host.native_app_audit();
        assert!(audit.issues.is_empty(), "{audit:?}");
        assert_eq!(
            host.root::<DatePicker>()
                .accessibility()
                .and_then(|node| node.action_target),
            Some("date-picker/day".to_owned()),
            "a fed picker is reached by its choice's id where it is drawn"
        );
    }

    phase clicking_a_date_focuses_the_grid_for_keyboard_navigation;
    {
        let initial = Date::new(2024, 6, 1).unwrap();
        let state = tilo_ui_base::LocalState::new(initial);
        let mut host = tilo_ui::app::driver::Driver::for_root(
            tilo_ui::State::new(),
            DatePicker::from_state(&state),
            (400, 400),
            Theme::dark(),
        );
        let target = Date::new(2024, 6, 7).unwrap();
        let point = host
            .root::<DatePicker>()
            .test_day_rects()
            .iter()
            .find(|(date, _)| *date == target)
            .unwrap()
            .1
            .center();
        host.click_at(point.x, point.y);
        assert!(host.root::<DatePicker>().is_focused());
        assert_eq!(state.get(), target);
        host.key("Right");
        assert_eq!(state.get(), target.tomorrow().unwrap());
        let later = target + jiff::Span::new().days(9);
        let point = host
            .root::<DatePicker>()
            .test_day_rects()
            .iter()
            .find(|(date, _)| *date == later)
            .unwrap()
            .1
            .center();
        host.click_at(point.x, point.y);
        assert_eq!(state.get(), later);
        host.poll_frame().unwrap();
        assert!(
            host.root::<DatePicker>().needs_animation_frame(),
            "the chosen disc slides to the day picked rather than jumping"
        );
        host.key("Left");
        assert_eq!(state.get(), later.yesterday().unwrap());
        // 2024-06-15 is a Saturday: End goes to the week's Sunday, Home to its Monday.
        host.key("End");
        assert_eq!(state.get(), Date::new(2024, 6, 16).unwrap());
        host.key("Home");
        assert_eq!(state.get(), Date::new(2024, 6, 10).unwrap());
        host.key("Shift+PageDown");
        assert_eq!(host.root::<DatePicker>().month_view(), (2025, 6));
        host.key("PageUp");
        assert_eq!(host.root::<DatePicker>().month_view(), (2025, 5));
    }

    phase date_picker_month_navigation_and_selection;
    {
        fn date_picker_first_day_of_week_is_monday() {
            let dp = DatePicker::new(Date::new(2024, 1, 1).unwrap());
            let first_col: usize = dp.first_visible_col();
            assert_eq!(first_col, 0_usize);
        }

        /// Header label, chevrons, weekday row and day numbers all centre their
        /// measured line in their band; the font size alone sat each 1.5 px low.
        fn picker_text_centres_its_measured_line_in_every_band() {
            let selected = Date::new(2026, 5, 22).expect("valid date");
            let mut picker = DatePicker::new(selected);
            let rect = Rect::new(0.0, 0.0, 224.0, 240.0);
            picker.apply_layout_external(rect);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            picker.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, PresenceCtx::default().draw_env()));
            let style = theme.text(TextRole::Caption);
            // Draw order: header label, seven weekday labels, then day cells;
            // the month steps are icons, not glyph runs.
            let header_band = (rect.y0, HEADER_HEIGHT);
            let weekday_band = (rect.y0 + HEADER_HEIGHT, WEEKDAY_HEIGHT);
            let first_cell = picker
                .test_day_rects()
                .first()
                .map(|(_, r)| *r)
                .expect("day cells");
            let expectations: Vec<(&str, (f64, f64))> = std::iter::once(("May 2026", header_band))
                .chain(WEEKDAY_LABELS.iter().map(|label| (*label, weekday_band)))
                .chain(std::iter::once((
                    "27",
                    (first_cell.y0, first_cell.height()),
                )))
                .collect();
            for (index, (text, (band_y0, band_h))) in expectations.into_iter().enumerate() {
                tilo_ui::widget::internals::text::assert_glyph_run_centred_in_band(
                    &scene,
                    index,
                    text,
                    &style,
                    band_y0,
                    band_h,
                    &mut tr,
                    theme.text_primary,
                );
            }
        }

        fn focused_date_picker_paints_a_focus_ring() {
            let without = picker_color_fills(false);
            let with = picker_color_fills(true);
            assert_eq!(
                with,
                without + 1,
                "a focused date picker must add exactly one focus-ring stroke ({with} vs {without})",
            );
        }

        date_picker_first_day_of_week_is_monday();
        picker_text_centres_its_measured_line_in_every_band();
        focused_date_picker_paints_a_focus_ring();
    }

    phase date_picker_semantics_and_shared_state_track_selection;
    {
        fn accessibility_reports_combobox_role_and_selected_date_value() {
            use tilo_ui_base::a11y::Role;
            let dp = DatePicker::new(Date::new(2024, 6, 15).unwrap());
            let node = Widget::accessibility(&dp).expect("date picker is a selector");
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(node.value.as_deref(), Some("2024-06-15"));
            assert!(node.state.focusable);
            assert_eq!(node.state.expanded, None, "the grid is rendered inline");
        }

        fn local_state_keeps_date_pickers_in_sync() {
            let first = Date::new(2026, 3, 12).unwrap();
            let second = Date::new(2027, 8, 4).unwrap();
            let state = tilo_ui_base::LocalState::new(first);
            let mut left = DatePicker::from_state(&state);
            let mut right = DatePicker::from_state(&state);
            let doc = AutoCommit::new();

            left.set_selected(second);
            assert_eq!(state.with(|date| *date), second);
            Widget::sync(&mut right, &doc, &automerge::ROOT);
            assert_eq!(right.selected(), second);
            assert_eq!(right.month_view(), (2027, 8));
        }

        fn semantic_date_change_updates_state_and_callback() {
            let initial = Date::new(2026, 1, 1).unwrap();
            let expected = Date::new(2026, 11, 23).unwrap();
            let state = tilo_ui_base::LocalState::new(initial);
            let captured = Rc::new(RefCell::new(None));
            let callback = Rc::clone(&captured);
            let mut picker = DatePicker::from_state(&state).on_choose("date-picker", move |date| {
                *callback.borrow_mut() = Some(date)
            });
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_action(
                &mut picker,
                &tilo_ui_base::a11y::Action::SetValue("2026-11-23".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|date| *date), expected);
            assert_eq!(*captured.borrow(), Some(expected));

            assert!(!Widget::perform_action(
                &mut picker,
                &tilo_ui_base::a11y::Action::SetValue("2026-02-30".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|date| *date), expected);
        }

        accessibility_reports_combobox_role_and_selected_date_value();
        local_state_keeps_date_pickers_in_sync();
        semantic_date_change_updates_state_and_callback();
    }

    phase a_navigator_fills_its_column_and_marks_busy_days;
    {
        let day = Date::new(2026, 9, 14).unwrap();
        let picker = DatePicker::new(day)
            .navigator()
            .marked([day, day.tomorrow().unwrap()]);
        assert_eq!(picker.test_marked().len(), 2);
        let narrow = picker.intrinsic_height(168.0);
        let wide = picker.intrinsic_height(224.0);
        assert!(narrow < wide, "cells follow the column's width");
        let app = tilo_ui::app::driver::Driver::new(
            tilo_ui::State::new(),
            Box::new(tilo_ui::column![picker.with_accessible_name("Month")]),
            (200, 600),
        );
        let _ = app.targets();
    }

    phase a_bound_text_day_is_shown_and_followed;
    {
        let state = tilo_ui_base::LocalState::new("2026-06-30".to_owned());
        let mut picker = DatePicker::from_text_state(&state);
        assert_eq!(
            picker.selected(),
            Date::new(2026, 6, 30).unwrap(),
            "the day the binding holds is shown"
        );
        state.set("2026-07-04".to_owned());
        picker.sync(&(), &());
        assert_eq!(
            picker.selected(),
            Date::new(2026, 7, 4).unwrap(),
            "a day set elsewhere shows in place"
        );
    }

    phase a_change_of_props_stays_on_the_month_the_person_walked_to;
    {
        let mut picker = DatePicker::new(Date::new(2026, 6, 15).unwrap());
        picker.advance_month();
        let walked_to = picker.month_view();
        assert!(
            <DatePicker as tilo_ui_base::component::Described>::set_props(
                &mut picker,
                tilo_ui::widget::DatePickerProps::default()
            ),
            "a day set elsewhere is taken where it stands"
        );
        assert_eq!(
            picker.month_view(),
            walked_to,
            "the calendar stays on the month it was walked to"
        );
    }
}

fn picker_color_fills(focused: bool) -> usize {
    use vello::Scene;
    use vello_encoding::DrawTag;
    let mut dp = DatePicker::new(Date::new(2024, 6, 15).unwrap());
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let node = dp.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(node, taffy::Size::MAX_CONTENT)
        .unwrap();
    dp.apply_layout(&taffy, node, Point::ZERO);
    dp.set_focus(focused);

    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    dp.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        presence.draw_env(),
    ));
    scene
        .encoding()
        .draw_tags
        .iter()
        .filter(|t| **t == DrawTag::COLOR)
        .count()
}
