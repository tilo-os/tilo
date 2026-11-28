//! The History host as an app's window drives it: its bar, its panel, its previews and what a
//! retained record's activity hands back.
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::{cell::Cell, rc::Rc};
use tilo_ui_app::history_mode::testing::*;
use tilo_ui_app::history_mode::*;
use tilo_ui_app::history_mode::{HistoryHost, SIDE_PANEL_WIDTH};
use tilo_ui_app::{State, headless::Driver, widget::Button, widget::Column};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};

#[test]
fn history_status_and_recovery_keep_the_existing_bar_geometry() {
    for width in [360.0, 900.0] {
        let state = State::new();
        let mut live = Column::new().push(Box::new(Button::new("live", "Send")));
        let mut history = HistoryHost::new(registration(), &state.doc).unwrap();
        history.set_width(width);
        let theme = Theme::dark();
        let mut renderer = tilo_ui_base::font_catalog::fixture_renderer();
        let mut baseline = None;
        for key in [
            None,
            Some("history-local-action-not-saved"),
            Some("history-sync-offline"),
            // A refused change keeps the connection, so it is its own
            // status: the bar must say what happened, not who is offline.
            Some("history-sync-refused"),
            None,
        ] {
            let status = key
                .map(tilo_ui_base::i18n::I18nManager::lookup_plain)
                .map(tilo_ui_app::widget::WindowStatus::says);
            history.set_sync_status(status.clone());
            let mut tree = taffy::TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let root = history.build_composition(&mut live, &mut tree, &theme, &mut nodes);
            tree.compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(width)),
                    height: taffy::AvailableSpace::Definite(650.0),
                },
            )
            .unwrap();
            history.apply_composition(&mut live, &tree, root, vello::kurbo::Point::ZERO);
            history.draw(&mut tilo_ui_base::draw::DrawCtx::new(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                tilo_ui_base::presence::PresenceCtx::default().draw_env(),
            ));
            let geometry = (live.get_rect(), tree.total_node_count());
            assert_eq!(
                *baseline.get_or_insert(geometry),
                geometry,
                "status must not add a row or layout nodes"
            );
            let published = history
                .children()
                .into_iter()
                .filter_map(Widget::accessibility)
                .any(|node| {
                    node.name.as_deref() == status.as_ref().map(|s| s.text.as_str())
                        && status.is_some()
                });
            assert_eq!(
                published,
                status.is_some(),
                "status and recovery must reach accessibility"
            );
            if status.is_some() {
                let bounds = history
                    .children()
                    .into_iter()
                    .find(|child| child.name() == "HeaderStatus")
                    .unwrap()
                    .get_rect();
                assert!(
                    bounds.width() > 0.0,
                    "status must be painted, not only announced"
                );
                assert!(bounds.y1 <= live.get_rect().y0);
            }
        }
    }
}

#[test]
fn first_control_capture_preserves_admitted_toolbar_until_recomposition() {
    fn addresses(widget: &dyn Widget, output: &mut Vec<*const ()>) {
        output.push(std::ptr::from_ref(widget).cast());
        widget.for_each_child(&mut |child| addresses(child, output));
    }

    let mut state = State::new();
    state.doc.put(ROOT, "body", "existing document").unwrap();
    state.commit_action(None, None);
    let mut live = Column::new().push(Box::new(Button::new("live", "Send")));
    let mut history = HistoryHost::new(registration(), &state.doc).unwrap();
    history.set_width(900.0);
    assert!(history.request_open());
    history.refresh(&state.doc);
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    history.build_composition(&mut live, &mut tree, &Theme::dark(), &mut nodes);
    assert!(!nodes.is_empty());
    assert!(!history.needs_refresh(&state.doc, 900.0));
    let mut admitted = Vec::new();
    addresses(&history, &mut admitted);

    assert!(history.capture_pending_controls(&mut live, &state.doc, uuid::Uuid::new_v4()));
    assert!(
        history.needs_refresh(&state.doc, 900.0),
        "control capture must schedule recomposition before replacing mapped controls"
    );
    let mut retained = Vec::new();
    addresses(&history, &mut retained);
    assert_eq!(admitted, retained, "admitted controls must remain alive");

    nodes.clear();
    tree = taffy::TaffyTree::new();
    history.refresh(&state.doc);
    history.build_composition(&mut live, &mut tree, &Theme::dark(), &mut nodes);
    assert!(!nodes.is_empty());
    assert!(!history.needs_refresh(&state.doc, 900.0));
    assert!(history.captures_input());
}

