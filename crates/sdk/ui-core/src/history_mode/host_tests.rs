use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::{
    State,
    headless::FrameHarness,
    widget::{Button, Column, Widget},
};
use crate::{text_render::TextRenderer, theme::Theme};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::{cell::Cell, rc::Rc};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};

static DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.history-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: None,
    }],
};
struct NotebookPreview {
    body: String,
    bounds: Rect,
    view: PreviewView,
}
pub(crate) fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = automerge::AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let body = doc
        .get(ROOT, "body")
        .map_err(|e| e.to_string())?
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(Box::new(NotebookPreview {
        body,
        bounds: Rect::ZERO,
        view: input.initial_view,
    }))
}
impl HistoryPreview for NotebookPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.bounds,
        );
        text.draw_text(
            scene,
            "Notebook",
            24.0,
            theme.text_primary,
            transform * Affine::translate((self.bounds.x0 + 24.0, self.bounds.y0 + 24.0)),
            None,
        );
        text.draw_text(
            scene,
            &self.body,
            18.0,
            theme.text_primary,
            transform
                * Affine::translate((
                    self.bounds.x0 + 24.0,
                    self.bounds.y0 + 70.0 + self.view.scroll.1,
                )),
            None,
        );
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Scroll { dx, dy } = command {
            self.view.scroll.0 += dx;
            self.view.scroll.1 += dy;
            true
        } else {
            false
        }
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("Notebook: {}; scroll {}", self.body, self.view.scroll.1),
            pages: vec![],
        }
    }
}
pub(super) fn registration() -> HistoryRegistration {
    HistoryRegistration::new(
        &DEFINITION,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "lifetime-1",
                DEFINITION.app_id,
                crate::RecordId::for_tab(777),
            ),
        }],
    )
    .unwrap()
}

#[test]
fn history_status_and_recovery_keep_the_existing_bar_geometry() {
    for width in [360.0, 900.0] {
        let state = State::new();
        let mut live = Column::new().push(Box::new(Button::new("live", "Send")));
        let mut history = host::HistoryHost::new(registration(), &state.doc).unwrap();
        history.set_width(width);
        let theme = Theme::dark();
        let mut renderer = crate::font_catalog::fixture_renderer();
        let mut baseline = None;
        for key in [
            None,
            Some("history-local-action-unsigned"),
            Some("history-local-action-pending"),
            None,
        ] {
            let status = key.map(crate::i18n::I18nManager::lookup_plain);
            history.set_sync_status(status.clone());
            let mut tree = taffy::TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let root = history.build_composition(&mut live, &mut tree, &theme, &mut nodes);
            tree.compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(width)),
                    height: taffy::AvailableSpace::Definite(650.0),
                },
            )
            .unwrap();
            history.apply_composition(&mut live, &tree, root, vello::kurbo::Point::ZERO);
            history.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &crate::presence::PresenceCtx::default(),
            );
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
                .any(|node| node.name.as_deref() == status.as_deref() && status.is_some());
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
    let mut history = host::HistoryHost::new(registration(), &state.doc).unwrap();
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

    // An external open can run during update, after the runner's capture pass.
    // Its first control capture then occurs with the prior frame still admitted.
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

