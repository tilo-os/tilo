//! The main panel's unit tests.

use super::*;
use automerge::ObjId;
use ui::widget::BlockKind;
use uuid::Uuid;

include!("local_capture_tests.rs");

fn fresh_page_id() -> PageId {
    Uuid::new_v4()
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn page_titles_and_empty_state_are_accessible_rename_boundaries() {
    eprintln!("phase set_title_marks_dirty_when_changed");
    {
        let mut p = MainPanel::new();
        p.mark_drawn();
        assert!(!p.needs_redraw());
        p.set_title(Some("Hello".into()));
        assert!(p.needs_redraw());
        p.mark_drawn();
        p.set_title(Some("Hello".into()));
        assert!(!p.needs_redraw());
    }

    eprintln!("phase accessible_page_title_queues_the_same_rename_boundary");
    {
        let mut panel = MainPanel::new();
        let id = fresh_page_id();
        panel.load_page(id, "Launch", "");
        let node = panel
            .accessibility()
            .expect("open pages expose their title");
        assert_eq!(node.role, ui::a11y::Role::Group);
        let title = node
            .children
            .first()
            .expect("header exposes the page title");
        assert_eq!(title.role, ui::a11y::Role::TextInput);
        assert_eq!(title.action_target.as_deref(), Some("notes.page-title"));
        let target = title.action_target.as_deref().unwrap();
        let mut doc = AutoCommit::new();
        assert!(panel.perform_semantic_action(
            target,
            &ui::a11y::Action::SetValue("Launch plan".into()),
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            panel.take_title_rename().is_none(),
            "typing remains an uncommitted inline rename"
        );
        panel.title_label.commit();
        assert_eq!(panel.take_title_rename().as_deref(), Some("Launch plan"));
    }

    eprintln!("phase empty_state_primary_action_is_a_focused_accessible_button");
    {
        let mut p = MainPanel::new();
        p.set_focus(true);
        let node = p.accessibility().expect("empty state is exposed");
        assert_eq!(node.role, ui::a11y::Role::Button);
        assert_eq!(node.name.as_deref(), Some("New note"));
        assert!(node.state.focusable);
        assert!(node.state.focused);

        let mut doc = AutoCommit::new();
        assert!(p.perform_action(
            &ui::a11y::Action::Activate,
            &[],
            &mut Edits::lend(&mut doc),
            &ObjId::Root
        ));
        assert!(p.take_empty_action());
    }

    eprintln!("phase surface_hints_follow_hit_targets_and_reveal_exact_compact_dates");
    {
        use ui::headless::Framed;
        use ui::tilo_actions::Action;
        crate::metadata::register_messages();
        let page = fresh_page_id();
        let tag = fresh_page_id();
        let backlink = fresh_page_id();
        let mut doc = AutoCommit::new();
        crate::actions::create_page_action(page, None, "Launch")
            .apply(&mut doc)
            .unwrap();
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(1_700_000_000));
        crate::actions::edit_body_action(
            page,
            tilo_document::RichTextAction::AppendParagraph {
                text: "Body".into(),
            },
        )
        .apply(&mut doc)
        .unwrap();
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(1_700_003_600));
        let mut panel = MainPanel::new();
        panel.load_page(page, "Launch", "Body");
        panel.set_tags(vec![TagChip {
            id: tag,
            name: "Research".into(),
        }]);
        panel.set_backlinks(vec![BacklinkChip {
            page: backlink,
            title: "A long source note title".into(),
            references: 1,
        }]);
        let mut panel = Framed::new(ui::State::from_doc(doc), panel, (300, 600), Theme::dark());
        let targets = Framed::harness(&panel).targets();
        let tag_rect = targets[&format!("notes.tag-chip:{tag}")];
        let backlink_rect = targets[&format!("notes.backlink-chip:{backlink}")];
        let date_rect = targets["notes.caption:0"];
        for rect in [panel.title_rect, tag_rect, backlink_rect, date_rect] {
            assert!(
                !rect.is_zero_area(),
                "the retained target is laid out: {rect:?}"
            );
            assert!(
                panel.rect.contains(rect.origin())
                    && panel
                        .rect
                        .contains(Point::new(rect.x1 - 0.1, rect.y1 - 0.1)),
                "header target fits the panel: {rect:?}"
            );
        }
        assert_eq!(
            panel.cursor_shape(panel.title_rect.center()),
            Some(winit::window::CursorIcon::Text)
        );
        assert!(
            panel.tooltip_target_at(panel.title_rect.center()).is_none(),
            "the title is a text field: its caret says it is editable"
        );
        assert!(
            panel
                .tooltip_target_at(tag_rect.center())
                .unwrap()
                .1
                .contains("Research")
        );
        assert!(
            panel
                .tooltip_target_at(backlink_rect.center())
                .unwrap()
                .1
                .contains("A long source note title")
        );
        let (target, label) = panel.tooltip_target_at(date_rect.center()).unwrap();
        assert_eq!(target, date_rect);
        assert_eq!(label, panel.dates.display_labels(false).join("\n"));
        assert_eq!(
            label.lines().count(),
            2,
            "both persisted dates are revealed"
        );
        assert!(
            label.lines().all(|line| line.contains(':')),
            "compact metadata exposes each exact time"
        );
        assert!(panel.tooltip_target_at(Point::new(400.0, 400.0)).is_none());
        assert!(panel.tooltip_target_at(Point::new(100.0, 500.0)).is_none());
        panel.suppress_tooltips(true);
        for rect in [panel.title_rect, tag_rect, backlink_rect, date_rect] {
            assert!(panel.tooltip_target_at(rect.center()).is_none());
        }
        panel.suppress_tooltips(false);
        for rect in [tag_rect, backlink_rect, date_rect] {
            assert!(panel.tooltip_target_at(rect.center()).is_some());
        }
    }
}