/// An app whose header takes the History control (Settings, Files, Notes) learns it is presenting it
/// during the first build, which rebuilds the bar to leave the control out.
#[test]
fn a_first_build_whose_header_takes_the_history_control_is_admitted_as_built() {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "existing document").unwrap();
    state.commit_action(None, None);
    let mut live = tilo_ui_nav::NavShell::new()
        .content(Box::new(Button::new("live", "Send")))
        .content_header("Page", Vec::new());
    let mut history = HistoryHost::new(registration(), &state.doc).unwrap();
    history.set_width(900.0);
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    history.build_composition(&mut live, &mut tree, &Theme::dark(), &mut nodes);
    assert!(
        !history.take_structure_dirty(),
        "the first build admits the bar it built"
    );
}

tilo_test_env::scenario! {
    fn every_input_enters_the_same_history_canvas_without_live_effects;

    phase managed_history_button_opens_real_activity_without_invoking_live_effects;
    {
        let effects = Rc::new(Cell::new(0));
        let probe = effects.clone();
        let root = Column::new().push(Box::new(
            Button::new("live-effect", "Send").on_click(move || probe.set(probe.get() + 1)),
        ));
        let mut state = State::new();
        state.doc.put(ROOT, "body", "first").unwrap();
        state.commit_action(None, None);
        state.doc.put(ROOT, "body", "second").unwrap();
        state.commit_action(None, None);
        let before = state.doc.get_heads();
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(root), registration()),
            (900, 650),
        )
        .unwrap();
        assert!(
            driver.targets().contains_key("history-open"),
            "managed roots need the actual shared History control"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
        assert!(
            driver.inspection_json().unwrap().contains("Send"),
            "wide Domain activity keeps the live surface beside the timeline"
        );
        assert_eq!(effects.get(), 0);
        assert_eq!(driver.state_mut().doc.get_heads(), before);
    }

    phase pointer_selection_draws_pinned_domain_canvas_and_back_returns_live;
    {
        let (mut driver, target, before) = versioned_driver((900, 650));
        driver.click_named("history-open").unwrap();
        let inspection = driver.inspection();
        let entry =
            find_access(&inspection.root, &target).expect("actual visible activity is accessible");
        let bounds = entry.bounds.as_ref().unwrap();
        driver.click_at(bounds.x0 + 100.0, (bounds.y0 + bounds.y1) / 2.0);
        assert!(driver.targets().contains_key("history-preview"));
        assert!(!driver.targets().contains_key("live-effect"));
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        let targets = driver.targets();
        assert!(
            targets["history-preview"].x1 <= targets["activity_timeline"].x0,
            "wide activity is a panel at the trailing side, the preview in place beside it"
        );
        if let Ok(directory) = std::env::var("TILO_HISTORY_SHOTS") {
            driver.render_png(&std::path::Path::new(&directory).join("wide.png"));
        }
        assert_eq!(driver.state_mut().doc.get_heads(), before);
        driver.state_mut().doc.put(ROOT, "body", "remote").unwrap();
        driver.state_mut().commit_action(None, None);
        driver.relayout();
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        driver.type_text("cannot-edit");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        assert_eq!(tilo_ui_app::testing::clicks("effect"), 0);
        driver.click_named("history-back-live").unwrap();
        assert!(driver.targets().contains_key("live-effect"));
        assert!(!driver.targets().contains_key("history-preview"));
    }

    phase accessible_activation_uses_the_same_canvas_and_narrow_activity_navigation;
    {
        let (mut driver, target, before) = versioned_driver((390, 650));
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &tilo_ui_base::a11y::Action::Activate,
            )
            .unwrap();
        assert!(driver.targets().contains_key("history-preview"));
        assert!(!driver.targets().contains_key("activity_timeline"));
        assert!(!driver.targets().contains_key("live-effect"));
        driver.click_named("history-before").unwrap();
        assert!(
            !driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        driver.click_named("history-after").unwrap();
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        assert!(
            driver.targets()["history-preview"].y0 <= 104.0,
            "narrow navigation leaves the canvas usable"
        );
        if let Ok(directory) = std::env::var("TILO_HISTORY_SHOTS") {
            driver.render_png(&std::path::Path::new(&directory).join("narrow.png"));
        }
        driver.click_named("history-activity").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
        assert!(!driver.targets().contains_key("history-preview"));
        assert_eq!(driver.state_mut().doc.get_heads(), before);
    }

    phase keyboard_enters_activity_and_selects_the_same_domain_canvas;
    {
        let (mut driver, target, _) = versioned_driver((900, 650));
        driver
            .perform_named("history-open", &tilo_ui_base::a11y::Action::Focus)
            .unwrap();
        driver
            .state_mut()
            .doc
            .put(ROOT, "pending-keyboard", "keep")
            .unwrap();
        let pending = driver.state_mut().doc.pending_ops();
        let heads = driver.state_mut().doc.base_heads();
        driver.key("Enter");
        assert!(driver.targets().contains_key("activity_timeline"));
        assert_eq!(driver.state_mut().doc.base_heads(), heads);
        assert_eq!(driver.state_mut().doc.pending_ops(), pending);
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &tilo_ui_base::a11y::Action::Focus,
            )
            .unwrap();
        driver.key("Enter");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first")
        );
        driver
            .perform_named("history-back-live", &tilo_ui_base::a11y::Action::Focus)
            .unwrap();
        driver.key("Enter");
        assert!(driver.targets().contains_key("live-effect"));
    }

    phase exported_accessibility_activation_preserves_pending_edits_through_back_live;
    {
        let (mut driver, target, _) = versioned_driver((900, 650));
        driver
            .state_mut()
            .doc
            .put(ROOT, "pending", "untouched")
            .unwrap();
        let heads = driver.state_mut().doc.base_heads();
        let pending = driver.state_mut().doc.pending_ops();
        let open = driver.a11y_node_named("History").unwrap();
        driver
            .perform_a11y_action(open, &tilo_ui_base::a11y::Action::Activate)
            .unwrap();
        assert_eq!(
            driver.state_mut().doc.base_heads(),
            heads,
            "AccessKit History open"
        );
        let inspection = driver.inspection();
        let name = find_access(&inspection.root, &target)
            .unwrap()
            .name
            .as_ref()
            .unwrap();
        let bounds = find_access(&inspection.root, &target)
            .unwrap()
            .bounds
            .as_ref()
            .unwrap();
        let row = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some(name) && node.bounds().is_some_and(|rect| rect.y0 == bounds.y0)
            })
            .unwrap()
            .0;
        driver
            .perform_a11y_action(row, &tilo_ui_base::a11y::Action::Activate)
            .unwrap();
        assert!(
            driver.targets().contains_key("history-preview"),
            "{}",
            driver.inspection_json().unwrap()
        );
        let back = driver.a11y_node_named("Back to live").unwrap();
        driver
            .perform_a11y_action(back, &tilo_ui_base::a11y::Action::Activate)
            .unwrap();
        assert_eq!(
            driver.state_mut().doc.base_heads(),
            heads,
            "AccessKit Back to live"
        );
        assert_eq!(driver.state_mut().doc.pending_ops(), pending);
        assert!(driver.targets().contains_key("live-effect"));
    }
}