/// Runners keep raw pointers into the admitted composition between frames.
/// Replacing the toolbar, or swapping which children are presented, must
/// report structure dirty until the next build admits the new widgets.
#[test]
fn replacing_admitted_history_controls_reports_structure_dirty() {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "existing document").unwrap();
    state.commit_action(None, None);
    let mut live = Column::new().push(Box::new(Button::new("live", "Send")));
    let mut history = host::HistoryHost::new(registration(), &state.doc).unwrap();
    history.set_width(900.0);
    let build = |history: &mut host::HistoryHost, live: &mut Column| {
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        history.build_composition(live, &mut tree, &Theme::dark(), &mut nodes);
    };
    build(&mut history, &mut live);
    assert!(
        !history.take_structure_dirty(),
        "an admitted composition is clean"
    );

    history.set_sync_status(Some("Syncing".into()));
    assert!(
        history.take_structure_dirty(),
        "the admitted toolbar was replaced"
    );
    assert!(
        history.take_structure_dirty(),
        "stale maps stay reported until a build admits the new toolbar"
    );
    build(&mut history, &mut live);
    assert!(!history.take_structure_dirty());

    assert!(history.request_open());
    history.refresh(&state.doc);
    assert!(
        history.take_structure_dirty(),
        "opening presents other children"
    );
    build(&mut history, &mut live);
    assert!(!history.take_structure_dirty());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn every_input_enters_the_same_history_canvas_without_live_effects() {
    // phase: managed_history_button_opens_real_activity_without_invoking_live_effects
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
        let mut driver = FrameHarness::from_managed_root(
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
    // phase: pointer_selection_draws_pinned_domain_canvas_and_back_returns_live
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
        // USER REQUIREMENT (2026-10-19): History is "a right-side panel per
        // window or active tab"; selecting an entry "previews that state in
        // place".
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
        assert_eq!(crate::testing::clicks("effect"), 0);
        driver.click_named("history-back-live").unwrap();
        assert!(driver.targets().contains_key("live-effect"));
        assert!(!driver.targets().contains_key("history-preview"));
    }
    // phase: accessible_activation_uses_the_same_canvas_and_narrow_activity_navigation
    {
        let (mut driver, target, before) = versioned_driver((390, 650));
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
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
    // phase: keyboard_enters_activity_and_selects_the_same_domain_canvas
    {
        let (mut driver, target, _) = versioned_driver((900, 650));
        driver
            .perform_named("history-open", &crate::a11y::Action::Focus)
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
                &crate::a11y::Action::Focus,
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
            .perform_named("history-back-live", &crate::a11y::Action::Focus)
            .unwrap();
        driver.key("Enter");
        assert!(driver.targets().contains_key("live-effect"));
    }
    // phase: exported_accessibility_activation_preserves_pending_edits_through_back_live
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
            .perform_a11y_action(open, &crate::a11y::Action::Activate)
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
            .perform_a11y_action(row, &crate::a11y::Action::Activate)
            .unwrap();
        assert!(
            driver.targets().contains_key("history-preview"),
            "{}",
            driver.inspection_json().unwrap()
        );
        let back = driver.a11y_node_named("Back to live").unwrap();
        driver
            .perform_a11y_action(back, &crate::a11y::Action::Activate)
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
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn managed_registration_admits_only_real_owners_and_scopes() {
    // phase: managed_registration_rejects_missing_source_and_foreign_owner
    {
        assert!(HistoryRegistration::new(&DEFINITION, vec![]).is_err());
        assert!(
            HistoryRegistration::new(
                &DEFINITION,
                vec![HistorySourceRegistration::HostDocument {
                    scope: "document",
                    resource: HistoryResource::new(
                        "fixture",
                        "other",
                        "different-app",
                        crate::RecordId::for_tab(777)
                    ),
                }]
            )
            .is_err()
        );
    }
    // phase: managed_registration_supports_multiple_scopes_and_refuses_empty_namespace
    {
        static MULTI: HistoryDefinition = HistoryDefinition {
            app_id: "dev.tilo.history-fixture",
            scopes: &[
                HistoryScopeDefinition {
                    key: "a",
                    title: "A",
                    presentation: HistoryPresentation::Domain { create: preview },
                    content_schema: None,
                },
                HistoryScopeDefinition {
                    key: "b",
                    title: "B",
                    presentation: HistoryPresentation::Domain { create: preview },
                    content_schema: None,
                },
            ],
        };
        let sources = ["a", "b"]
            .into_iter()
            .map(|scope| HistorySourceRegistration::HostDocument {
                scope,
                resource: HistoryResource::new(
                    "fixture",
                    "lifetime-1",
                    MULTI.app_id,
                    crate::RecordId::for_tab(777),
                ),
            })
            .collect();
        let registration = HistoryRegistration::new(&MULTI, sources).unwrap();
        let mut driver = FrameHarness::from_managed_root(
            State::new(),
            ManagedRoot::new(Box::new(Column::new()), registration),
            (900, 650),
        )
        .expect("registered scopes must compose in the managed host");
        driver.click_named("history-scope-b").unwrap();
        assert!(driver.targets().contains_key("history-scope-a"));
        assert!(
            HistoryRegistration::new(
                &DEFINITION,
                vec![HistorySourceRegistration::HostDocument {
                    scope: "document",
                    resource: HistoryResource::new(
                        "",
                        "",
                        DEFINITION.app_id,
                        crate::RecordId::for_tab(777)
                    )
                }]
            )
            .is_err()
        );
    }
    // phase: outer_state_registration_rejects_legacy_scratch_owner_at_root_or_descendant
    {
        use crate::widget::{AppKind, TileContent, TileHost};
        for nested in [false, true] {
            let mut tile = TileHost::new(TileContent::Stub {
                kind: AppKind::Terminal,
                label: "terminal".into(),
            });
            tile.bind_or_create_record(
                crate::record::RecordStore::in_memory(),
                crate::record::RecordCheckout::new(
                    crate::RecordId::for_tab(999),
                    crate::record::RecordKind::new("scratch"),
                ),
            );
            let root: Box<dyn crate::widget::Widget> = if nested {
                Box::new(Column::new().push(Box::new(tile)))
            } else {
                Box::new(tile)
            };
            assert!(
                FrameHarness::from_managed_root(
                    State::new(),
                    ManagedRoot::new(root, registration()),
                    (900, 650)
                )
                .is_err()
            );
        }
    }
    // phase: managed_history_rejects_owner_mounted_during_initial_settlement
    {
        for pending in [false, true] {
            for layout in [false, true] {
                let result = FrameHarness::from_managed_root(
                    State::new(),
                    ManagedRoot::new(
                        lazy_owner_root(Rc::new(Cell::new(true)), pending, layout),
                        registration(),
                    ),
                    (900, 650),
                );
                assert!(
                    matches!(result, Err(HistoryRegistrationError::UnsupportedOwner)),
                    "pending={pending}, layout={layout}"
                );
            }
        }
    }
    // phase: managed_history_invalidates_after_later_owner_mount_without_outer_state_fallback
    {
        for pending in [false, true] {
            for layout in [false, true] {
                let mount = Rc::new(Cell::new(false));
                let mut state = State::new();
                state.doc.put(ROOT, "body", "outer document").unwrap();
                state.commit_action(None, None);
                let mut driver = FrameHarness::from_managed_root(
                    state,
                    ManagedRoot::new(
                        lazy_owner_root(mount.clone(), pending, layout),
                        registration(),
                    ),
                    (900, 650),
                )
                .unwrap();
                driver.click_named("history-open").unwrap();
                assert!(driver.targets().contains_key("activity_timeline"));
                let stale_row = driver
                    .a11y_nodes()
                    .into_iter()
                    .find(|(_, node)| node.role() == accesskit::Role::ListItem)
                    .unwrap()
                    .0;
                if !layout {
                    driver
                        .perform_a11y_action(stale_row, &crate::a11y::Action::Activate)
                        .unwrap();
                    assert!(driver.targets().contains_key("history-preview"));
                }
                driver
                    .state_mut()
                    .doc
                    .put(ROOT, "unfinished", "pending live work")
                    .unwrap();
                let heads = driver.state_mut().doc.base_heads();
                let pending_ops = driver.state_mut().doc.pending_ops();
                mount.set(true);
                driver.relayout();
                assert_eq!(
                    driver.validate_history_registration(),
                    Err(HistoryRegistrationError::UnsupportedOwner),
                    "pending={pending}, layout={layout}"
                );
                assert!(!driver.targets().contains_key("activity_timeline"));
                assert!(!driver.targets().contains_key("history-preview"));
                assert!(!driver.targets().contains_key("history-open"));
                assert!(
                    driver
                        .inspection_json()
                        .unwrap()
                        .contains("requires the component History runner")
                );
                assert!(
                    driver
                        .perform_a11y_action(stale_row, &crate::a11y::Action::Activate)
                        .is_err(),
                    "removed History AT target must be refused"
                );
                assert_eq!(driver.state_mut().doc.base_heads(), heads);
                assert_eq!(driver.state_mut().doc.pending_ops(), pending_ops);
                driver.relayout();
                assert!(!driver.targets().contains_key("activity_timeline"));
                assert_eq!(driver.state_mut().doc.base_heads(), heads);
                assert_eq!(driver.state_mut().doc.pending_ops(), pending_ops);
            }
        }
    }
    // phase: strict_existing_admission_never_reserves_missing_memory_or_persisted_records
    {
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(918),
            crate::record::RecordKind::new("note"),
        );
        let memory = crate::record::RecordStore::in_memory();
        assert!(matches!(
            memory.open_existing_record_checked(&checkout),
            Err(crate::record::RecordOpenError::MissingRecord)
        ));
        assert!(memory.open_records().is_empty());
        memory.open_record(&checkout);
        assert!(memory.open_existing_record_checked(&checkout).is_ok());
        let wrong = crate::record::RecordCheckout::new(
            checkout.id.clone(),
            crate::record::RecordKind::new("wrong"),
        );
        assert!(matches!(
            memory.open_existing_record_checked(&wrong),
            Err(crate::record::RecordOpenError::IdentityMismatch)
        ));
        let directory = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(directory.path().to_path_buf());
        let persisted = owners
            .open("dev.tilo.history-fixture", "workspace")
            .unwrap();
        assert!(matches!(
            persisted.open_existing_record_checked(&checkout),
            Err(crate::record::RecordOpenError::MissingRecord)
        ));
        assert!(persisted.open_records().is_empty());
        persisted
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        assert!(persisted.open_existing_record_checked(&checkout).is_ok());
        assert!(matches!(
            persisted.open_existing_record_checked(&wrong),
            Err(crate::record::RecordOpenError::IdentityMismatch)
        ));
    }
}

fn versioned_driver(viewport: (u32, u32)) -> (FrameHarness, String, Vec<automerge::ChangeHash>) {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let first = state.doc.get_heads().pop().unwrap();
    let id = crate::history_source::entry_id_from_hash(&first.to_string());
    state.doc.put(ROOT, "body", "second").unwrap();
    state.commit_action(None, None);
    let before = state.doc.get_heads();
    let root = Column::new().push(Box::new(
        Button::new("live-effect", "Send").on_click(crate::testing::record_click("effect")),
    ));
    let driver = FrameHarness::from_managed_root(
        state,
        ManagedRoot::new(Box::new(root), registration()),
        viewport,
    )
    .unwrap();
    (
        driver,
        format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&id)
        ),
        before,
    )
}

