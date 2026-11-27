use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::stat_tile::*;
use tilo_ui_app::widget::*;
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
use tilo_ui_base::widget::Tone;
use tilo_ui_base::widget::*;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    /// A stat tile laid out without a host.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_stat_tile_sizes_elides_activates_and_reads;

    phase tile_height_comes_from_its_lines_and_follows_the_text_scale;
    {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let plain = StatTileFace::new("Protected", "1.2").unit("TB");
        let full = plain
            .change("4% this week", Trend::Up)
            .sparkline(&[1.0, 3.0, 2.0, 5.0]);
        let h = plain.height(&mut text, &theme);
        assert!(full.height(&mut text, &theme) > h);
        assert!(
            plain.hero().height(&mut text, &theme) > h,
            "Display is taller"
        );
        assert!(plain.height(&mut text, &Theme::dark().with_text_scale(2.0)) > h * 1.8);
    }

    phase a_tile_is_not_a_card;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let face = StatTileFace::new("Snapshots", "128");
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 200.0, 80.0),
        );
        let fills_before_text = scene.encoding().draw_tags.len();
        let mut toned = Scene::new();
        face.tone(Tone::Danger).paint(
            &mut PaintCtx::new(&mut toned, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 200.0, 80.0),
        );
        assert_eq!(
            toned.encoding().draw_tags.len(),
            fills_before_text + 1,
            "a tone adds only the ticker"
        );
    }

    phase a_long_value_elides_inside_the_tile;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let value = test_tile_value_style(&theme);
        let shown = text.elide_to_width_style("1,284,000,000,000", 80.0, &value);
        assert!(shown.ends_with('…'));
    }

    phase a_rebuilt_group_starts_with_the_columns_of_its_width;
    // A group built again (the window's tree rebuilt) starts with the
    // columns of the width it was laid out at, so its first solve is its
    // height: a scroll view around it never sees the tiles stacked, an
    // overflow that is not there.
    {
        let theme = Theme::dark();
        let mut group = StatGroup::new(
            ["Allowed services", "Allow rules", "Deny rules"]
                .into_iter()
                .map(|l| Box::new(StatTile::new(l, "3")) as Element)
                .collect(),
        );
        let columns = |group: &mut StatGroup| {
            let mut taffy = TaffyTree::new();
            let node = group.build_layout(&mut taffy, &theme, &mut HashMap::new());
            taffy
                .style(node)
                .map(|style| style.grid_template_columns.len())
                .unwrap_or_default()
        };
        assert_eq!(columns(&mut group), 1, "no width yet: one flexible column");
        group.test_set_rect(Rect::new(0.0, 0.0, 1000.0, 80.0));
        assert_eq!(group.columns_for_width(1000.0), 3);
        assert_eq!(columns(&mut group), 3, "laid out 1000 wide: all in one row");
    }

    phase a_pressable_tile_activates_and_speaks_its_value;
    {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = hits.clone();
        let mut tile = StatTile::new("CPU", "42")
            .unit("%")
            .change("3% in the last minute", Trend::Down)
            .on_open("cpu", move || counter.set(counter.get() + 1));
        let node = Widget::accessibility(&tile).unwrap();
        assert_eq!(node.name.as_deref(), Some("CPU"));
        assert_eq!(
            node.value.as_deref(),
            Some("42 %, down 3% in the last minute")
        );
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut tile,
            &tilo_ui_base::a11y::Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(hits.get(), 1);
    }

    phase a_note_is_read_with_the_value_and_tiles_update_in_place;
    {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let plain = StatTileFace::new("Used", "1.2").unit("TB");
        let noted = plain.note("of 2 TB");
        assert!(noted.height(&mut text, &theme) > plain.height(&mut text, &theme));
        let node = noted.accessibility(Rect::new(0.0, 0.0, 200.0, 80.0));
        assert_eq!(node.name.as_deref(), Some("Used"));
        assert_eq!(node.value.as_deref(), Some("1.2 TB of 2 TB"));
        assert!(node.bounds.is_some());

        let mut group = StatGroup::new(vec![
            Box::new(StatTile::new("Download", "0")),
            Box::new(StatTile::new("Upload", "0")),
        ]);
        group.tile_mut(1).expect("a stat tile").set_value("48");
        assert_eq!(
            Widget::accessibility(group.tile_mut(1).unwrap())
                .unwrap()
                .value
                .as_deref(),
            Some("48")
        );
        group.set_tiles(vec![Box::new(StatTile::new("Ping", "12").unit("ms"))]);
        assert!(group.take_structure_dirty());
        assert!(group.tile_mut(1).is_none());
    }
}

#[test]
fn groups_reflow_four_two_one_and_never_leave_an_orphan() {
    assert_eq!(columns_for(4, 1000.0, 140.0, 24.0), 4);
    assert_eq!(
        columns_for(4, 500.0, 140.0, 24.0),
        2,
        "3 fit, but 2×2 beats 3+1"
    );
    assert_eq!(columns_for(4, 200.0, 140.0, 24.0), 1);
    assert_eq!(columns_for(3, 500.0, 140.0, 24.0), 3);
    assert_eq!(columns_for(3, 400.0, 140.0, 24.0), 1);
    let group = StatGroup::new(
        ["Protected", "Snapshots", "Next run", "Last backup"]
            .into_iter()
            .map(|l| Box::new(StatTile::new(l, "12")) as Element)
            .collect(),
    );
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(group),
        (520, 400),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let ys: Vec<f64> = root.children.iter().map(|c| c.rect[1]).collect();
    assert_eq!(ys[0], ys[1]);
    assert!(ys[2] > ys[0]);
    assert_eq!(ys[2], ys[3]);
}

#[test]
fn a_detail_tile_is_admitted_for_pointer_and_assistive_activation() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = calls.clone();
    let tile = StatTile::new("CPU", "42").on_open("cpu.detail", move || {
        count.set(count.get() + 1);
    });
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), tile, (280, 140), Theme::dark());
    let audit = harness.native_app_audit();
    assert!(audit.is_certified(), "{audit:?}");
    let target = harness.targets()["cpu.detail"];
    harness.click_at(target.center().x, target.center().y);
    assert_eq!(calls.get(), 1);
    harness
        .perform_named("cpu.detail", &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(calls.get(), 2);
}