fn versioned_driver(viewport: (u32, u32)) -> (Driver, String, Vec<automerge::ChangeHash>) {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let first = state.doc.get_heads().pop().unwrap();
    let id = tilo_ui_app::history_source::entry_id_from_hash(&first.to_string());
    state.doc.put(ROOT, "body", "second").unwrap();
    state.commit_action(None, None);
    let before = state.doc.get_heads();
    let root = Column::new().push(Box::new(
        Button::new("live-effect", "Send").on_click(tilo_ui_app::testing::record_click("effect")),
    ));
    let driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(root), registration()),
        viewport,
    )
    .unwrap();
    (
        driver,
        format!("timeline-entry:{}", tilo_ui_base::HistoryEntryId::hex(&id)),
        before,
    )
}

fn assert_history_frame_idle(driver: &mut Driver, context: &str) {
    assert!(
        !driver.frame_output().needs_redraw,
        "{context}: visible History branch stayed dirty after draw: {:?}",
        driver.frame_requesters()
    );
    assert!(
        !driver.poll_frame().unwrap(),
        "{context}: idle History branch produced another frame"
    );
}

#[test]
fn managed_history_visible_branches_drain_and_retained_modes_return_current() {
    // Each branch is read at rest: History's panel slides in as it opens,
    // and a settled clock shows where it lands.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut wide, target, _) = versioned_driver((900, 650));
    assert_history_frame_idle(&mut wide, "toolbar");

    wide.click_named("history-open").unwrap();
    assert!(wide.targets().contains_key("activity_timeline"));
    // Drawn, History's chrome owes no frame: the frame that opened it listed
    // and laid its changes out already.
    assert!(
        !wide.poll_frame().unwrap(),
        "the opening frame leaves History at rest"
    );
    assert_history_frame_idle(&mut wide, "available timeline");

    wide.perform_semantic_named(
        "activity_timeline",
        Some(&target),
        &tilo_ui_base::a11y::Action::Activate,
    )
    .unwrap();
    assert!(wide.inspection_json().unwrap().contains("Notebook: first"));
    assert_history_frame_idle(&mut wide, "after preview");

    wide.click_named("history-before").unwrap();
    assert!(!wide.inspection_json().unwrap().contains("Notebook: first"));
    assert_history_frame_idle(&mut wide, "before preview");
    wide.click_named("history-after").unwrap();
    assert!(wide.inspection_json().unwrap().contains("Notebook: first"));
    assert_history_frame_idle(&mut wide, "retained after preview");

    let (mut narrow, target, _) = versioned_driver((390, 650));
    narrow.click_named("history-open").unwrap();
    narrow
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    assert_history_frame_idle(&mut narrow, "narrow preview");
    narrow.click_named("history-activity").unwrap();
    assert!(narrow.targets().contains_key("activity_timeline"));
    assert!(!narrow.targets().contains_key("history-preview"));
    assert_history_frame_idle(&mut narrow, "narrow activity");

    static UNVERSIONED: HistoryDefinition = HistoryDefinition {
        app_id: "so.tilo.history-unavailable-fixture",
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Unavailable notebook",
            presentation: HistoryPresentation::External {
                reason: "Fixture history unavailable",
            },
        }],
    };
    let registration = HistoryRegistration::new(
        &UNVERSIONED,
        vec![HistorySourceRegistration::Unversioned {
            scope: "document",
            reason: "Fixture history unavailable",
        }],
    )
    .unwrap();
    let mut unavailable = Driver::from_managed_root(
        State::new(),
        ManagedRoot::new(Box::new(Column::new()), registration),
        (900, 650),
    )
    .unwrap();
    unavailable.click_named("history-open").unwrap();
    assert!(
        unavailable
            .inspection_json()
            .unwrap()
            .contains("Fixture history unavailable")
    );
    assert_history_frame_idle(&mut unavailable, "unavailable timeline");
}
fn find_access<'a>(
    node: &'a tilo_ui_app::headless::InspectionNode,
    target: &str,
) -> Option<&'a tilo_ui_base::a11y::AccessNode> {
    fn in_access<'a>(
        node: &'a tilo_ui_base::a11y::AccessNode,
        target: &str,
    ) -> Option<&'a tilo_ui_base::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| in_access(child, target))
    }
    node.semantics
        .as_ref()
        .and_then(|access| in_access(access, target))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| find_access(child, target))
        })
}