fn assert_history_frame_idle(driver: &mut FrameHarness, context: &str) {
    assert!(
        !driver.frame_output().needs_redraw,
        "{context}: visible History branch stayed dirty after draw"
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
    let _settled = crate::motion::testing::settled();
    let (mut wide, target, _) = versioned_driver((900, 650));
    assert_history_frame_idle(&mut wide, "toolbar");

    wide.click_named("history-open").unwrap();
    assert!(wide.targets().contains_key("activity_timeline"));
    assert_history_frame_idle(&mut wide, "available timeline");

    wide.perform_semantic_named(
        "activity_timeline",
        Some(&target),
        &crate::a11y::Action::Activate,
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
            &crate::a11y::Action::Activate,
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
            content_schema: None,
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
    let mut unavailable = FrameHarness::from_managed_root(
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
    node: &'a crate::headless::InspectionNode,
    target: &str,
) -> Option<&'a crate::a11y::AccessNode> {
    fn in_access<'a>(
        node: &'a crate::a11y::AccessNode,
        target: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn history_preserves_pending_work_and_restores_exactly_one_focus() {
    // phase: history_from_unfinished_editor_preserves_pending_work_and_restores_stable_focus
    {
        use crate::widget::TextInput;
        use winit::event::{Ime, WindowEvent};
        for route in ["pointer", "keyboard", "accesskit"] {
            let mut state = State::new();
            state.doc.put(ROOT, "body", "first").unwrap();
            state.commit_action(None, None);
            let version = state.doc.get_heads().pop().unwrap();
            let id = crate::history_source::entry_id_from_hash(&version.to_string());
            let target = format!(
                "timeline-entry:{}",
                crate::widget::history_group::hex16(&id)
            );
            let root = Column::new().push(Box::new(TextInput::local().with_id("draft-editor")));
            let mut driver = FrameHarness::from_managed_root(
                state,
                ManagedRoot::new(Box::new(root), registration()),
                (900, 650),
            )
            .unwrap();
            driver.click_named("draft-editor").unwrap();
            driver.type_text("unfinished");
            driver
                .perform_named(
                    "draft-editor",
                    &crate::a11y::Action::SetSelection {
                        anchor: 2,
                        focus: 7,
                    },
                )
                .unwrap();
            driver.send(&WindowEvent::Ime(Ime::Preedit(
                "composition".into(),
                Some((0, 11)),
            )));
            let editor_before = driver.focused_access_node().unwrap();
            // An in-progress editor transaction may outlive the last input dispatch.
            // Keep that real owner transaction pending while opening framework chrome.
            driver
                .state_mut()
                .doc
                .put(ROOT, "in-progress", "unfinished")
                .unwrap();
            let before_heads = driver.state_mut().doc.base_heads();
            let before_pending = driver.state_mut().doc.pending_ops();
            let before_bytes = driver.state_mut().doc.clone().save();
            assert!(
                before_pending > 0,
                "fixture has actual unfinished live writes"
            );
            match route {
                "pointer" => driver.click_named("history-open").unwrap(),
                "keyboard" => {
                    driver.tab(false);
                    driver.key("Enter");
                }
                _ => {
                    let node = driver.a11y_node_named("History").unwrap();
                    driver
                        .perform_a11y_action(node, &crate::a11y::Action::Activate)
                        .unwrap();
                }
            }
            assert_eq!(
                driver.state_mut().doc.base_heads(),
                before_heads,
                "entry via {route}"
            );
            if route == "keyboard" {
                driver
                    .perform_semantic_named(
                        "activity_timeline",
                        Some(&target),
                        &crate::a11y::Action::Focus,
                    )
                    .unwrap();
                driver.key("Enter");
            } else if route == "pointer" {
                let inspection = driver.inspection();
                let bounds = find_access(&inspection.root, &target)
                    .unwrap()
                    .bounds
                    .as_ref()
                    .unwrap();
                driver.click_at(bounds.x0 + 100.0, (bounds.y0 + bounds.y1) / 2.0);
            } else {
                let inspection = driver.inspection();
                let bounds = find_access(&inspection.root, &target)
                    .unwrap()
                    .bounds
                    .as_ref()
                    .unwrap();
                let node = driver
                    .a11y_nodes()
                    .into_iter()
                    .find(|(_, node)| {
                        node.role() == accesskit::Role::ListItem
                            && node.bounds().is_some_and(|rect| rect.y0 == bounds.y0)
                    })
                    .unwrap()
                    .0;
                driver
                    .perform_a11y_action(node, &crate::a11y::Action::Activate)
                    .unwrap();
            }
            assert_eq!(
                driver.state_mut().doc.base_heads(),
                before_heads,
                "selection"
            );
            assert_eq!(driver.state_mut().doc.pending_ops(), before_pending);
            assert_eq!(driver.state_mut().doc.clone().save(), before_bytes);
            match route {
                "pointer" => driver.click_named("history-back-live").unwrap(),
                "keyboard" => {
                    driver
                        .perform_named("history-back-live", &crate::a11y::Action::Focus)
                        .unwrap();
                    driver.key("Enter");
                }
                _ => {
                    let node = driver.a11y_node_named("Back to live").unwrap();
                    driver
                        .perform_a11y_action(node, &crate::a11y::Action::Activate)
                        .unwrap();
                }
            }
            let editor_after = driver.focused_access_node().unwrap();
            assert_eq!(editor_after.semantic_key.as_deref(), Some("draft-editor"));
            assert_eq!(editor_after.text_selection, editor_before.text_selection);
            assert_eq!(editor_after.value, editor_before.value);
            assert_eq!(
                driver.state_mut().doc.base_heads(),
                before_heads,
                "return via {route}"
            );
            assert_eq!(driver.state_mut().doc.pending_ops(), before_pending);
            driver.key("q");
            assert_eq!(
                driver.state_mut().doc.pending_ops(),
                0,
                "later ordinary live input keeps its normal commit boundary"
            );
        }
    }
    // phase: unopened_history_focus_returns_to_exactly_one_live_editor
    {
        use crate::widget::TextInput;
        for route in ["tab", "shift-tab", "pointer"] {
            let root = Column::new()
                .push(Box::new(TextInput::local().with_id("first-editor")))
                .push(Box::new(TextInput::local().with_id("second-editor")));
            let mut driver = FrameHarness::from_managed_root(
                State::new(),
                ManagedRoot::new(Box::new(root), registration()),
                (900, 650),
            )
            .unwrap();
            driver.click_named("second-editor").unwrap();
            driver.type_text("second");
            driver.tab(false);
            assert_eq!(
                driver.focused_access_node().unwrap().name.as_deref(),
                Some("History")
            );
            let target = match route {
                "tab" => {
                    driver.tab(false);
                    "first-editor"
                }
                "shift-tab" => {
                    driver.tab(true);
                    "second-editor"
                }
                _ => {
                    driver.click_named("first-editor").unwrap();
                    "first-editor"
                }
            };
            assert_eq!(
                driver.focused_access_node().map(|node| node.semantic_key),
                Some(Some(target.into())),
                "{route} must retain runner focus"
            );
            let inspection = driver.inspection();
            for id in ["first-editor", "second-editor"] {
                assert_eq!(
                    find_access(&inspection.root, id).unwrap().state.focused,
                    id == target,
                    "{route}: {id}"
                );
            }
            driver.key("x");
            let inspection = driver.inspection();
            assert_eq!(
                find_access(&inspection.root, "first-editor")
                    .unwrap()
                    .value
                    .as_deref(),
                Some(if target == "first-editor" { "x" } else { "" })
            );
            assert_eq!(
                find_access(&inspection.root, "second-editor")
                    .unwrap()
                    .value
                    .as_deref(),
                Some(if target == "second-editor" {
                    "secondx"
                } else {
                    "second"
                })
            );
            assert!(!driver.targets().contains_key("activity_timeline"));
        }
    }
    // phase: moving_host_and_rebuilding_chrome_preserves_real_accessibility_targets
    {
        for _ in 0..8 {
            let (driver, _, _) = versioned_driver((900, 650));
            let mut moved = vec![driver];
            let mut driver = moved.pop().unwrap();
            let open = driver.a11y_node_named("History").unwrap();
            driver
                .perform_a11y_action(open, &crate::a11y::Action::Activate)
                .unwrap();
            let row = driver
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.role() == accesskit::Role::ListItem)
                .unwrap()
                .0;
            driver
                .perform_a11y_action(row, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(driver.targets().contains_key("history-preview"));
            driver.resize((390, 650)).unwrap();
            let before = driver.a11y_node_named("Before").unwrap();
            driver
                .perform_a11y_action(before, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(driver.a11y_node_named("Historical: Before").is_some());
            let back = driver.a11y_node_named("Back to live").unwrap();
            driver
                .perform_a11y_action(back, &crate::a11y::Action::Activate)
                .unwrap();
            assert!(driver.targets().contains_key("live-effect"));
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn the_history_timeline_explains_scopes_and_announces_live_updates() {
    // phase: history_timeline_omits_unavailable_mutating_affordances
    {
        use crate::widget::ActivityTimeline;
        let (mut driver, _, _) = versioned_driver((900, 650));
        let resource = HistoryResource::new(
            "fixture",
            "lifetime-1",
            DEFINITION.app_id,
            crate::RecordId::for_tab(777),
        );
        let controller = HistoryController::new(resource, &driver.state_mut().doc).unwrap();
        let mut timeline =
            ActivityTimeline::with_entries(controller.entries().to_vec()).with_preview_selection();
        timeline.set_rect(Rect::new(0.0, 0.0, 400.0, 500.0));
        timeline.set_selection(Some((0, 0)));
        assert!(
            !timeline.begin_naming_selection(),
            "preview has no owned naming action"
        );
        for row in 0..8 {
            assert!(
                timeline.revert_chip_rect(row).is_none(),
                "restore is reviewed by host, never a direct timeline revert"
            );
        }
    }
    // phase: external_history_explains_why_no_versions_exist
    {
        static EXTERNAL: HistoryDefinition = HistoryDefinition {
            app_id: "dev.tilo.external-fixture",
            scopes: &[HistoryScopeDefinition {
                key: "session",
                title: "Remote session",
                presentation: HistoryPresentation::External {
                    reason: "Remote content has no document versions",
                },
                content_schema: None,
            }],
        };
        for viewport in [(390, 650), (900, 650)] {
            let registration = HistoryRegistration::new(
                &EXTERNAL,
                vec![HistorySourceRegistration::Unversioned {
                    scope: "session",
                    reason: "Remote content has no document versions",
                }],
            )
            .unwrap();
            let live = Column::new().push(Box::new(Button::new(
                "external-live-marker",
                "Retained live session",
            )));
            let mut driver = FrameHarness::from_managed_root(
                State::new(),
                ManagedRoot::new(Box::new(live), registration),
                viewport,
            )
            .unwrap();
            assert!(driver.targets().contains_key("external-live-marker"));
            driver.click_named("history-open").unwrap();
            let inspection = driver.inspection_json().unwrap();
            assert!(inspection.contains("Remote content has no document versions"));
            assert!(!driver.targets().contains_key("activity_timeline"));
            assert!(
                !inspection.contains("Retained live session"),
                "External History presents only its limitation at {viewport:?}"
            );
            driver.click_named("history-open").unwrap();
            assert!(
                driver
                    .inspection_json()
                    .unwrap()
                    .contains("Retained live session")
            );
        }
    }
    // phase: preview_wheel_navigation_is_scoped_to_canvas_and_preserved_per_compare_side
    {
        let (mut driver, target, _) = versioned_driver((900, 650));
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        driver.click_named("history-after").unwrap();
        driver.scroll(0.0, 1.0);
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first; scroll 0"),
            "toolbar wheel is not canvas navigation"
        );
        let canvas = driver.targets()["history-preview"].center();
        driver.send(&crate::synthetic_input::cursor_moved(canvas.x, canvas.y));
        driver.scroll(0.0, 1.0);
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first; scroll 32")
        );
        driver.key("PageDown");
        driver.type_text("not-preview-input");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first; scroll 32")
        );
        driver.click_named("history-before").unwrap();
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: ; scroll 0")
        );
        driver.click_named("history-after").unwrap();
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: first; scroll 32")
        );
    }
    // phase: pinned_history_announces_live_updates_and_clears_notice_on_return
    {
        let (mut driver, _, heads) = versioned_driver((390, 650));
        let id = crate::history_source::entry_id_from_hash(&heads[0].to_string());
        let target = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&id)
        );
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        assert!(driver.a11y_node_named("Live content has updates").is_none());
        driver
            .state_mut()
            .doc
            .put(ROOT, "body", "remote update")
            .unwrap();
        driver.state_mut().commit_action(None, None);
        driver.relayout();
        let notice = driver.a11y_node_named("Live content has updates").unwrap();
        let node = driver
            .a11y_nodes()
            .into_iter()
            .find(|(id, _)| *id == notice)
            .unwrap()
            .1;
        assert_eq!(node.live(), Some(accesskit::Live::Polite));
        let bounds = node.bounds().unwrap();
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= 390.0 && bounds.y1 <= 650.0);
        if let Ok(directory) = std::env::var("TILO_HISTORY_SHOTS") {
            driver.render_png(&std::path::Path::new(&directory).join("narrow-live-update.png"));
            driver.resize((900, 650)).unwrap();
            driver.render_png(&std::path::Path::new(&directory).join("wide-live-update.png"));
        }
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Notebook: second")
        );
        driver.click_named("history-back-live").unwrap();
        assert!(driver.a11y_node_named("Live content has updates").is_none());
    }
}