#[test]
fn main_panel_loads_snapshots_and_clears_page_bodies() {
    eprintln!("phase load_page_seeds_editor_from_markdown");
    {
        let mut p = MainPanel::new();
        let id = fresh_page_id();
        p.load_page(id, "Hello", "# H\n\nbody\n");
        assert_eq!(p.page_id(), Some(id));
        assert_eq!(p.editor().blocks().len(), 2);
        assert!(matches!(p.editor().blocks()[0].kind, BlockKind::Heading(1)));
    }

    eprintln!("phase load_page_preserves_external_link_presentation");
    {
        let mut panel = MainPanel::new();
        panel.load_page(
            fresh_page_id(),
            "Links",
            "Read [the guide](https://example.test/guide).\n",
        );
        assert!(matches!(
            &panel.editor().blocks()[0].line.marks()[0].mark,
            ui::widget::Mark::ExternalLink(url) if url == "https://example.test/guide"
        ));
    }

    eprintln!("phase take_body_snapshot_round_trips");
    {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T", "# H\n\nbody\n");
        let snap = p.take_body_snapshot();
        assert_eq!(snap, "# H\n\nbody\n");
    }

    eprintln!("phase clear_resets_state");
    {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T", "# H\n");
        p.clear();
        assert!(p.page_id().is_none());
        assert!(p.title().is_none());
    }
}

// Catches a note maximised on a large display drawing a laptop-sized
// strip of small text in a field of empty space.
#[test]
fn a_note_in_a_wide_window_grows_its_type_and_keeps_its_measure_centred() {
    use ui::headless::Framed;
    let laid_out = |width: u32| {
        let mut panel = MainPanel::new();
        panel.load_page(fresh_page_id(), "Launch", "First body paragraph");
        let panel = Framed::new(ui::State::new(), panel, (width, 900), Theme::dark());
        let line = panel.editor.editor().blocks()[0].line.get_rect().height();
        let column = panel.title_rect;
        (panel.zoom, line, column)
    };
    let (laptop_zoom, laptop_line, laptop_column) = laid_out(1000);
    assert!((laptop_zoom - 1.0).abs() < f32::EPSILON);
    // A 1440p display at 1.5x keeps the laptop page.
    assert!((laid_out(1427).0 - 1.0).abs() < f32::EPSILON);
    let (zoom, line, column) = laid_out(2280);
    assert!(
        (zoom - ui::theme::MAX_DOCUMENT_ZOOM).abs() < f32::EPSILON,
        "{zoom}"
    );
    assert!(line > laptop_line * 1.4, "{line} vs {laptop_line}");
    assert!(column.width() > laptop_column.width() * 1.3);
    // Still one centred reading column, not lines as wide as the screen.
    let (left, right) = (column.x0, 2280.0 - column.x1);
    assert!(
        column.width() < 1140.0 && (left - right).abs() < 120.0,
        "{column:?}"
    );
}

#[test]
fn metadata_bands_tags_and_backlinks_clear_the_editor_and_keep_identity() {
    use ui::headless::Framed;
    fn mounted(width: u32, theme: Theme) -> (Framed<MainPanel>, Vec<crate::model::TagId>, PageId) {
        let mut panel = MainPanel::new();
        panel.load_page(fresh_page_id(), "Launch", "First body paragraph");
        let tags = ["Release planning", "Customer research", "Accessibility"]
            .into_iter()
            .map(|name| TagChip {
                id: fresh_page_id(),
                name: name.into(),
            })
            .collect::<Vec<_>>();
        let ids = tags.iter().map(|t| t.id).collect::<Vec<_>>();
        panel.set_tags(tags);
        panel.set_parent_path("Projects / Launch".into());
        let source = fresh_page_id();
        panel.set_backlinks(vec![BacklinkChip {
            page: source,
            title: "Long-running launch notes".into(),
            references: 2,
        }]);
        (
            Framed::new(ui::State::new(), panel, (width, 700), theme),
            ids,
            source,
        )
    }
    for (width, scale) in [(800, 1.0), (300, 2.0)] {
        let theme = Theme::dark().with_text_scale(scale);
        let (panel, tags, source) = mounted(width, theme.clone());
        let targets = Framed::harness(&panel).targets();
        let chips = tags
            .iter()
            .map(|tag| targets[&format!("notes.tag-chip:{tag}")])
            .collect::<Vec<_>>();
        assert_eq!(chips.len(), 3);
        assert!(
            chips
                .iter()
                .all(|r| r.y0 >= panel.title_rect.y1 + f64::from(theme.tokens.spacing.s1)),
            "title and tags have separate bands"
        );
        assert!(
            chips.iter().all(|r| r.y0 >= panel.scroll_viewport().y0),
            "metadata starts inside the scrolling viewport"
        );
        assert!(
            chips
                .iter()
                .all(|r| r.x0 >= 0.0 && r.x1 <= f64::from(width)),
            "tags remain inside the panel"
        );
        if width == 300 {
            assert!(chips[1].y0 > chips[0].y0, "long tags wrap");
        }
        let backlink = targets[&format!("notes.backlink-chip:{source}")];
        let editor_top = panel.rect.y0 + panel.editor_top_inset();
        assert!(chips.iter().all(|r| r.y1 <= editor_top));
        assert!(
            backlink.y1 <= editor_top,
            "incoming-link metadata clears the first block"
        );
    }
}