static FOLDER_COMPOSITION: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.folder-composition-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "folder",
        title: "Folder",
        presentation: HistoryPresentation::Folder,
    }],
};

struct FolderCompositionRoot {
    inner: Box<dyn Widget>,
    source: tilo_ui_app::history_mode::folder::FolderHistorySource,
}

impl Widget for FolderCompositionRoot {
    tilo_ui_base::provides!(dyn tilo_ui_app::history_mode::capability::DocumentHistory);

    tilo_ui_app::delegate_widget_methods!(
        inner: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );

    fn children(&self) -> Vec<&dyn Widget> {
        self.inner.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.inner.children_mut()
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(self.inner.as_ref());
        true
    }

    fn name(&self) -> &str {
        "folder-composition-root"
    }
}

impl tilo_ui_app::history_mode::capability::DocumentHistory for FolderCompositionRoot {
    fn history_registration(
        &self,
    ) -> Result<Option<HistoryRegistration>, HistoryRegistrationError> {
        HistoryRegistration::new(
            &FOLDER_COMPOSITION,
            vec![HistorySourceRegistration::FolderFiles {
                scope: "folder",
                source: self.source.clone(),
                window: None,
            }],
        )
        .map(Some)
    }
    fn document_history_definition(&self) -> Option<&'static HistoryDefinition> {
        Some(&FOLDER_COMPOSITION)
    }
}