/// Real lazy composition: the initially unbound live child is replaced by a
/// TileHost in sync or layout, as responsive/component owners may mount later.
struct LazyOwnerRoot {
    child: Box<dyn Widget>,
    mount: Rc<Cell<bool>>,
    pending: bool,
    during_layout: bool,
    mounted: bool,
    dirty: bool,
}
impl LazyOwnerRoot {
    fn mount_owner(&mut self) {
        if self.mounted || !self.mount.get() {
            return;
        }
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::Stub {
            kind: crate::widget::AppKind::Terminal,
            label: "separate-owner".into(),
        });
        let store = crate::record::RecordStore::in_memory();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(301),
            crate::record::RecordKind::new("scratch"),
        );
        if self.pending {
            tile.bind_record(store, checkout);
        } else {
            tile.bind_or_create_record(store, checkout);
        }
        self.child = Box::new(Column::new().push(Box::new(tile)));
        self.mounted = true;
        self.dirty = true;
    }
}
impl Widget for LazyOwnerRoot {
    fn name(&self) -> &str {
        "lazy-owner-fixture"
    }
    fn get_rect(&self) -> Rect {
        self.child.get_rect()
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        if !self.during_layout {
            self.mount_owner();
        } else if self.mount.get() && !self.mounted {
            // A widget that will compose an owner during its next build has
            // to request that build: a retained host never rebuilds unasked.
            self.dirty = true;
        }
        self.child.sync(doc, path);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        if self.during_layout {
            self.mount_owner();
        }
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.child.handle_event(event, ctx);
    }
}
fn lazy_owner_root(mount: Rc<Cell<bool>>, pending: bool, during_layout: bool) -> Box<dyn Widget> {
    Box::new(LazyOwnerRoot {
        child: Box::new(Column::new()),
        mount,
        pending,
        during_layout,
        mounted: false,
        dirty: false,
    })
}