#[test]
fn folder_activity_keeps_the_wide_live_surface() {
    let source = tilo_ui_app::history_mode::folder::FolderHistorySource::default();
    let registration = HistoryRegistration::new(
        &FOLDER_COMPOSITION,
        vec![HistorySourceRegistration::FolderFiles {
            scope: "folder",
            source: source.clone(),
            window: None,
        }],
    )
    .unwrap();
    let live = FolderCompositionRoot {
        inner: Box::new(Column::new().push(Box::new(Button::new(
            "folder-live-marker",
            "Retained folder",
        )))),
        source,
    };
    let mut driver = Driver::from_managed_root(
        State::new(),
        ManagedRoot::new(Box::new(live), registration),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Retained folder"),
        "a wide folder History keeps the live surface beside its controls"
    );
}

const TOOLKIT_EN: &str = include_str!("../../ui-base/locales/en-US.ftl");
const TOOLKIT_DE: &str = include_str!("../../ui-base/locales/de-DE.ftl");

#[test]
fn history_chrome_follows_the_shell_language() {
    tilo_ui_base::i18n::I18nManager::install(
        "en-US",
        "de-DE",
        &[("en-US", TOOLKIT_EN), ("de-DE", TOOLKIT_DE)],
    )
    .unwrap();
    let (mut driver, _, _) = versioned_driver((900, 650));
    let open = driver.a11y_node_named("Verlauf").unwrap();
    driver
        .perform_a11y_action(open, &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    let row = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == accesskit::Role::ListItem)
        .unwrap()
        .0;
    driver
        .perform_a11y_action(row, &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert!(driver.targets().contains_key("history-preview"));
    for german in [
        "Vorher",
        "Nachher",
        "Frühere Version, schreibgeschützt",
        "Wiederherstellen…",
    ] {
        assert!(
            driver.a11y_node_named(german).is_some(),
            "{german}: {}",
            driver.inspection_json().unwrap()
        );
    }
    for english in ["Back to live", "Before", "After", "Restore…"] {
        assert!(driver.a11y_node_named(english).is_none(), "{english}");
    }
    let back = driver
        .a11y_node_named("Zurück zur aktuellen Version")
        .unwrap();
    driver
        .perform_a11y_action(back, &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert!(driver.targets().contains_key("live-effect"));
}

/// Every History host message is translated and formats with the variables the host passes, so no
/// German shell falls back to English copy.
#[test]
fn history_host_messages_exist_in_every_shipped_language() {
    let keys: Vec<&str> = TOOLKIT_EN
        .lines()
        .filter_map(|line| line.split_once(" = ").map(|(key, _)| key))
        .filter(|key| {
            [
                "history-mode-",
                "history-folder-",
                "history-media-",
                "history-sync-",
            ]
            .iter()
            .any(|prefix| key.starts_with(prefix))
        })
        .collect();
    assert!(keys.len() > 40, "{keys:?}");
    let args: &[(&str, tilo_ui_base::i18n::ArgValue)] = &[
        ("id", 7.into()),
        ("owner", "owner".into()),
        ("action", "app/action".into()),
        ("arguments", "{}".into()),
        ("phrase", "nvme0n1".into()),
        ("file", "brief.md".into()),
        ("revision", "r1".into()),
        ("identity", "abcdef".into()),
        ("count", 2.into()),
        ("intent", "i1".into()),
        ("mapping", "m".into()),
        ("reason", "refused".into()),
    ];
    for (locale, catalog) in [("en-US", TOOLKIT_EN), ("de-DE", TOOLKIT_DE)] {
        tilo_ui_base::i18n::I18nManager::install(locale, locale, &[(locale, catalog)]).unwrap();
        for key in &keys {
            assert!(
                tilo_ui_base::i18n::I18nManager::has_message(key),
                "{locale} lacks {key}"
            );
            let value = tilo_ui_base::i18n::I18nManager::format(key, args);
            assert!(
                !value.contains('{'),
                "{locale} {key} left a placeable unresolved: {value}"
            );
        }
    }
}

#[test]
fn closed_history_strip_insets_its_button_by_the_theme_spacing() {
    let (driver, _, _) = versioned_driver((900, 650));
    let spacing = driver.theme().tokens.spacing;
    let button = driver.targets()["history-open"];
    assert_eq!(
        button.x1,
        900.0 - f64::from(spacing.s3),
        "sibling step from the trailing edge"
    );
    assert_eq!(
        button.y0,
        f64::from(spacing.s1),
        "one control tall, not a band"
    );
}

/// A `NavShell` app carries the window's History control in its header row; under the app's registered
/// History that control opens and closes that History, never the standalone chrome the app does not
/// have.
#[test]
fn header_history_control_opens_the_registered_history() {
    fn history_toggled(driver: &Driver) -> Vec<Option<accesskit::Toggled>> {
        driver
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| {
                node.label() == Some("History") && node.role() == accesskit::Role::Button
            })
            .map(|(_, node)| node.toggled())
            .collect()
    }
    let mut state = State::new();
    for body in ["first", "second"] {
        state.doc.put(ROOT, "body", body).unwrap();
        state.commit_action(None, None);
    }
    let before = state.doc.get_heads();
    let shell = tilo_ui_nav::NavShell::new()
        .content_header("Notes", vec![])
        .content(Box::new(tilo_ui_app::widget::Text::new("Body")));
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(shell), registration()),
        (1000, 700),
    )
    .unwrap();
    assert!(
        !driver.targets().contains_key("history-open"),
        "the header row presents History; the window draws no bar for it"
    );
    assert_eq!(history_toggled(&driver), [Some(accesskit::Toggled::False)]);

    driver.click_named("nav-shell-history").unwrap();
    assert!(
        driver.targets().contains_key("activity_timeline"),
        "the header control opens the registered History"
    );
    assert!(
        !driver.targets().contains_key("nav-shell-history"),
        "while open the inert app's header gives the control to History's bar"
    );
    assert_eq!(
        history_toggled(&driver),
        [Some(accesskit::Toggled::True)],
        "and the one History control reads as on"
    );

    driver.click_named("history-open").unwrap();
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert!(driver.targets().contains_key("nav-shell-history"));
    assert_eq!(history_toggled(&driver), [Some(accesskit::Toggled::False)]);

    driver.click_named("nav-shell-history").unwrap();
    assert!(driver.targets().contains_key("activity_timeline"));
    driver
        .invoke_local_history_action(
            &tilo_actions::ActionAddress::new(tilo_ui_app::history_api::HISTORY_RESOURCE, "hide"),
            None,
        )
        .unwrap();
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(driver.state_mut().doc.get_heads(), before);
}

/// Opening History narrows the window's workspace for the app: its `NavShell` lays its header rows out
/// again at the width it is given, so header actions keep their own places and never fold onto one
/// another.
#[test]
fn opening_history_lays_the_shell_out_at_its_narrower_width() {
    use tilo_ui_base::icons::Icon;
    use tilo_ui_base::widget::Decorator;
    struct Probe(Button, Rc<Cell<Rect>>);
    impl Decorator for Probe {
        type Inner = Button;
        fn inner(&self) -> &Button {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Button {
            &mut self.0
        }
        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.0.apply_layout(taffy, node, origin);
            self.1.set(self.0.get_rect());
        }
    }
    let mut state = State::new();
    for body in ["first", "second"] {
        state.doc.put(ROOT, "body", body).unwrap();
        state.commit_action(None, None);
    }
    let ids = [
        "overview-search",
        "overview-new",
        "content-star",
        "content-more",
    ];
    let seen: Vec<Rc<Cell<Rect>>> = ids.iter().map(|_| Rc::default()).collect();
    let mut probes = ids.iter().zip(&seen).map(|(id, seen)| -> Box<dyn Widget> {
        Box::new(Probe(
            Button::icon_only(id, Icon::Plus, id),
            Rc::clone(seen),
        ))
    });
    let overview = vec![probes.next().unwrap(), probes.next().unwrap()];
    let content = vec![probes.next().unwrap(), probes.next().unwrap()];
    let shell = tilo_ui_nav::NavShell::new()
        .sidebar(Box::new(tilo_ui_app::widget::Text::new("Library")))
        .overview(Box::new(tilo_ui_app::widget::Text::new("List")))
        .overview_header("All snippets", overview)
        .content_header("Multi-stage build", content)
        .content(Box::new(tilo_ui_app::widget::Text::new("Body")));
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(shell), registration()),
        (1280, 800),
    )
    .unwrap();
    let apart = |phase: &str| {
        for pair in [[0, 1], [2, 3]] {
            let [a, b] = pair.map(|index| seen[index].get());
            assert!(
                a.width() > 0.0 && b.width() > 0.0 && a.intersect(b).area() <= 0.0,
                "{phase}: {a:?} and {b:?} keep their own places"
            );
        }
    };
    apart("closed");

    driver.click_named("nav-shell-history").unwrap();
    let timeline = driver.targets()["activity_timeline"];
    apart("open");
    for (id, seen) in ids.iter().zip(&seen) {
        let rect = seen.get();
        assert!(
            rect.x0 >= timeline.x1 || rect.x1 <= timeline.x0,
            "{id} stands beside the timeline, not under it: {rect:?} vs {timeline:?}"
        );
        assert!(rect.x1 <= 1280.0, "{id} stays in the window: {rect:?}");
    }
}