fn checked_preview(
    _: crate::history_mode::checked_files::FilePreviewInput,
) -> Result<Box<dyn crate::history_mode::HistoryPreview>, String> {
    Err("no preview selected by the composition test".into())
}

static CHECKED_COMPOSITION: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.checked-composition-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "file",
        title: "File",
        presentation: HistoryPresentation::CheckedFiles {
            create: checked_preview,
        },
        content_schema: None,
    }],
};

struct CheckedCompositionRoot {
    inner: Box<dyn Widget>,
    source: crate::history_mode::checked_files::FileHistorySource,
}

impl Widget for CheckedCompositionRoot {
    crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

    crate::testing::delegate_widget_methods!(
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

    fn name(&self) -> &str {
        "checked-composition-root"
    }
}

impl crate::history_mode::capability::DocumentHistory for CheckedCompositionRoot {
    fn history_registration(
        &self,
    ) -> Result<Option<HistoryRegistration>, HistoryRegistrationError> {
        HistoryRegistration::new(
            &CHECKED_COMPOSITION,
            vec![HistorySourceRegistration::CheckedFiles {
                scope: "file",
                source: self.source.clone(),
            }],
        )
        .map(Some)
    }
    fn document_history_definition(&self) -> Option<&'static HistoryDefinition> {
        Some(&CHECKED_COMPOSITION)
    }
}

#[test]
fn checked_files_activity_keeps_the_wide_live_surface() {
    let source = crate::history_mode::checked_files::FileHistorySource::default();
    let registration = HistoryRegistration::new(
        &CHECKED_COMPOSITION,
        vec![HistorySourceRegistration::CheckedFiles {
            scope: "file",
            source: source.clone(),
        }],
    )
    .unwrap();
    let live = CheckedCompositionRoot {
        inner: Box::new(Column::new().push(Box::new(Button::new(
            "checked-live-marker",
            "Retained checked file",
        )))),
        source,
    };
    let mut driver = FrameHarness::from_managed_root(
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
            .contains("Retained checked file"),
        "wide CheckedFiles activity keeps the live surface beside its controls"
    );
}

const TOOLKIT_EN: &str = include_str!("../../locales/en-US.ftl");
const TOOLKIT_DE: &str = include_str!("../../locales/de-DE.ftl");

#[test]
fn history_chrome_follows_the_desktop_language() {
    crate::i18n::I18nManager::install(
        "en-US",
        "de-DE",
        &[("en-US", TOOLKIT_EN), ("de-DE", TOOLKIT_DE)],
    )
    .unwrap();
    let (mut driver, _, _) = versioned_driver((900, 650));
    let open = driver.a11y_node_named("Verlauf").unwrap();
    driver
        .perform_a11y_action(open, &crate::a11y::Action::Activate)
        .unwrap();
    let row = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == accesskit::Role::ListItem)
        .unwrap()
        .0;
    driver
        .perform_a11y_action(row, &crate::a11y::Action::Activate)
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
        .perform_a11y_action(back, &crate::a11y::Action::Activate)
        .unwrap();
    assert!(driver.targets().contains_key("live-effect"));
}