fn root_text(doc: &automerge::AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
}

#[test]
fn history_is_a_side_panel_whose_change_previews_in_place_and_restores_on_its_own() {
    let mut state = State::new();
    state.doc.put(ROOT, "title", "Draft").unwrap();
    state.commit_action(None, None);
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let body_change = state.doc.get_heads().pop().unwrap();
    state.doc.put(ROOT, "status", "done").unwrap();
    state.commit_action(None, None);
    let root = Column::new().push(Box::new(Button::new("live-effect", "Send")));
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(root), registration()),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    let targets = driver.targets();
    let timeline = targets["activity_timeline"];
    assert!(
        timeline.x0 >= 900.0 - SIDE_PANEL_WIDTH - 1.0,
        "History stands at the window's trailing side: {timeline:?}"
    );
    assert!(
        (timeline.x1 - 900.0).abs() < 24.0,
        "the panel reaches the window's trailing edge: {timeline:?}"
    );

    let target = format!(
        "timeline-entry:{}",
        tilo_ui_base::HistoryEntryId::hex(&tilo_ui_app::history_source::entry_id_from_hash(
            &body_change.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    let targets = driver.targets();
    assert!(
        targets["history-preview"].x1 <= targets["activity_timeline"].x0,
        "the selected version shows in place of the content, beside the panel"
    );
    assert!(targets.contains_key("history-restore"), "Restore");
    assert!(
        targets.contains_key("history-back-now"),
        "the way back to now"
    );
    driver
        .click_named("history-restore-change")
        .expect("a document's change restores on its own");
    let doc = &driver.state_mut().doc;
    assert_eq!(
        root_text(doc, "body"),
        None,
        "just this change is taken back"
    );
    assert_eq!(root_text(doc, "title").as_deref(), Some("Draft"));
    assert_eq!(
        root_text(doc, "status").as_deref(),
        Some("done"),
        "what came after it stays"
    );
}

#[test]
fn the_side_panel_names_the_selected_changes_and_bands_them_under_the_name() {
    let (mut driver, target, _) = versioned_driver((900, 650));
    driver.click_named("history-open").unwrap();
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    let changes = driver.state_mut().doc.get_changes(&[]).len();
    driver.click_named("history-name").unwrap();
    driver.click_named("history-name-field").unwrap();
    driver.commit_text("First pass");
    driver.key("Enter");
    let names = |driver: &mut Driver| {
        tilo_ui_app::history_source::groups::read_named_ranges(
            &driver.state_mut().doc,
            &tilo_ui_app::history_source::AuthorDirectory::default(),
        )
    };
    let [named] = names(&mut driver).try_into().unwrap();
    assert_eq!(named.label, "First pass");
    assert_eq!(
        format!(
            "timeline-entry:{}",
            tilo_ui_base::HistoryEntryId::hex(&named.start)
        ),
        target
    );
    assert_eq!(named.end, Some(named.start), "one change is a run of one");
    assert_eq!(
        driver.state_mut().doc.get_changes(&[]).len(),
        changes + 1,
        "naming is one change"
    );
    assert_eq!(
        root_text(&driver.state_mut().doc, "body").as_deref(),
        Some("second"),
        "naming moves no content"
    );
    driver.relayout();
    fn band(node: &tilo_ui_base::a11y::AccessNode, name: &str) -> bool {
        (node.role == tilo_ui_base::a11y::Role::TreeItem && node.name.as_deref() == Some(name))
            || node.children.iter().any(|child| band(child, name))
    }
    fn banded(node: &tilo_ui_app::headless::InspectionNode, name: &str) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|access| band(access, name))
            || node.children.iter().any(|child| banded(child, name))
    }
    assert!(
        banded(&driver.inspection().root, "First pass"),
        "the timeline bands the change under its name"
    );
}