/// Every History host message is translated and formats with the variables
/// the host passes, so no German desktop falls back to English copy.
#[test]
fn history_host_messages_exist_in_every_shipped_language() {
    let keys: Vec<&str> = TOOLKIT_EN
        .lines()
        .filter_map(|line| line.split_once(" = ").map(|(key, _)| key))
        .filter(|key| {
            [
                "history-mode-",
                "history-files-",
                "history-media-",
                "history-sync-",
            ]
            .iter()
            .any(|prefix| key.starts_with(prefix))
        })
        .collect();
    assert!(keys.len() > 80, "{keys:?}");
    let args: &[(&str, crate::i18n::ArgValue)] = &[
        ("id", 7.into()),
        ("owner", "owner".into()),
        ("action", "app/action".into()),
        ("arguments", "{}".into()),
        ("phrase", "CONFIRM".into()),
        ("file", "brief.md".into()),
        ("revision", "r1".into()),
        ("identity", "abcdef".into()),
        ("count", 2.into()),
        ("intent", "i1".into()),
        ("mapping", "m".into()),
    ];
    for (locale, catalog) in [("en-US", TOOLKIT_EN), ("de-DE", TOOLKIT_DE)] {
        crate::i18n::I18nManager::install(locale, locale, &[(locale, catalog)]).unwrap();
        for key in &keys {
            assert!(
                crate::i18n::I18nManager::has_message(key),
                "{locale} lacks {key}"
            );
            let value = crate::i18n::I18nManager::format(key, args);
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

/// A `NavShell` app carries the window's History control in its header row;
/// under the app's registered History that control opens and closes that
/// History, never the standalone chrome the app does not have.
#[test]
fn header_history_control_opens_the_registered_history() {
    fn history_toggled(driver: &FrameHarness) -> Vec<Option<accesskit::Toggled>> {
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
    let shell = crate::widget::NavShell::new()
        .content_header("Notes", vec![])
        .content(Box::new(crate::widget::Text::new("Body")));
    let mut driver = FrameHarness::from_managed_root(
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

    // The History door the control is bound to closes it too.
    driver.click_named("nav-shell-history").unwrap();
    assert!(driver.targets().contains_key("activity_timeline"));
    driver
        .invoke_local_history_action(
            &tilo_actions::ActionAddress::new(
                crate::standalone_history_host::HISTORY_RESOURCE,
                "hide",
            ),
            None,
        )
        .unwrap();
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(driver.state_mut().doc.get_heads(), before);
}

/// Opening History narrows the window's room for the app: its `NavShell`
/// lays its header rows out again at the width it is given, so header
/// actions keep their own places and never fold onto one another.
#[test]
fn opening_history_lays_the_shell_out_at_its_narrower_width() {
    use crate::icons::Icon;
    use crate::widget::decorator::Decorator;
    /// A header action that remembers where it was last laid out.
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
    let shell = crate::widget::NavShell::new()
        .sidebar(Box::new(crate::widget::Text::new("Library")))
        .overview(Box::new(crate::widget::Text::new("List")))
        .overview_header("All snippets", overview)
        .content_header("Multi-stage build", content)
        .content(Box::new(crate::widget::Text::new("Body")));
    let mut driver = FrameHarness::from_managed_root(
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

/// The text at `key` of `doc`'s root, if it holds text.
fn root_text(doc: &automerge::AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
}

// USER REQUIREMENT (2026-10-19): "A right-side panel per window or active tab
// ... Selecting an entry previews that state in place (read-only), with
// Restore or Restore just this change."
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
    let mut driver = FrameHarness::from_managed_root(
        state,
        ManagedRoot::new(Box::new(root), registration()),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    let targets = driver.targets();
    let timeline = targets["activity_timeline"];
    assert!(
        timeline.x0 >= 900.0 - host::SIDE_PANEL_WIDTH - 1.0,
        "History stands at the window's trailing side: {timeline:?}"
    );
    assert!(
        (timeline.x1 - 900.0).abs() < 24.0,
        "the panel reaches the window's trailing edge: {timeline:?}"
    );

    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &body_change.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
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
