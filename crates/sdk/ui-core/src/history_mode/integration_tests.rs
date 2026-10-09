use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHost, runner_permissions, runner_probe_definition,
};
use crate::widget::{AppModel, ModelPath};
use crate::{
    State,
    headless::Driver,
    widget::{BoundAppWidget, Widget},
};
use automerge::{ROOT, transaction::Transactable};

static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
        content_schema: None,
    }],
};

#[test]
fn managed_components_bind_shared_history_to_their_real_owner() {
    // phase: managed_bound_history_resolves_real_owner_instead_of_outer_state
    {
        let host = ProbeHost::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 904, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        let selected = binding
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "bound first").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let hash = doc.base_heads()[0];
                doc.put(ROOT, "body", "bound latest").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                format!(
                    "timeline-entry:{}",
                    crate::widget::history_group::hex16(
                        &crate::history_source::entry_id_from_hash(&hash.to_string())
                    )
                )
            })
            .unwrap();
        let root = Box::new(BoundAppWidget::new(component));
        let source =
            HistorySourceRegistration::owned("document", APP, root.history_owner().unwrap())
                .unwrap();
        let registration = HistoryRegistration::new(&BOUND, vec![source]).unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "body", "unrelated outer").unwrap();
        state.commit_action(None, None);
        let mut driver =
            Driver::from_managed_root(state, ManagedRoot::new(root, registration), (900, 650))
                .expect("the registered actual bound owner must be admitted");
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&selected),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        assert!(driver.targets().contains_key("history-preview"));
        let names = format!("{:?}", driver.a11y_nodes());
        assert!(
            names.contains("bound"),
            "preview reads bound content: {names}"
        );
        assert!(!names.contains("unrelated outer"));
    }
    // phase: managed_component_bundle_automatically_installs_shared_history_in_frame_runner
    {
        let (_host, component) = component_with_history();
        let mut driver = Driver::from_component(
            component,
            (900, 650),
            crate::application::ComponentRunnerInputs {
                theme: crate::Theme::dark(),
                text_renderer: crate::font_catalog::fixture_renderer(),
            },
        )
        .unwrap();
        assert!(
            driver.targets().contains_key("history-open"),
            "managed component metadata follows its actual runner"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
    // phase: managed_component_tile_automatically_composes_shared_history
    {
        let (_host, component) = component_with_history();
        let root = crate::widget::TileHost::from_component(component);
        let mut driver = Driver::new(State::new(), Box::new(root), (900, 650));
        assert!(
            driver.targets().contains_key("history-open"),
            "a managed tile retains the component's History bundle"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
}

pub(crate) static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.restore-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
        content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
    }],
};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn managed_review_confirmation_rechecks_authority_and_freshness() {
    // phase: managed_review_confirm_restores_exactly_one_authored_change_and_can_cancel
    {
        let mut state = State::new();
        state.doc.put(ROOT, "body", "old").unwrap();
        state.commit_action(None, None);
        let hash = state.doc.base_heads()[0];
        state.doc.put(ROOT, "body", "live").unwrap();
        state.commit_action(None, None);
        let before = state.doc.base_heads();
        let source = HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "restore-lifetime",
                RESTORABLE.app_id,
                crate::RecordId::for_tab(17),
            ),
        };
        let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let target = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                &hash.to_string()
            ))
        );
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        driver
            .click_named("history-restore")
            .expect("registered content schema exposes reviewed restore");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Restore version")
        );
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.key("Escape");
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.click_named("history-restore").unwrap();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "live"
        );
    }
    // phase: actual_confirmation_rechecks_unchanged_grant_expiry_replacement_and_actor_mapping
    {
        use crate::permissions::*;
        for case in ["expiry", "replacement", "actor"] {
            let mut driver = review_driver();
            driver.set_permission_ctx(grant(LOCAL_USER, RESTORABLE.app_id, Some(150)));
            driver.click_named("history-restore").unwrap();
            let before = driver.state_mut().doc.base_heads();
            match case {
                "expiry" => NOW.set(200),
                "replacement" => driver.set_permission_ctx(
                    crate::permissions::PermissionContext::stranger(LOCAL_USER),
                ),
                _ => driver.set_permission_ctx(grant("another-person", RESTORABLE.app_id, None)),
            }
            let confirm = driver
                .a11y_node_named("Restore version")
                .expect("real modal footer exports AT action");
            driver
                .perform_a11y_action(confirm, &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(driver.state_mut().doc.base_heads(), before, "{case}");
            assert!(
                driver
                    .inspection_json()
                    .unwrap()
                    .contains("no longer have permission"),
                "{case}"
            );
            assert!(driver.a11y_node_named("Refresh review").is_some());
        }
    }
    // phase: stale_review_requires_refresh_and_cancelled_footer_target_cannot_confirm
    {
        let mut driver = review_driver();
        driver.click_named("history-restore").unwrap();
        let old_confirm = driver.a11y_node_named("Restore version").unwrap();
        driver.key("Escape");
        assert!(
            driver
                .perform_a11y_action(old_confirm, &crate::a11y::Action::Activate)
                .is_err()
        );
        driver.click_named("history-restore").unwrap();
        driver
            .state_mut()
            .doc
            .put(ROOT, "body", "remote change")
            .unwrap();
        driver.state_mut().commit_action(None, None);
        let before = driver.state_mut().doc.base_heads();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        assert!(driver.a11y_node_named("Refresh review").is_some());
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            driver.state_mut().doc.base_heads(),
            before,
            "refresh itself does not write"
        );
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
    }
}

thread_local! { static NOW: std::cell::Cell<u64> = const { std::cell::Cell::new(100) }; }
fn current_time() -> Option<u64> {
    Some(NOW.get())
}
fn review_driver() -> Driver {
    NOW.set(100);
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    let hash = state.doc.base_heads()[0];
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let source = HistorySourceRegistration::HostDocument {
        scope: "document",
        resource: HistoryResource::new(
            "fixture",
            "restore-lifetime",
            RESTORABLE.app_id,
            crate::RecordId::for_tab(18),
        ),
    };
    let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
        (900, 650),
    )
    .unwrap();
    driver.set_history_clock(current_time);
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &hash.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver
}

#[test]
fn managed_review_dialog_drains_and_returns_to_retained_preview() {
    let mut driver = review_driver();
    assert!(driver.targets().contains_key("history-preview"));
    assert!(
        !driver.frame_output().needs_redraw,
        "selected preview stayed dirty after draw"
    );

    driver.click_named("history-restore").unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Restore version")
    );
    assert!(
        !driver.frame_output().needs_redraw,
        "review dialog stayed dirty after draw: {:?}",
        driver.frame_requesters()
    );
    assert!(!driver.poll_frame().unwrap());

    driver.key("Escape");
    assert!(driver.targets().contains_key("history-preview"));
    assert!(driver.inspection_json().unwrap().contains("Notebook: old"));
    assert!(
        !driver.frame_output().needs_redraw,
        "retained preview was not current after closing review"
    );
    assert!(!driver.poll_frame().unwrap());
}
fn grant(actor: &str, app: &str, until: Option<u64>) -> crate::permissions::PermissionContext {
    use crate::permissions::*;
    PermissionContext::granted(
        actor,
        &TreePath::root().app(app),
        policy::KindSet::ALL,
        until,
    )
    .at_unix_time(100)
}

#[derive(Default)]
struct DeferredMedia(std::cell::RefCell<Vec<(HistoricalAssetRequest, HistoricalAssetResponse)>>);
impl HistoricalAssetSource for DeferredMedia {
    fn request(&self, request: HistoricalAssetRequest, response: HistoricalAssetResponse) {
        self.0.borrow_mut().push((request, response));
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn historical_media_uses_only_the_selected_snapshot_in_every_registration() {
    // phase: historical_media_requests_capture_both_sides_and_late_failure_cannot_replace_new_selection
    {
        let provider = std::rc::Rc::new(DeferredMedia::default());
        let registration = super::host_tests::registration()
            .with_asset_source("document", provider.clone())
            .unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "body", "first").unwrap();
        state.commit_action(None, None);
        let first = state.doc.base_heads()[0];
        state.doc.put(ROOT, "body", "second").unwrap();
        state.commit_action(None, None);
        state.doc.put(ROOT, "body", "live").unwrap();
        state.commit_action(None, None);
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let select = |driver: &mut Driver, hash: automerge::ChangeHash| {
            let target = format!(
                "timeline-entry:{}",
                crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                    &hash.to_string()
                ))
            );
            driver
                .perform_semantic_named(
                    "activity_timeline",
                    Some(&target),
                    &crate::a11y::Action::Activate,
                )
                .unwrap();
        };
        select(&mut driver, first);
        assert_eq!(
            provider.0.borrow().len(),
            2,
            "source executes outside renderer for each immutable side"
        );
        assert!(format!("{:?}", driver.a11y_nodes()).contains("Loading historical media"));
        let old = provider.0.borrow_mut().drain(..).collect::<Vec<_>>();
        // Return through live and select the exact same frontier: generation, not
        // just heads/resource equality, must prevent the old response winning.
        driver.click_named("history-back-live").unwrap();
        driver.click_named("history-open").unwrap();
        select(&mut driver, first);
        assert_eq!(provider.0.borrow().len(), 2);
        for (_, response) in old {
            response.complete(Err("old request must stay invisible".into()));
        }
        driver.key("Shift");
        assert!(!format!("{:?}", driver.a11y_nodes()).contains("old request must stay invisible"));
        let mut current = provider.0.borrow_mut().drain(..).collect::<Vec<_>>();
        let (_, after) = current.pop().unwrap();
        after.complete(Err("Recorded media is unavailable".into()));
        driver.key("Shift");
        assert!(format!("{:?}", driver.a11y_nodes()).contains("Recorded media is unavailable"));
        driver.click_named("history-before").unwrap();
        assert!(
            !format!("{:?}", driver.a11y_nodes()).contains("Recorded media is unavailable"),
            "after result cannot relabel before"
        );
        let (_, before) = current.pop().unwrap();
        driver.click_named("history-back-live").unwrap();
        before.complete(Err("closed request must stay invisible".into()));
        driver.key("Shift");
        assert!(
            !format!("{:?}", driver.a11y_nodes()).contains("closed request must stay invisible")
        );
    }
    // phase: media_preview_uses_only_digest_recorded_in_immutable_selected_snapshot
    {
        use sha2::{Digest, Sha256};
        use std::{collections::BTreeMap, sync::Arc};
        let historical: Arc<[u8]> = Arc::from(b"historical photograph".as_slice());
        let digest = hex::encode(Sha256::digest(&historical));
        let current: Arc<[u8]> = Arc::from(b"current photograph".as_slice());
        let current_digest = hex::encode(Sha256::digest(&current));
        let provider = std::rc::Rc::new(DeferredMedia::default());
        let mut state = State::new();
        state.doc.put(ROOT, "media", digest.clone()).unwrap();
        state.commit_action(None, None);
        let selected = state.doc.base_heads()[0];
        state
            .doc
            .put(ROOT, "media", current_digest.clone())
            .unwrap();
        state.commit_action(None, None);
        let registration = HistoryRegistration::new(
            &MEDIA,
            vec![HistorySourceRegistration::HostDocument {
                scope: "media",
                resource: HistoryResource::new(
                    "profile",
                    "lifetime",
                    MEDIA.app_id,
                    crate::RecordId::for_tab(9),
                ),
            }],
        )
        .unwrap()
        .with_asset_source("media", provider.clone())
        .unwrap();
        let mut driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let target = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                &selected.to_string()
            ))
        );
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        let mut requests = provider.0.borrow_mut().drain(..).collect::<Vec<_>>();
        let (request, response) = requests.pop().unwrap();
        use automerge::ReadDoc;
        let snapshot = automerge::AutoCommit::load(request.snapshot.bytes()).unwrap();
        assert_eq!(
            snapshot.get(ROOT, "media").unwrap().unwrap().0.as_str(),
            Some(digest.as_str())
        );
        response.complete(Ok(BTreeMap::from([
            (digest, historical),
            (current_digest, current),
        ])));
        driver.key("Shift");
        let names = format!("{:?}", driver.a11y_nodes());
        assert!(names.contains("historical photograph"));
        assert!(!names.contains("current photograph"));
    }
    // phase: late_asset_result_after_real_component_owner_close_stays_unavailable
    {
        let (host, mut component) = component_with_history();
        let binding = component.callback_binding();
        let hash = binding
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "selected").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let selected = doc.base_heads()[0];
                doc.put(ROOT, "body", "live").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                selected
            })
            .unwrap();
        let provider = std::rc::Rc::new(DeferredMedia::default());
        component.history = Some(
            component
                .history
                .take()
                .unwrap()
                .with_asset_source("document", provider.clone())
                .unwrap(),
        );
        let mut driver = Driver::from_component(
            component,
            (900, 650),
            crate::application::ComponentRunnerInputs {
                theme: crate::Theme::dark(),
                text_renderer: crate::font_catalog::fixture_renderer(),
            },
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let selected = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                &hash.to_string()
            ))
        );
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&selected),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        let requests = provider.0.borrow_mut().drain(..).collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        driver.close_components();
        for (_, response) in requests {
            response.complete(Err("closed owner response must not appear".into()));
        }
        driver.relayout();
        let tree = format!("{:?}", driver.a11y_nodes());
        assert!(!tree.contains("closed owner response must not appear"));
        assert!(!driver.targets().contains_key("history-preview"));
        drop(host);
    }
    // phase: automatic_asset_factory_delivers_selected_requests_in_standalone_registration
    {
        let mut state = State::new();
        state.doc.put(ROOT, "body", "selected media").unwrap();
        state.commit_action(None, None);
        let selected = state.doc.base_heads()[0];
        state.doc.put(ROOT, "body", "live media").unwrap();
        state.commit_action(None, None);
        let registration = HistoryRegistration::installed_app_state(
            &AUTOMATIC_MEDIA_DEFINITION,
            None,
            uuid::Uuid::new_v4(),
        )
        .unwrap();
        let driver = Driver::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        check_automatic_media(driver, selected);
    }
    // phase: automatic_asset_factory_delivers_selected_requests_in_owned_component
    {
        let host = ProbeHost::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 978, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        let selected = binding
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "selected media").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let selected = doc.base_heads()[0];
                doc.put(ROOT, "body", "live media").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                selected
            })
            .unwrap();
        component
            .register_document_history(&AUTOMATIC_MEDIA_DEFINITION)
            .unwrap();
        let driver = Driver::from_component(
            component,
            (900, 650),
            crate::application::ComponentRunnerInputs {
                theme: crate::Theme::dark(),
                text_renderer: crate::font_catalog::fixture_renderer(),
            },
        )
        .unwrap();
        check_automatic_media(driver, selected);
    }
    // phase: automatic_asset_factory_delivers_selected_requests_in_bound_tile
    {
        let directory = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(directory.path().to_path_buf());
        let store = owners.open(APP, "automatic-media").unwrap();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(979),
            crate::record::RecordKind::new("deck"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "selected media").unwrap();
                Ok(())
            })
            .unwrap();
        let selected = store
            .with_history(&checkout.id, |doc, history| {
                let selected = doc.base_heads()[0];
                doc.put(ROOT, "body", "live media").unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(crate::collaboration::local_user_id()),
                );
                selected
            })
            .unwrap();
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            TileEditor(crate::widget::Text::new("Live")),
        )));
        tile.bind_record(store, checkout);
        tile.register_document_history(&AUTOMATIC_MEDIA_DEFINITION)
            .unwrap();
        check_automatic_media(
            Driver::new(State::new(), Box::new(tile), (900, 650)),
            selected,
        );
    }
}

struct MediaPreview {
    description: String,
    view: PreviewView,
}
impl HistoryPreview for MediaPreview {
    fn layout(
        &mut self,
        _: vello::kurbo::Rect,
        _: &mut crate::text_render::TextRenderer,
        _: &crate::theme::Theme,
    ) -> Result<(), String> {
        Ok(())
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut crate::text_render::TextRenderer,
        _: &crate::theme::Theme,
    ) {
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Scroll { dx, dy } = command {
            self.view.scroll = (dx, dy);
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
            description: self.description.clone(),
            pages: vec![],
        }
    }
}
fn media_preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    use automerge::ReadDoc;
    let doc = automerge::AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let digest = doc
        .get(ROOT, "media")
        .map_err(|e| e.to_string())?
        .and_then(|(v, _)| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let description = input
        .assets
        .get(&digest)
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .unwrap_or_else(|| "Recorded media missing".into());
    Ok(Box::new(MediaPreview {
        description,
        view: input.initial_view,
    }))
}
static MEDIA: HistoryDefinition = HistoryDefinition {
    app_id: "media-app",
    scopes: &[HistoryScopeDefinition {
        key: "media",
        title: "Photograph",
        presentation: HistoryPresentation::Domain {
            create: media_preview,
        },
        content_schema: None,
    }],
};

pub(crate) fn component_with_history() -> (ProbeHost, crate::application::AppComponent) {
    let host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 950, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
}

struct LegacyTiles {
    _directory: tempfile::TempDir,
    _owners: crate::record::RecordOwners,
    store: crate::record::RecordStore,
    first: crate::record::RecordCheckout,
    second: crate::record::RecordCheckout,
    driver: Driver,
}
impl LegacyTiles {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
        let store = owners.open(RESTORABLE.app_id, "tile-history-room").unwrap();
        let first = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(311),
            crate::record::RecordKind::new("note"),
        );
        let second = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(312),
            crate::record::RecordKind::new("note"),
        );
        for checkout in [&first, &second] {
            store
                .reserve_record(checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", "live").unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let root = legacy_tiles_root(&store, &first, &second);
        let mut state = State::new();
        state.doc.put(ROOT, "body", "outer").unwrap();
        state.commit_action(None, None);
        let driver = Driver::new(state, root, (1200, 650));
        Self {
            _directory: directory,
            _owners: owners,
            store,
            first,
            second,
            driver,
        }
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn tiled_history_keeps_pending_owners_and_checks_current_authority() {
    // phase: tiled_history_semantic_open_preserves_pending_owner_and_other_tile_stays_live
    {
        let mut fixture = LegacyTiles::new();
        fixture.driver.click_named("first-editor").unwrap();
        fixture
            .store
            .with(&fixture.first.id, |doc| {
                doc.put(ROOT, "unfinished", "pending").unwrap()
            })
            .unwrap();
        let pending = fixture
            .store
            .with(&fixture.first.id, |doc| {
                (doc.base_heads(), doc.pending_ops())
            })
            .unwrap();
        let outer = fixture.driver.state_mut().doc.base_heads();
        fixture
            .driver
            .perform_named("history-open", &crate::a11y::Action::Activate)
            .unwrap();
        assert!(fixture.driver.targets().contains_key("activity_timeline"));
        assert!(
            !fixture.driver.targets().contains_key("first-editor"),
            "frozen live controls cannot remain addressable"
        );
        assert_eq!(
            fixture
                .store
                .with(&fixture.first.id, |doc| (
                    doc.base_heads(),
                    doc.pending_ops()
                ))
                .unwrap(),
            pending
        );
        fixture.driver.click_named("second-editor").unwrap();
        fixture.driver.type_text("B");
        fixture
            .store
            .with_history(&fixture.second.id, |doc, history| {
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(crate::collaboration::local_user_id()),
                );
            })
            .unwrap();
        let revision = fixture
            .store
            .with_history(&fixture.second.id, |_, history| history.revision())
            .unwrap();
        fixture
            .driver
            .state_mut()
            .doc
            .put(ROOT, "outer unfinished", "preserved")
            .unwrap();
        let outer_pending = fixture.driver.state_mut().doc.pending_ops();
        fixture
            .driver
            .perform_named(
                "second-editor",
                &crate::a11y::Action::SetValue("second changed".into()),
            )
            .unwrap();
        assert_eq!(
            fixture
                .store
                .with(&fixture.second.id, |doc| {
                    use automerge::ReadDoc;
                    let (value, object) = doc.get(ROOT, "body").unwrap().unwrap();
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| doc.text(object).unwrap())
                })
                .unwrap(),
            "second changed"
        );
        assert_eq!(
            fixture
                .store
                .with(&fixture.first.id, |doc| (
                    doc.base_heads(),
                    doc.pending_ops()
                ))
                .unwrap(),
            pending
        );
        assert_eq!(fixture.driver.state_mut().doc.base_heads(), outer);
        assert_eq!(fixture.driver.state_mut().doc.pending_ops(), outer_pending);
        assert_eq!(
            fixture
                .store
                .with_history(&fixture.second.id, |_, history| history.revision())
                .unwrap(),
            revision + 1
        );
        assert!(
            fixture.driver.perform_undo(),
            "runner undo uses focused legacy owner"
        );
        assert_eq!(
            fixture
                .store
                .with(&fixture.second.id, |doc| {
                    use automerge::ReadDoc;
                    let (_, object) = doc.get(ROOT, "body").unwrap().unwrap();
                    doc.text(object).unwrap()
                })
                .unwrap(),
            "liveB"
        );
        assert_eq!(fixture.driver.state_mut().doc.base_heads(), outer);
        assert_eq!(fixture.driver.state_mut().doc.pending_ops(), outer_pending);
        fixture.driver.key("Tab");
        assert!(fixture.driver.focused_access_node().is_some());
    }
    // phase: tiled_semantic_edit_checks_current_authority_and_focus_does_not_commit
    {
        use crate::permissions::*;
        let mut fixture = LegacyTiles::new();
        fixture
            .store
            .with(&fixture.second.id, |doc| {
                doc.put(ROOT, "unfinished", "pending").unwrap()
            })
            .unwrap();
        let before = fixture
            .store
            .with(&fixture.second.id, |doc| {
                (doc.base_heads(), doc.pending_ops())
            })
            .unwrap();
        fixture
            .driver
            .perform_named("second-editor", &crate::a11y::Action::Focus)
            .unwrap();
        assert_eq!(
            fixture
                .store
                .with(&fixture.second.id, |doc| (
                    doc.base_heads(),
                    doc.pending_ops()
                ))
                .unwrap(),
            before
        );
        fixture
            .driver
            .set_permission_ctx(crate::permissions::PermissionContext::stranger(LOCAL_USER));
        assert!(
            fixture
                .driver
                .perform_named(
                    "second-editor",
                    &crate::a11y::Action::SetValue("denied".into())
                )
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .with(&fixture.second.id, |doc| (
                    doc.base_heads(),
                    doc.pending_ops()
                ))
                .unwrap(),
            before
        );
    }
}

pub(crate) struct TileEditor<T: Widget = crate::testing::OwnedText>(pub(crate) T);
crate::testing::test_app_widget!(impl [T: Widget + 'static] TileEditor<T>, "Tile editor");
impl<T: Widget + 'static> Widget for TileEditor<T> {
    crate::provides!(dyn crate::record::capability::RecordScoped);

    crate::testing::delegate_widget_methods!(0: sync, draw, get_rect);
    fn name(&self) -> &str {
        "TileEditor"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.0]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.0]
    }
    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        self.0.update(doc, path)
    }
    fn has_separate_document_owner(&self) -> bool {
        false
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        self.0.start_operation(
            op,
            &mut crate::widget::Edits::lend(doc.document_mut()),
            path,
        )
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let child = self.0.build_layout(tree, theme, nodes);
        tree.new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(500.0),
                    height: taffy::Dimension::length(40.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap()
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let layout = tree.layout(node).unwrap();
        self.0.apply_layout(
            tree,
            tree.children(node).unwrap()[0],
            vello::kurbo::Point::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
        );
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.0.handle_event(
            event,
            &mut ctx.reborrow(ctx.path, ctx.taffy.children(ctx.node_id).unwrap()[0]),
        );
    }
}

impl<T: Widget + 'static> crate::record::capability::RecordScoped for TileEditor<T> {
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        crate::record::capability::record_intent(&self.0)
    }
}

pub(crate) fn legacy_tiles_root(
    store: &crate::RecordStore,
    first: &crate::record::RecordCheckout,
    second: &crate::record::RecordCheckout,
) -> Box<dyn Widget> {
    let tile = |checkout: &crate::record::RecordCheckout, name: &str| {
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            TileEditor(crate::testing::OwnedText::new(
                "body",
                crate::widget::TextInput::local()
                    .with_id(name)
                    .with_accessible_name(name),
            )),
        )));
        tile.bind_record(store.clone(), checkout.clone());
        tile
    };
    let mut first_tile = tile(first, "first-editor");
    first_tile.register_document_history(&RESTORABLE).unwrap();
    let second_tile = tile(second, "second-editor");
    let root = crate::widget::Row::new()
        .no_wrap()
        .push(Box::new(first_tile))
        .push(Box::new(second_tile));
    Box::new(root)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn legacy_admission_refuses_busy_rebound_dropped_or_replaced_owners() {
    // phase: legacy_raw_admission_refuses_busy_rebound_and_dropped_owner_before_callback
    {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let mut owners_a = crate::record::RecordOwners::new(first_dir.path().to_path_buf());
        let mut owners_b = crate::record::RecordOwners::new(second_dir.path().to_path_buf());
        let a = owners_a.open(RESTORABLE.app_id, "same-room").unwrap();
        let b = owners_b.open(RESTORABLE.app_id, "same-room").unwrap();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(913),
            crate::record::RecordKind::new("note"),
        );
        for (store, body) in [(&a, "first"), (&b, "second")] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", body).unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let content = || {
            crate::widget::TileContent::App(Box::new(TileEditor(crate::testing::OwnedText::new(
                "body",
                crate::widget::TextInput::local().with_id("legacy-leaf"),
            ))))
        };
        let mut tile = crate::widget::TileHost::new(content());
        tile.bind_record(a.clone(), checkout.clone());
        let mut outer = automerge::AutoCommit::new();
        outer.put(ROOT, "outer", "pending").unwrap();
        tile.sync(&outer, &ROOT);
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let root = tile.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
        let target = nodes
            .iter()
            .find_map(|(node, pointer)| unsafe {
                (&**pointer)
                    .test_id()
                    .is_some_and(|id| id == "legacy-leaf")
                    .then_some(*node)
            })
            .unwrap();
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&tile, &tree, root, &nodes);
        }
        let outer_before = (outer.base_heads(), outer.pending_ops());
        let called = std::cell::Cell::new(false);
        a.with(&checkout.id, |_| {
            let refused = unsafe {
                crate::bound_dispatch::invoke_bound_node(
                    &bindings,
                    &nodes,
                    target,
                    &mut outer,
                    &ROOT,
                    |_, _, _| called.set(true),
                )
            };
            assert!(
                refused.is_err(),
                "a busy exact owner never falls through to outer State"
            );
        })
        .unwrap();
        tile.bind_record(b.clone(), checkout.clone());
        let refused = unsafe {
            crate::bound_dispatch::invoke_bound_node(
                &bindings,
                &nodes,
                target,
                &mut outer,
                &ROOT,
                |_, _, _| called.set(true),
            )
        };
        assert!(
            refused.is_err(),
            "same record spelling in a different store invalidates prior admission"
        );
        drop(tile);
        assert!(
            unsafe {
                crate::bound_dispatch::inspect_bound_node(&bindings, &nodes, target, |_| {
                    called.set(true)
                })
            }
            .is_err()
        );
        assert!(
            unsafe {
                crate::bound_dispatch::mutate_bound_node(&bindings, &nodes, target, |_| {
                    called.set(true)
                })
            }
            .is_err()
        );
        assert!(!called.get());
        assert_eq!((outer.base_heads(), outer.pending_ops()), outer_before);
        assert_eq!(
            b.read_checked(&checkout.id, crate::state::owned_restore_tests::value)
                .unwrap(),
            "second"
        );
    }
    // phase: frame_legacy_at_alias_is_stable_for_owner_rebuild_and_rejects_replacement
    {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let mut owners_a = crate::record::RecordOwners::new(first_dir.path().to_path_buf());
        let mut owners_b = crate::record::RecordOwners::new(second_dir.path().to_path_buf());
        let a = owners_a.open(RESTORABLE.app_id, "same").unwrap();
        let b = owners_b.open(RESTORABLE.app_id, "same").unwrap();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(514),
            crate::record::RecordKind::new("note"),
        );
        for store in [&a, &b] {
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", "live").unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let mut tile = Box::new(crate::widget::TileHost::new(
            crate::widget::TileContent::App(Box::new(TileEditor(crate::testing::OwnedText::new(
                "body",
                crate::widget::TextInput::local().with_accessible_name("owned editor"),
            )))),
        ));
        tile.bind_record(a.clone(), checkout.clone());
        let pointer = std::ptr::from_mut(tile.as_mut());
        let mut driver = Driver::new(State::new(), tile, (900, 650));
        let old = driver.a11y_node_named("owned editor").unwrap();
        driver.relayout();
        assert_eq!(driver.a11y_node_named("owned editor"), Some(old));
        driver
            .perform_a11y_action(old, &crate::a11y::Action::Focus)
            .unwrap();
        assert_eq!(driver.a11y_focus().unwrap().0, old);
        let text_run = driver
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == accesskit::Role::TextRun)
            .map(|(id, _)| id)
            .unwrap();
        driver
            .perform_a11y_action(
                text_run,
                &crate::a11y::Action::SetSelection {
                    anchor: 1,
                    focus: 3,
                },
            )
            .unwrap();
        // The same boxed root remains live and no widget references are borrowed.
        // Rebinding invalidates the old raw admission before the next frame.
        unsafe {
            (*pointer).bind_record(b.clone(), checkout.clone());
        }
        driver.relayout();
        assert_ne!(driver.a11y_node_named("owned editor"), Some(old));
        let before = b
            .with(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(
            driver
                .perform_a11y_action(old, &crate::a11y::Action::SetValue("stale".into()))
                .is_err()
        );
        assert!(
            driver
                .perform_a11y_action(
                    text_run,
                    &crate::a11y::Action::SetSelection {
                        anchor: 0,
                        focus: 0
                    }
                )
                .is_err()
        );
        assert_eq!(
            b.with(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            before
        );
        let fresh = driver.a11y_node_named("owned editor").unwrap();
        driver
            .perform_a11y_action(fresh, &crate::a11y::Action::SetValue("fresh edit".into()))
            .unwrap();
        assert_ne!(
            b.with(&checkout.id, |doc| doc.base_heads()).unwrap(),
            before.0
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn legacy_restores_use_the_actual_owner_and_preserve_custody() {
    // phase: legacy_tile_review_restore_uses_actual_owner_one_undo_and_current_file_authority
    {
        use crate::permissions::*;
        for case in [
            "app record",
            "file app only",
            "file foreign path",
            "file record grant",
            "file matching path",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
            let store = owners.open(RESTORABLE.app_id, "legacy-restore").unwrap();
            let path = directory.path().join("owned.txt");
            let id = if case == "app record" {
                crate::RecordId::for_tab(910)
            } else {
                crate::RecordId::for_file(&path)
            };
            let checkout =
                crate::record::RecordCheckout::new(id, crate::record::RecordKind::new("note"));
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", "old").unwrap();
                    Ok(())
                })
                .unwrap();
            let selected = store
                .with_history(&checkout.id, |doc, history| {
                    history.commit_action_authored(
                        doc,
                        None,
                        None,
                        Some(crate::collaboration::local_user_id()),
                    );
                    let selected = doc.base_heads()[0];
                    doc.put(ROOT, "body", "live").unwrap();
                    history.commit_action_authored(
                        doc,
                        None,
                        None,
                        Some(crate::collaboration::local_user_id()),
                    );
                    selected
                })
                .unwrap();
            let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
                TileEditor(crate::widget::Text::new("Live notebook")),
            )));
            tile.bind_record(store.clone(), checkout.clone());
            tile.register_document_history(&RESTORABLE).unwrap();
            let mut state = State::new();
            state.doc.put(ROOT, "body", "outer").unwrap();
            state.commit_action(None, None);
            let mut driver = Driver::new(state, Box::new(tile), (900, 650));
            let principal = Subject::Principal(Principal::Person(LOCAL_USER.into()));
            let mut permissions = PermissionContext::granted(
                LOCAL_USER,
                &TreePath::root().app(RESTORABLE.app_id),
                policy::KindSet::ALL,
                None,
            );
            let extra = match case {
                "file matching path" => Some(Scope::FsPath(path.to_string_lossy().into_owned())),
                "file foreign path" => Some(Scope::FsPath(
                    directory
                        .path()
                        .join("foreign.txt")
                        .to_string_lossy()
                        .into_owned(),
                )),
                "file record grant" => Some(Scope::FsRecord("unrelated-collaborative-file".into())),
                _ => None,
            };
            if let Some(scope) = extra {
                permissions = permissions.granting(CapOverride::new(
                    principal,
                    scope,
                    Effect::Grant,
                    Access::Write,
                ));
            }
            driver.set_permission_ctx(permissions);
            let before = store
                .with_history(&checkout.id, |doc, history| {
                    (doc.base_heads(), history.revision())
                })
                .unwrap();
            let outer = driver.state_mut().doc.base_heads();
            driver.click_named("history-open").unwrap();
            let target = format!(
                "timeline-entry:{}",
                crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                    &selected.to_string()
                ))
            );
            driver
                .perform_semantic_named(
                    "activity_timeline",
                    Some(&target),
                    &crate::a11y::Action::Activate,
                )
                .unwrap();
            driver.click_named("history-restore").unwrap();
            let confirm = driver.a11y_node_named("Restore version").unwrap();
            driver
                .perform_a11y_action(confirm, &crate::a11y::Action::Activate)
                .unwrap();
            let allowed = matches!(case, "app record" | "file matching path");
            assert_eq!(
                store
                    .read_checked(&checkout.id, crate::state::owned_restore_tests::value)
                    .unwrap(),
                if allowed { "old" } else { "live" },
                "{case}"
            );
            assert_eq!(
                driver.state_mut().doc.base_heads(),
                outer,
                "{case}: outer isolation"
            );
            if allowed {
                store
                    .with_history(&checkout.id, |doc, history| {
                        assert_eq!(
                            doc.get_changes(&before.0).len(),
                            1,
                            "{case}: one canonical change"
                        );
                        assert_eq!(history.revision(), before.1 + 1);
                        assert_eq!(
                            history.peek_last_undo().unwrap().author,
                            Some(crate::collaboration::local_user_id())
                        );
                        assert!(
                            history
                                .undo_as(
                                    doc,
                                    Some(&crate::collaboration::local_user_id()),
                                    None,
                                    None
                                )
                                .is_some()
                        );
                        assert_eq!(crate::state::owned_restore_tests::value(doc), "live");
                    })
                    .unwrap();
            } else {
                assert_eq!(
                    store
                        .with_history(&checkout.id, |doc, history| (
                            doc.base_heads(),
                            history.revision()
                        ))
                        .unwrap(),
                    before
                );
                assert!(
                    driver
                        .inspection_json()
                        .unwrap()
                        .contains("permission to write this file")
                );
            }
        }
    }
    // phase: legacy_ui_restore_schedules_only_target_and_reopens_durable_result
    {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("records");
        let mut owners = crate::record::RecordOwners::new(profile.clone());
        let store = owners.open(RESTORABLE.app_id, "restore-durable").unwrap();
        let first = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(920),
            crate::record::RecordKind::new("note"),
        );
        let other = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(921),
            crate::record::RecordKind::new("note"),
        );
        for checkout in [&first, &other] {
            store
                .reserve_record(checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", "old").unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let selected = store
            .with_history(&first.id, |doc, history| {
                let old = doc.base_heads()[0];
                doc.put(ROOT, "body", "live").unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(crate::collaboration::local_user_id()),
                );
                old
            })
            .unwrap();
        store.try_flush().unwrap();
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            TileEditor(crate::widget::Text::new("Live")),
        )));
        tile.bind_record(store.clone(), first.clone());
        tile.register_document_history(&RESTORABLE).unwrap();
        let mut driver = Driver::new(State::new(), Box::new(tile), (900, 650));
        store
            .with_action_state(&other.id, |doc, _, _| {
                doc.put(ROOT, "body", "unrelated unfinished edit").unwrap();
            })
            .unwrap();
        let other_before = store
            .read_checked(&other.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        driver.click_named("history-open").unwrap();
        let target = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                &selected.to_string()
            ))
        );
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        driver.click_named("history-restore").unwrap();
        let confirm = driver.a11y_node_named("Restore version").unwrap();
        driver
            .perform_a11y_action(confirm, &crate::a11y::Action::Activate)
            .unwrap();
        assert_eq!(
            store
                .read_checked(&first.id, crate::state::owned_restore_tests::value)
                .unwrap(),
            "old"
        );
        store.await_submitted(&first.id).unwrap();
        assert_eq!(
            store.save_status(&first.id).unwrap(),
            crate::record::RecordSaveStatus::Saved,
            "normal target DocWriter acknowledges restore without sweep/flush"
        );
        assert_eq!(
            store
                .read_checked(&other.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            other_before
        );
        drop(driver);
        drop(store);
        drop(owners);
        let mut reopened_owners = crate::record::RecordOwners::new(profile);
        let reopened = reopened_owners
            .open(RESTORABLE.app_id, "restore-durable")
            .unwrap();
        reopened.try_open_record(&first).unwrap();
        assert_eq!(
            reopened
                .read_checked(&first.id, crate::state::owned_restore_tests::value)
                .unwrap(),
            "old"
        );
    }
    // phase: round1_top_level_frame_history_preserves_legacy_custody_and_stale_at_refusal
    {
        use crate::permissions::*;
        let mut fixture = TopLegacyFixture::new();
        let tile_pointer = std::ptr::from_mut(fixture.tile.as_mut());
        let mut driver = Driver::from_managed_root(
            State::new(),
            ManagedRoot::new(fixture.tile, fixture.registration),
            (900, 650),
        )
        .unwrap();
        let allowed = PermissionContext::permissive();
        driver.set_permission_ctx(allowed.clone());
        driver
            .state_mut()
            .doc
            .put(ROOT, "unfinished outer", "pending")
            .unwrap();
        let outer = (
            driver.state_mut().doc.base_heads(),
            driver.state_mut().doc.pending_ops(),
        );
        let before = fixture
            .store
            .read_history(&fixture.checkout.id, |_, history| history.revision())
            .unwrap();
        let target = driver.a11y_node_named("owned editor").unwrap();
        driver
            .perform_a11y_action(target, &crate::a11y::Action::SetValue("owned edit".into()))
            .unwrap();
        assert_eq!(
            legacy_body(&fixture.store, &fixture.checkout),
            "owned edit",
            "top-level chrome must retain the actual legacy owner"
        );
        assert_eq!(
            (
                driver.state_mut().doc.base_heads(),
                driver.state_mut().doc.pending_ops()
            ),
            outer
        );
        assert_eq!(
            fixture
                .store
                .read_history(&fixture.checkout.id, |_, history| history.revision())
                .unwrap(),
            before + 1
        );
        driver.set_permission_ctx(crate::permissions::PermissionContext::stranger(LOCAL_USER));
        assert!(
            driver
                .perform_a11y_action(target, &crate::a11y::Action::SetValue("denied".into()))
                .is_err()
        );
        assert_eq!(legacy_body(&fixture.store, &fixture.checkout), "owned edit");
        driver.set_permission_ctx(allowed);
        // Exclusive runner owns this retained box, with no borrowed node/widget refs.
        unsafe {
            (*tile_pointer).bind_record(fixture.replacement.clone(), fixture.checkout.clone());
        }
        driver.relayout();
        assert_ne!(driver.a11y_node_named("owned editor"), Some(target));
        assert!(
            driver
                .perform_a11y_action(target, &crate::a11y::Action::SetValue("stale".into()))
                .is_err()
        );
        assert_eq!(legacy_body(&fixture.replacement, &fixture.checkout), "live");
        // Owner replacement invalidates the old registration; install its actual new witness.
        let owner = unsafe { (*tile_pointer).history_owner().unwrap() };
        driver
            .install_history(
                HistoryRegistration::new(
                    &RESTORABLE,
                    vec![
                        HistorySourceRegistration::owned("document", RESTORABLE.app_id, owner)
                            .unwrap(),
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        let fresh = driver.a11y_node_named("owned editor").unwrap();
        assert_ne!(fresh, target);
        assert!(
            driver
                .perform_a11y_action(
                    target,
                    &crate::a11y::Action::SetValue("stale allowed".into())
                )
                .is_err()
        );
        let replacement_revision = fixture
            .replacement
            .read_history(&fixture.checkout.id, |_, history| history.revision())
            .unwrap();
        driver
            .perform_a11y_action(
                fresh,
                &crate::a11y::Action::SetValue("replacement edit".into()),
            )
            .unwrap();
        assert_eq!(
            legacy_body(&fixture.replacement, &fixture.checkout),
            "replacement edit"
        );
        assert_eq!(legacy_body(&fixture.store, &fixture.checkout), "owned edit");
        assert_eq!(
            fixture
                .replacement
                .read_history(&fixture.checkout.id, |_, history| history.revision())
                .unwrap(),
            replacement_revision + 1
        );
        assert_eq!(
            (
                driver.state_mut().doc.base_heads(),
                driver.state_mut().doc.pending_ops()
            ),
            outer
        );
    }
}

pub(crate) struct TopLegacyFixture {
    pub(crate) store: crate::RecordStore,
    pub(crate) replacement: crate::RecordStore,
    pub(crate) checkout: crate::record::RecordCheckout,
    pub(crate) tile: Box<crate::widget::TileHost>,
    pub(crate) registration: HistoryRegistration,
    _owners: [crate::record::RecordOwners; 2],
    _directories: [tempfile::TempDir; 2],
}
impl TopLegacyFixture {
    pub(crate) fn new() -> Self {
        let directories = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let mut owners = directories
            .each_ref()
            .map(|directory| crate::record::RecordOwners::new(directory.path().to_path_buf()));
        let store = owners[0].open(RESTORABLE.app_id, "top-level").unwrap();
        let replacement = owners[1].open(RESTORABLE.app_id, "top-level").unwrap();
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(813),
            crate::record::RecordKind::new("note"),
        );
        for record in [&store, &replacement] {
            record
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|doc| {
                    doc.put(ROOT, "body", "live").unwrap();
                    Ok(())
                })
                .unwrap();
        }
        let mut tile = Box::new(crate::widget::TileHost::new(
            crate::widget::TileContent::App(Box::new(TileEditor(crate::testing::OwnedText::new(
                "body",
                crate::widget::TextInput::local()
                    .with_id("owned-editor")
                    .with_accessible_name("owned editor"),
            )))),
        ));
        tile.bind_record(store.clone(), checkout.clone());
        let source = HistorySourceRegistration::owned(
            "document",
            RESTORABLE.app_id,
            tile.history_owner().unwrap(),
        )
        .unwrap();
        let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
        Self {
            store,
            replacement,
            checkout,
            tile,
            registration,
            _owners: owners,
            _directories: directories,
        }
    }
}
pub(crate) fn legacy_body(
    store: &crate::RecordStore,
    checkout: &crate::record::RecordCheckout,
) -> String {
    use automerge::ReadDoc;
    store
        .read_checked(&checkout.id, |doc| {
            let (value, object) = doc.get(ROOT, "body").unwrap().unwrap();
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| doc.text(object).unwrap())
        })
        .unwrap()
}

thread_local! {
    static AUTOMATIC_MEDIA: std::cell::RefCell<Option<std::rc::Rc<DeferredMedia>>> = const { std::cell::RefCell::new(None) };
}
fn automatic_media_source() -> std::rc::Rc<dyn HistoricalAssetSource> {
    let source = std::rc::Rc::new(DeferredMedia::default());
    AUTOMATIC_MEDIA.with(|slot| *slot.borrow_mut() = Some(source.clone()));
    source
}
static AUTOMATIC_MEDIA_DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Recorded media",
        content_schema: None,
        presentation: HistoryPresentation::DomainWithAssets {
            create: super::host_tests::preview,
            assets: automatic_media_source,
        },
    }],
};
fn check_automatic_media(mut driver: Driver, selected: automerge::ChangeHash) {
    use automerge::ReadDoc;
    let source = AUTOMATIC_MEDIA.with(|slot| slot.borrow_mut().take().unwrap());
    assert!(
        source.0.borrow().is_empty(),
        "factory never reads before a selected request"
    );
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    let old = source.0.borrow_mut().drain(..).collect::<Vec<_>>();
    assert_eq!(old.len(), 2);
    let after = old
        .iter()
        .find(|(r, _)| r.snapshot.side() == PreviewSide::After)
        .unwrap();
    let snapshot = automerge::AutoCommit::load(after.0.snapshot.bytes()).unwrap();
    assert_eq!(
        snapshot.get(ROOT, "body").unwrap().unwrap().0.as_str(),
        Some("selected media")
    );
    assert_eq!(after.0.resource.app_id, APP);
    driver.click_named("history-back-live").unwrap();
    driver.click_named("history-open").unwrap();
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    for (_, response) in old {
        response.complete(Err("late automatic provider response".into()));
    }
    driver.key("Shift");
    assert!(!format!("{:?}", driver.a11y_nodes()).contains("late automatic provider response"));
    let current = source.0.borrow_mut().drain(..).collect::<Vec<_>>();
    assert_eq!(current.len(), 2);
    for (_, response) in current {
        response.complete(Ok(Default::default()));
    }
    driver.key("Shift");
    assert!(!format!("{:?}", driver.a11y_nodes()).contains("Loading historical media"));
}

const RETAINED_LIFECYCLE_OPERATION: &str = "fixture.retained-lifecycle";

#[derive(Clone)]
struct RetainedLifecycleHandles {
    emitter: std::rc::Rc<std::cell::RefCell<crate::widget::ActionEmitter<()>>>,
    queued: std::rc::Rc<std::cell::RefCell<Option<QueuedLifecycleResult>>>,
    admitted: std::rc::Rc<std::cell::Cell<Option<tilo_actions::InvocationId>>>,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
    accepts: std::rc::Rc<std::cell::Cell<usize>>,
    completions: std::rc::Rc<std::cell::Cell<usize>>,
    polls: std::rc::Rc<std::cell::Cell<usize>>,
    syncs: std::rc::Rc<std::cell::Cell<usize>>,
    updates: std::rc::Rc<std::cell::Cell<usize>>,
    events: std::rc::Rc<std::cell::Cell<usize>>,
    event_value: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    wrong_owner: std::rc::Rc<std::cell::Cell<usize>>,
    intent: std::rc::Rc<std::cell::RefCell<Option<crate::record::RecordCheckout>>>,
    last_owner: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    accepted: Accepted,
}

/// The results an owner accepted, each with the owner of the document it was
/// handed when it did.
type Accepted = std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>;

/// `(owner, value)` for each result accepted so far.
fn accepted(accepted: &Accepted) -> Vec<(String, String)> {
    accepted.borrow().clone()
}

/// One accepted result: `value` under `owner`.
fn one(owner: &str, value: &str) -> Vec<(String, String)> {
    vec![(owner.to_owned(), value.to_owned())]
}

struct QueuedLifecycleResult {
    invocation: tilo_actions::InvocationId,
    value: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the fixture tracks independent lifecycle and owner-policy states"
)]
struct RetainedLifecycleProbe {
    id: &'static str,
    expected_owner: &'static str,
    enforce_result_owner: bool,
    streaming: bool,
    text: crate::widget::Text,
    handles: RetainedLifecycleHandles,
    ready: bool,
    observed: String,
}

fn increment(counter: &std::cell::Cell<usize>) {
    counter.set(counter.get().checked_add(1).unwrap());
}

/// A record store for `workspace` in a fresh directory. Keep all three alive.
fn lifecycle_store(
    workspace: &str,
) -> (
    tempfile::TempDir,
    crate::record::RecordOwners,
    crate::record::RecordStore,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners.open(RESTORABLE.app_id, workspace).unwrap();
    (directory, owners, store)
}

fn note(tab: u64) -> crate::record::RecordCheckout {
    crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(tab),
        crate::record::RecordKind::new("note"),
    )
}

fn seed_note(
    store: &crate::record::RecordStore,
    checkout: &crate::record::RecordCheckout,
    owner: &str,
    body: &str,
) {
    store
        .reserve_record(checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "owner", owner).unwrap();
            doc.put(ROOT, "body", body).unwrap();
            Ok(())
        })
        .unwrap();
}

fn bound_tile(
    root: impl crate::widget::AppWidget + 'static,
    store: &crate::record::RecordStore,
    checkout: &crate::record::RecordCheckout,
) -> crate::widget::TileHost {
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(root)));
    tile.bind_record(store.clone(), checkout.clone());
    tile
}

fn stored_body(
    store: &crate::record::RecordStore,
    checkout: &crate::record::RecordCheckout,
) -> String {
    store
        .read_checked(&checkout.id, crate::state::owned_restore_tests::value)
        .unwrap()
}

/// Admits one retained-lifecycle operation and asserts the owner accepted it.
fn admit(
    tile: &mut crate::widget::TileHost,
    invocation: tilo_actions::InvocationId,
    doc: &mut automerge::AutoCommit,
) {
    assert!(matches!(
        tile.start_operation(
            &crate::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                RETAINED_LIFECYCLE_OPERATION,
                &[]
            ),
            &mut crate::widget::Edits::lend(doc),
            &ROOT
        )
        .unwrap(),
        Some(crate::widget::ResourceOperationStart::Accepted)
    ));
}

fn retained_lifecycle_resource(id: &str) -> tilo_actions::Resource {
    tilo_actions::Resource::new(id, "fixture", "Retained lifecycle").with_action(
        tilo_actions::ActionSpec::operation(
            "receive",
            "Receive queued result",
            tilo_actions::OperationKind::Effect,
            RETAINED_LIFECYCLE_OPERATION,
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job),
    )
}

fn retained_lifecycle_probe(
    id: &'static str,
    expected_owner: &'static str,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    retained_lifecycle_probe_with_owner_policy(id, expected_owner, true, false)
}

fn unfenced_retained_lifecycle_probe(
    id: &'static str,
    admission_owner: &'static str,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    retained_lifecycle_probe_with_owner_policy(id, admission_owner, false, false)
}

fn streaming_unfenced_retained_lifecycle_probe(
    id: &'static str,
    admission_owner: &'static str,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    retained_lifecycle_probe_with_owner_policy(id, admission_owner, false, true)
}

fn retained_lifecycle_probe_with_owner_policy(
    id: &'static str,
    expected_owner: &'static str,
    enforce_result_owner: bool,
    streaming: bool,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    let emitter = std::rc::Rc::new(std::cell::RefCell::new(crate::widget::ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&retained_lifecycle_resource(id), "receive").unwrap(),
    )));
    let handles = RetainedLifecycleHandles {
        emitter,
        queued: std::rc::Rc::new(std::cell::RefCell::new(None)),
        admitted: std::rc::Rc::new(std::cell::Cell::new(None)),
        starts: std::rc::Rc::new(std::cell::Cell::new(0)),
        accepts: std::rc::Rc::new(std::cell::Cell::new(0)),
        completions: std::rc::Rc::new(std::cell::Cell::new(0)),
        polls: std::rc::Rc::new(std::cell::Cell::new(0)),
        syncs: std::rc::Rc::new(std::cell::Cell::new(0)),
        updates: std::rc::Rc::new(std::cell::Cell::new(0)),
        events: std::rc::Rc::new(std::cell::Cell::new(0)),
        event_value: std::rc::Rc::new(std::cell::RefCell::new(None)),
        wrong_owner: std::rc::Rc::new(std::cell::Cell::new(0)),
        intent: std::rc::Rc::new(std::cell::RefCell::new(None)),
        last_owner: std::rc::Rc::new(std::cell::RefCell::new(None)),
        accepted: Accepted::default(),
    };
    (
        RetainedLifecycleProbe {
            id,
            expected_owner,
            enforce_result_owner,
            streaming,
            text: crate::widget::Text::new("Retained lifecycle"),
            handles: handles.clone(),
            ready: false,
            observed: String::new(),
        },
        handles,
    )
}

crate::testing::test_app_widget!(
    RetainedLifecycleProbe,
    "Retained lifecycle",
    icon = crate::widget::IconDescriptor::Glyph("R")
);

impl Widget for RetainedLifecycleProbe {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::record::capability::RecordScoped
    );

    crate::testing::delegate_widget_methods!(text: apply_layout, draw, get_rect);
    fn name(&self) -> &'static str {
        "RetainedLifecycleProbe"
    }
    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        use automerge::ReadDoc;
        let (doc, path) = crate::doc::document_ref(doc, path);
        increment(&self.handles.syncs);
        self.observed = self
            .handles
            .accepted
            .borrow()
            .last()
            .map(|(_, value)| value.clone())
            .or_else(|| {
                doc.get(path, "body")
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned))
            })
            .unwrap_or_default();
        let Some(result) = self.handles.queued.borrow_mut().take() else {
            return;
        };
        let actual_owner = doc
            .get(path, "owner")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        self.handles.last_owner.replace(actual_owner.clone());
        if (self.enforce_result_owner && actual_owner.as_deref() != Some(self.expected_owner))
            || self.handles.admitted.get() != Some(result.invocation)
        {
            increment(&self.handles.wrong_owner);
            *self.handles.queued.borrow_mut() = Some(result);
            return;
        }
        self.handles
            .accepted
            .borrow_mut()
            .push((actual_owner.unwrap_or_default(), result.value.clone()));
        self.observed = result.value;
        increment(&self.handles.accepts);
        self.ready = true;
    }
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        increment(&self.handles.updates);
        false
    }
    fn has_separate_document_owner(&self) -> bool {
        false
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                .with_name(self.id)
                .with_value(self.observed.clone()),
        )
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        increment(&self.handles.events);
        if let Some(value) = self.handles.event_value.borrow().as_deref() {
            ctx.state
                .document_mut()
                .put(ctx.path, "body", value)
                .unwrap();
        }
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[RETAINED_LIFECYCLE_OPERATION]
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        use automerge::ReadDoc;
        let Some(crate::host_operation::OperationRequest {
            invocation,
            type_id,
            ..
        }) = op.unscoped()
        else {
            return Ok(None);
        };
        if type_id != RETAINED_LIFECYCLE_OPERATION {
            return Ok(None);
        }
        let owner = doc
            .get(path, "owner")
            .map_err(|error| error.to_string())?
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        if owner.as_deref() != Some(self.expected_owner) {
            return Err("operation admitted against the wrong document owner".into());
        }
        increment(&self.handles.starts);
        self.handles.admitted.set(Some(invocation));
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !self.ready || self.handles.admitted.get() != Some(invocation) {
            return None;
        }
        increment(&self.handles.polls);
        if self.streaming {
            return Some(tilo_actions::InvocationUpdate::Item {
                invocation,
                sequence: u64::try_from(self.handles.polls.get()).unwrap(),
                value: serde_json::json!(self.handles.polls.get()),
            });
        }
        self.ready = false;
        self.handles.admitted.set(None);
        increment(&self.handles.completions);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::record::capability::RecordScoped for RetainedLifecycleProbe {
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.handles.intent.borrow().clone()
    }
}

impl crate::resources::PublishesResources for RetainedLifecycleProbe {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![retained_lifecycle_resource(self.id)]
    }
}

impl crate::widget::controls::ActionControls for RetainedLifecycleProbe {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.handles.emitter.borrow_mut().take()
    }
}

#[derive(Clone)]
struct ReleasableLifecycleHandles {
    sender: std::rc::Rc<std::cell::RefCell<std::sync::mpsc::Sender<QueuedLifecycleResult>>>,
    intent: std::rc::Rc<std::cell::RefCell<Option<crate::record::RecordCheckout>>>,
    admitted: std::rc::Rc<std::cell::Cell<Option<tilo_actions::InvocationId>>>,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
    releases: std::rc::Rc<std::cell::Cell<usize>>,
    refuse_release: std::rc::Rc<std::cell::Cell<bool>>,
    accepts: std::rc::Rc<std::cell::Cell<usize>>,
    completions: std::rc::Rc<std::cell::Cell<usize>>,
    last_owner: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    accepted: Accepted,
    terminal_update: std::rc::Rc<std::cell::RefCell<Option<tilo_actions::InvocationUpdate>>>,
}

struct ReleasableLifecycleProbe {
    text: crate::widget::Text,
    receiver: Option<std::sync::mpsc::Receiver<QueuedLifecycleResult>>,
    handles: ReleasableLifecycleHandles,
    ready: bool,
}

fn releasable_lifecycle_probe() -> (ReleasableLifecycleProbe, ReleasableLifecycleHandles) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let handles = ReleasableLifecycleHandles {
        sender: std::rc::Rc::new(std::cell::RefCell::new(sender)),
        intent: std::rc::Rc::new(std::cell::RefCell::new(None)),
        admitted: std::rc::Rc::new(std::cell::Cell::new(None)),
        starts: std::rc::Rc::new(std::cell::Cell::new(0)),
        releases: std::rc::Rc::new(std::cell::Cell::new(0)),
        refuse_release: std::rc::Rc::new(std::cell::Cell::new(false)),
        accepts: std::rc::Rc::new(std::cell::Cell::new(0)),
        completions: std::rc::Rc::new(std::cell::Cell::new(0)),
        last_owner: std::rc::Rc::new(std::cell::RefCell::new(None)),
        accepted: Accepted::default(),
        terminal_update: std::rc::Rc::new(std::cell::RefCell::new(None)),
    };
    (
        ReleasableLifecycleProbe {
            text: crate::widget::Text::new("Releasable lifecycle"),
            receiver: Some(receiver),
            handles: handles.clone(),
            ready: false,
        },
        handles,
    )
}

crate::testing::test_app_widget!(
    ReleasableLifecycleProbe,
    "Releasable lifecycle",
    icon = crate::widget::IconDescriptor::Glyph("R")
);

impl Widget for ReleasableLifecycleProbe {
    crate::provides!(dyn crate::record::capability::RecordScoped);

    crate::testing::delegate_widget_methods!(text: apply_layout, draw, get_rect);
    fn name(&self) -> &'static str {
        "ReleasableLifecycleProbe"
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        use automerge::ReadDoc;
        let (doc, path) = crate::doc::document_ref(doc, path);
        let owner = doc
            .get(path, "owner")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        self.handles.last_owner.replace(owner.clone());
        let result = self
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = result else { return };
        if self.handles.admitted.get() != Some(result.invocation) {
            return;
        }
        self.handles
            .accepted
            .borrow_mut()
            .push((owner.unwrap_or_default(), result.value));
        self.ready = true;
        increment(&self.handles.accepts);
    }
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        false
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::host_operation::OperationRequest {
            invocation,
            type_id,
            ..
        }) = op.unscoped()
        else {
            return Ok(None);
        };
        if type_id != RETAINED_LIFECYCLE_OPERATION {
            return Ok(None);
        }
        self.handles.admitted.set(Some(invocation));
        increment(&self.handles.starts);
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.handles.admitted.get() != Some(invocation) {
            return None;
        }
        if let Some(update) = self.handles.terminal_update.borrow_mut().take() {
            if update.is_terminal() {
                self.handles.admitted.set(None);
                increment(&self.handles.completions);
            }
            return Some(update);
        }
        if !self.ready {
            return None;
        }
        self.ready = false;
        self.handles.admitted.set(None);
        increment(&self.handles.completions);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::record::capability::RecordScoped for ReleasableLifecycleProbe {
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.handles.intent.borrow().clone()
    }
    fn release_record_scope(&mut self) -> Result<(), String> {
        if self.handles.refuse_release.get() {
            return Err("draft release temporarily refused".into());
        }
        self.receiver = None;
        self.handles.admitted.set(None);
        let (sender, receiver) = std::sync::mpsc::channel();
        *self.handles.sender.borrow_mut() = sender;
        self.receiver = Some(receiver);
        increment(&self.handles.releases);
        Ok(())
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The test observes hidden-owner lifecycle and exactly-once settlement across one retained session."
)]
fn hidden_legacy_tile_continues_exact_owner_lifecycle_and_settles_once() {
    let (_directory, _owners, store) = lifecycle_store("retained-lifecycle");
    let first = note(711);
    let second = note(712);
    seed_note(&store, &first, "first", "historical");
    let selected = store
        .read_checked(&first.id, |doc| doc.base_heads()[0])
        .unwrap();
    store
        .with_history(&first.id, |doc, history| {
            doc.put(ROOT, "body", "live before picker").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(crate::collaboration::local_user_id()),
            );
        })
        .unwrap();
    seed_note(&store, &second, "second", "sibling");

    let (first_probe, first_handles) = retained_lifecycle_probe("first-lifecycle", "first");
    let (second_probe, second_handles) = retained_lifecycle_probe("second-lifecycle", "second");
    let mut first_tile = bound_tile(
        TileEditor(crate::widget::Column::new().push(Box::new(first_probe))),
        &store,
        &first,
    );
    first_tile.register_document_history(&RESTORABLE).unwrap();
    let second_tile = bound_tile(
        TileEditor(crate::widget::Column::new().push(Box::new(second_probe))),
        &store,
        &second,
    );
    let root = crate::widget::Row::new()
        .no_wrap()
        .push(Box::new(first_tile))
        .push(Box::new(second_tile));
    let mut outer = State::new();
    outer.doc.put(ROOT, "owner", "outer").unwrap();
    outer.doc.put(ROOT, "body", "outer").unwrap();
    outer.commit_action(None, None);
    let mut driver = Driver::new(outer, Box::new(root), (1200, 650));

    first_handles.emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(first_handles.starts.get(), 1);
    let invocation = first_handles
        .admitted
        .get()
        .expect("the actual host door retains the admitted invocation identity");
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Notebook: historical")
    );
    let first_syncs = first_handles.syncs.get();
    let first_updates = first_handles.updates.get();
    let sibling_syncs = second_handles.syncs.get();
    let sibling_updates = second_handles.updates.get();
    let hidden_events = first_handles.events.get();
    *first_handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation,
        value: "picker result".into(),
    });
    first_handles.emitter.borrow_mut().emit(()).unwrap();

    driver.key("x");
    assert_eq!(first_handles.syncs.get(), first_syncs);
    assert_eq!(first_handles.updates.get(), first_updates);
    driver.relayout();
    driver.relayout();
    driver.key("Shift");
    driver.relayout();

    assert_eq!(
        accepted(&first_handles.accepted),
        one("first", "picker result"),
        "the queued result must be accepted by the retained live root, on its owner"
    );
    assert_eq!(first_handles.accepts.get(), 1);
    assert_eq!(first_handles.completions.get(), 1);
    assert!(first_handles.syncs.get() > first_syncs);
    assert!(first_handles.updates.get() > first_updates);
    assert!(second_handles.syncs.get() > sibling_syncs);
    assert!(second_handles.updates.get() > sibling_updates);
    assert_eq!(first_handles.starts.get(), 1);
    assert_eq!(first_handles.wrong_owner.get(), 0);
    assert!(first_handles.queued.borrow().is_none());
    assert_eq!(first_handles.events.get(), hidden_events);
    assert!(driver.click_named("first-lifecycle").is_err());
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Notebook: historical"),
        "the selected immutable preview remains pinned while live lifecycle advances"
    );
    assert_eq!(stored_body(&store, &second), "sibling");
    assert_eq!(
        crate::state::owned_restore_tests::value(&driver.state_mut().doc),
        "outer"
    );

    let deadline = tilo_test_env::hang_deadline();
    while store.save_status(&first.id).unwrap() != crate::record::RecordSaveStatus::Saved
        && crate::time::now() < deadline
    {
        driver.relayout();
        std::thread::sleep(crate::time::Duration::from_millis(10));
    }
    assert_eq!(
        store.save_status(&first.id).unwrap(),
        crate::record::RecordSaveStatus::Saved
    );
    driver.click_named("history-back-live").unwrap();
    assert!(driver.inspection_json().unwrap().contains("picker result"));
    driver.key("y");
    assert_eq!(first_handles.starts.get(), 1);
    assert_eq!(first_handles.accepts.get(), 1);
    assert_eq!(first_handles.completions.get(), 1);
    assert!(first_handles.events.get() > hidden_events);
}

#[test]
fn nested_hidden_tile_and_component_sibling_keep_single_lifecycle_cadence() {
    let component_host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = component_host
        .prepare_with_permissions(definition, 715, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();

    let (_directory, _owners, store) = lifecycle_store("nested-lifecycle");
    let nested = note(716);
    seed_note(&store, &nested, "nested", "nested");

    let (nested_probe, nested_handles) = retained_lifecycle_probe("nested", "nested");
    let mut nested_tile = bound_tile(nested_probe, &store, &nested);
    nested_tile.register_document_history(&RESTORABLE).unwrap();
    let enclosing_tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        TileEditor(crate::widget::Column::new().push(Box::new(nested_tile))),
    )));

    let (component_probe, component_handles) = retained_lifecycle_probe("component", "component");
    component.root = Some(Box::new(component_probe));
    let component_tile = crate::widget::TileHost::from_component(component);
    let root = crate::widget::Row::new()
        .no_wrap()
        .push(Box::new(enclosing_tile))
        .push(Box::new(component_tile));
    let mut outer = State::new();
    outer.doc.put(ROOT, "body", "outer").unwrap();
    outer.commit_action(None, None);
    let mut driver = Driver::new(outer, Box::new(root), (1200, 650));

    let nested_live = (nested_handles.updates.get(), nested_handles.syncs.get());
    let component_live = (
        component_handles.updates.get(),
        component_handles.syncs.get(),
    );
    driver.relayout();
    let live_delta = (
        nested_handles.updates.get() - nested_live.0,
        nested_handles.syncs.get() - nested_live.1,
    );
    let component_live_delta = (
        component_handles.updates.get() - component_live.0,
        component_handles.syncs.get() - component_live.1,
    );
    assert_eq!(live_delta.0, 1);
    assert!(live_delta.1 > 0);
    assert_eq!(component_live_delta.0, 1);
    assert!(component_live_delta.1 > 0);

    driver.click_named("history-open").unwrap();
    let nested_hidden = (nested_handles.updates.get(), nested_handles.syncs.get());
    let component_hidden = (
        component_handles.updates.get(),
        component_handles.syncs.get(),
    );
    driver.relayout();
    assert_eq!(
        (
            nested_handles.updates.get() - nested_hidden.0,
            nested_handles.syncs.get() - nested_hidden.1,
        ),
        live_delta,
        "the nested TileHost keeps its exact live cadence while its own History is open"
    );
    assert_eq!(
        (
            component_handles.updates.get() - component_hidden.0,
            component_handles.syncs.get() - component_hidden.1,
        ),
        component_live_delta,
        "the independent canonical component keeps its exact cadence"
    );
    assert_eq!(stored_body(&store, &nested), "nested");
    assert_eq!(
        crate::state::owned_restore_tests::value(&driver.state_mut().doc),
        "outer"
    );
}

#[test]
fn hidden_canonical_component_updates_once_per_ordinary_host_pass() {
    let host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 717, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let (probe, handles) = retained_lifecycle_probe("canonical-component", "component");
    component.root = Some(Box::new(probe));
    component.register_document_history(&BOUND).unwrap();
    let root = crate::widget::TileHost::from_component(component);
    let mut driver = Driver::new(State::new(), Box::new(root), (900, 650));

    let live = (handles.updates.get(), handles.syncs.get());
    driver.relayout();
    let live_delta = (handles.updates.get() - live.0, handles.syncs.get() - live.1);
    assert_eq!(live_delta.0, 1);
    assert!(live_delta.1 > 0);

    driver.click_named("history-open").unwrap();
    let hidden = (handles.updates.get(), handles.syncs.get());
    driver.relayout();
    assert_eq!(handles.updates.get() - hidden.0, 1);
    assert_eq!(handles.syncs.get() - hidden.1, live_delta.1);
}

#[test]
fn framework_input_frame_still_suppresses_tile_lifecycle() {
    let (_directory, _owners, store) = lifecycle_store("framework-input-frame");
    let checkout = note(713);
    seed_note(&store, &checkout, "guarded", "guarded");
    let (probe, handles) = retained_lifecycle_probe("guarded-lifecycle", "guarded");
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        TileEditor(crate::widget::Column::new().push(Box::new(probe))),
    )));
    tile.bind_record(store, checkout);
    let outer = automerge::AutoCommit::new();
    let flag = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let _frame = crate::history_mode::frame::FrameworkInputFrame::new(&flag, true);
        assert!(!tile.update(&outer, &ROOT));
        tile.sync(&outer, &ROOT);
    }
    assert_eq!(handles.updates.get(), 0);
    assert_eq!(handles.syncs.get(), 0);
    assert!(!tile.update(&outer, &ROOT));
    tile.sync(&outer, &ROOT);
    assert_eq!(handles.updates.get(), 1);
    assert_eq!(handles.syncs.get(), 1);
}

#[test]
fn closed_hidden_owner_never_receives_queued_lifecycle_result() {
    let (_directory, _owners, store) = lifecycle_store("closed-hidden-lifecycle");
    let checkout = note(718);
    seed_note(&store, &checkout, "closed", "before close");
    let (probe, handles) = retained_lifecycle_probe("closed-lifecycle", "closed");
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(Box::new(probe)),
    ));
    tile.bind_record(store.clone(), checkout.clone());
    tile.register_document_history(&RESTORABLE).unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let mut outer = automerge::AutoCommit::new();
    admit(&mut tile, invocation, &mut outer);
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let mut driver = Driver::new(State::new(), tile, (900, 650));
    driver.click_named("history-open").unwrap();
    *handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation,
        value: "must not land after close".into(),
    });
    let syncs = handles.syncs.get();
    let updates = handles.updates.get();

    // SAFETY: the driver exclusively owns this retained box and no node/widget
    // references are borrowed across the close or following lifecycle pass.
    let _ = unsafe { crate::component_lifecycle::close_components(&mut (*tile_pointer)) };
    driver.relayout();

    assert!(
        accepted(&handles.accepted).is_empty(),
        "a closed retained root must never receive its former owner's document"
    );
    assert_eq!(handles.accepts.get(), 0);
    assert_eq!(handles.syncs.get(), syncs);
    assert_eq!(handles.updates.get(), updates);
    assert!(handles.queued.borrow().is_some());
    let update = unsafe {
        (*tile_pointer)
            .poll_resource_operation(invocation, false, &automerge::AutoCommit::new(), &ROOT)
            .unwrap()
    };
    assert!(matches!(
        update,
        tilo_actions::InvocationUpdate::Failed { invocation: failed, ref error }
            if failed == invocation && error.code == "owner_unavailable"
    ));
}

#[test]
fn rebound_owner_cannot_inherit_retained_result_or_invocation() {
    let first_directory = tempfile::tempdir().unwrap();
    let second_directory = tempfile::tempdir().unwrap();
    let mut first_owners = crate::record::RecordOwners::new(first_directory.path().join("records"));
    let mut second_owners =
        crate::record::RecordOwners::new(second_directory.path().join("records"));
    let first = first_owners
        .open(RESTORABLE.app_id, "retained-rebind")
        .unwrap();
    let second = second_owners
        .open(RESTORABLE.app_id, "retained-rebind")
        .unwrap();
    let checkout = note(714);
    for (store, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(store, &checkout, owner, owner);
    }
    let (probe, handles) = unfenced_retained_lifecycle_probe("rebound-lifecycle", "first");
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(Box::new(probe)),
    ));
    tile.bind_record(first.clone(), checkout.clone());
    tile.register_document_history(&RESTORABLE).unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let mut state = State::new();
    state.doc.put(ROOT, "owner", "outer").unwrap();
    state.doc.put(ROOT, "body", "outer").unwrap();
    state.commit_action(None, None);
    admit(&mut tile, invocation, &mut state.doc);
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let mut driver = Driver::new(state, tile, (900, 650));
    driver.click_named("history-open").unwrap();
    *handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation,
        value: "must stay with first".into(),
    });

    // SAFETY: the driver exclusively owns this retained box and no node/widget
    // references are borrowed across the rebind or following lifecycle pass.
    unsafe { (*tile_pointer).bind_record(second.clone(), checkout.clone()) };
    assert!(unsafe { (*tile_pointer).record_transition_error().is_some() });
    driver.relayout();
    assert!(
        unsafe { (*tile_pointer).record_transition_error().is_some() },
        "an unrelated lifecycle tick must not erase an explicit release refusal"
    );

    assert_eq!(
        stored_body(&second, &checkout),
        "second",
        "the replacement owner must never be loaned to opaque work admitted by the old owner"
    );
    assert!(
        accepted(&handles.accepted)
            .iter()
            .all(|(owner, _)| owner == "first"),
        "every queued result stays with the owner that admitted it"
    );
    let update = unsafe {
        (*tile_pointer)
            .poll_resource_operation(invocation, false, &driver.state_mut().doc, &ROOT)
            .expect("the retained operation can complete under the still-current old owner")
    };

    assert!(matches!(
        update,
        tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. }
            if completed == invocation
    ));
    assert_eq!(handles.accepts.get(), 1);
    assert_eq!(handles.completions.get(), 1);
    assert_eq!(handles.wrong_owner.get(), 0);
    assert!(handles.queued.borrow().is_none());
    assert_eq!(
        accepted(&handles.accepted),
        one("first", "must stay with first")
    );
    assert_eq!(
        crate::state::owned_restore_tests::value(&driver.state_mut().doc),
        "outer"
    );
}

#[test]
fn history_open_record_intent_cannot_loan_new_owner_to_unfenced_retained_result() {
    let (_directory, _owners, store) = lifecycle_store("retained-record-intent");
    let first = note(719);
    let second = note(720);
    for (checkout, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(&store, checkout, owner, owner);
    }
    let (probe, handles) = unfenced_retained_lifecycle_probe("intent-lifecycle", "first");
    let mut tile = bound_tile(probe, &store, &first);
    tile.register_document_history(&RESTORABLE).unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let mut outer = automerge::AutoCommit::new();
    admit(&mut tile, invocation, &mut outer);
    let mut tile = Box::new(tile);
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let mut driver = Driver::new(State::new(), tile, (900, 650));
    driver.click_named("history-open").unwrap();
    *handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation,
        value: "must remain with first".into(),
    });
    handles.intent.replace(Some(second.clone()));

    driver.relayout();

    assert!(
        accepted(&handles.accepted).is_empty(),
        "an ordinary record intent must not expose its new owner to opaque old-owner work"
    );
    assert!(unsafe { (*tile_pointer).record_transition_error().is_some() });
    assert_eq!(handles.accepts.get(), 0);
    assert!(handles.queued.borrow().is_some());
}

#[test]
fn live_record_intent_without_owner_bound_work_still_switches_normally() {
    let (_directory, _owners, store) = lifecycle_store("ordinary-record-intent");
    let first = note(721);
    let second = note(722);
    for (checkout, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(&store, checkout, owner, owner);
    }
    let (probe, handles) = retained_lifecycle_probe("ordinary-intent", "first");
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    tile.bind_record(store, first);
    let mut driver = Driver::new(State::new(), Box::new(tile), (900, 650));

    handles.intent.replace(Some(second));
    *handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation: tilo_actions::InvocationId::new(),
        value: "observe owner only".into(),
    });
    driver.relayout();

    assert_eq!(handles.last_owner.borrow().as_deref(), Some("second"));
}

#[test]
fn refused_transition_does_not_repoll_an_undrained_stream_update() {
    let (_directory, _owners, store) = lifecycle_store("bounded-release-poll");
    let first = note(727);
    let second = note(728);
    for (checkout, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(&store, checkout, owner, owner);
    }
    let (probe, handles) =
        streaming_unfenced_retained_lifecycle_probe("streaming-release", "first");
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    tile.bind_record(store, first);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    *handles.queued.borrow_mut() = Some(QueuedLifecycleResult {
        invocation,
        value: "stream ready".into(),
    });
    tile.sync(&outer, &ROOT);
    handles.intent.replace(Some(second));
    for _ in 0..5 {
        tile.sync(&outer, &ROOT);
        tile.update(&outer, &ROOT);
    }
    assert_eq!(handles.polls.get(), 1);
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Item { sequence: 1, .. })
    ));
    tile.sync(&outer, &ROOT);
    assert_eq!(handles.polls.get(), 2);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The test keeps old-owner release and new-owner admission in one ordered lifecycle."
)]
fn direct_admission_releases_old_receiver_before_new_owner_and_accepts_fresh_work() {
    let (_directory, _owners, store) = lifecycle_store("release-direct");
    let first = note(723);
    let second = note(724);
    for (checkout, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(&store, checkout, owner, owner);
    }
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &store, &first);
    let mut outer = automerge::AutoCommit::new();
    tile.sync(&outer, &ROOT);
    let old_invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, old_invocation, &mut outer);
    let old_sender = handles.sender.borrow().clone();
    old_sender
        .send(QueuedLifecycleResult {
            invocation: old_invocation,
            value: "completed first result".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);
    handles.intent.replace(Some(second.clone()));

    let fresh_invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, fresh_invocation, &mut outer);
    assert_eq!(handles.releases.get(), 1);
    assert_eq!(
        accepted(&handles.accepted),
        one("first", "completed first result"),
        "the old owner took its result before it was released"
    );
    assert!(
        old_sender
            .send(QueuedLifecycleResult {
                invocation: old_invocation,
                value: "stale old result".into(),
            })
            .is_err()
    );
    for _ in 0..3 {
        tile.sync(&outer, &ROOT);
        tile.update(&outer, &ROOT);
    }
    assert_eq!(accepted(&handles.accepted).len(), 1);
    let old_update = tile
        .poll_resource_operation(old_invocation, false, &outer, &ROOT)
        .unwrap();
    assert!(matches!(
        old_update,
        tilo_actions::InvocationUpdate::Succeeded { invocation, .. }
            if invocation == old_invocation
    ));
    assert!(
        tile.poll_resource_operation(old_invocation, false, &outer, &ROOT)
            .is_none()
    );

    handles
        .sender
        .borrow()
        .send(QueuedLifecycleResult {
            invocation: fresh_invocation,
            value: "fresh second result".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);
    assert_eq!(
        accepted(&handles.accepted).last(),
        Some(&("second".to_owned(), "fresh second result".to_owned()))
    );
    assert!(matches!(
        tile.poll_resource_operation(fresh_invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. })
            if invocation == fresh_invocation
    ));
    assert!(
        tile.poll_resource_operation(fresh_invocation, false, &outer, &ROOT)
            .is_none()
    );
}

#[test]
fn recovered_missing_target_still_releases_after_terminal_invocation_map_removal() {
    let (_directory, _owners, store) = lifecycle_store("release-recovery");
    let first = note(725);
    let second = note(726);
    seed_note(&store, &first, "first", "first");
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &store, &first);
    let mut outer = automerge::AutoCommit::new();
    tile.sync(&outer, &ROOT);
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    handles
        .sender
        .borrow()
        .send(QueuedLifecycleResult {
            invocation,
            value: "completed on first".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
    let late_sender = handles.sender.borrow().clone();

    tile.bind_record(store.clone(), second.clone());
    assert!(tile.record_transition_error().is_some());
    assert!(tile.history_owner().is_some());
    seed_note(&store, &second, "second", "second");
    tile.sync(&outer, &ROOT);
    assert_eq!(handles.releases.get(), 1);
    assert!(tile.record_transition_error().is_none());
    assert!(
        late_sender
            .send(QueuedLifecycleResult {
                invocation,
                value: "late after terminal poll".into(),
            })
            .is_err()
    );
    for _ in 0..3 {
        tile.sync(&outer, &ROOT);
    }
    assert_eq!(
        accepted(&handles.accepted),
        one("first", "completed on first"),
        "the late result reaches nobody"
    );
}

#[test]
fn unavailable_old_record_loan_blocks_release_without_acknowledging_commit() {
    let (_directory, _owners, store) = lifecycle_store("release-busy-old");
    let first = note(729);
    let second = note(730);
    for (checkout, owner) in [(&first, "first"), (&second, "second")] {
        seed_note(&store, checkout, owner, owner);
    }
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &store, &first);
    let mut outer = automerge::AutoCommit::new();
    tile.sync(&outer, &ROOT);
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    handles
        .sender
        .borrow()
        .send(QueuedLifecycleResult {
            invocation,
            value: "pending old commit".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);

    store
        .with(&first.id, |_| {
            tile.bind_record(store.clone(), second.clone());
        })
        .unwrap();
    assert!(tile.record_transition_error().is_some());
    assert_eq!(handles.releases.get(), 0);

    tile.bind_record(store.clone(), second);
    assert!(tile.record_transition_error().is_none());
    assert_eq!(handles.releases.get(), 1);
    assert_eq!(
        accepted(&handles.accepted),
        one("first", "pending old commit")
    );
}

#[test]
fn renewed_same_id_custody_releases_terminally_polled_old_queue_before_sync() {
    let (_directory, _owners, store) = lifecycle_store("same-id-custody");
    let current = note(731);
    seed_note(&store, &current, "current", "current");
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &store, &current);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    let stale_sender = handles.sender.borrow().clone();
    stale_sender
        .send(QueuedLifecycleResult {
            invocation,
            value: "must not resume after renewal".into(),
        })
        .unwrap();

    handles
        .intent
        .replace(Some(crate::record::RecordCheckout::new(
            current.id.clone(),
            crate::record::RecordKind::new("wrong-kind"),
        )));
    tile.sync(&outer, &ROOT);
    assert!(tile.record_transition_error().is_some() || tile.history_owner().is_none());
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Failed { invocation: failed, .. })
            if failed == invocation
    ));

    handles.intent.replace(Some(current.clone()));
    let _ = tile.content_mut();
    tile.sync(&outer, &ROOT);

    assert!(
        accepted(&handles.accepted).is_empty(),
        "a renewed lifetime with the same record spelling must release the old queue first"
    );
    assert_eq!(handles.releases.get(), 1);
    assert!(
        stale_sender
            .send(QueuedLifecycleResult {
                invocation,
                value: "late stale result".into(),
            })
            .is_err()
    );
}

#[test]
fn renewed_lifetime_blocks_recovery_event_before_old_custody_release() {
    let (_directory, _owners, store) = lifecycle_store("renewed-event-custody");
    let current = note(740);
    seed_note(&store, &current, "current", "current");
    let (probe, handles) = retained_lifecycle_probe("renewed-event", "current");
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(Box::new(probe)),
    ));
    tile.bind_record(store.clone(), current.clone());
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let mut driver = Driver::new(State::new(), tile, (700, 480));
    let invocation = tilo_actions::InvocationId::new();
    assert!(matches!(
        unsafe {
            (*tile_pointer).start_operation(
                &crate::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    RETAINED_LIFECYCLE_OPERATION,
                    &[],
                ),
                &mut crate::widget::Edits::lend(&mut driver.state_mut().doc),
                &ROOT,
            )
        }
        .unwrap(),
        Some(crate::widget::ResourceOperationStart::Accepted)
    ));
    handles
        .event_value
        .replace(Some("must not receive renewed event".into()));

    unsafe {
        (*tile_pointer).content_mut();
    }
    driver.key("x");

    assert_eq!(
        stored_body(&store, &current),
        "current",
        "renewing the lifetime must not let an event bypass admitted old custody"
    );
    assert_eq!(handles.events.get(), 0);
    assert!(unsafe { (*tile_pointer).record_transition_error().is_some() });
}

#[test]
fn explicit_current_bind_cancels_missing_target_without_later_replay() {
    let (_directory, _owners, store) = lifecycle_store("cancel-pending-bind");
    let current = note(732);
    let abandoned = note(733);
    seed_note(&store, &current, "current", "current");
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &store, &current);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);

    tile.bind_record(store.clone(), abandoned.clone());
    assert!(tile.record_transition_error().is_some());
    tile.bind_record(store.clone(), current.clone());
    assert!(tile.record_transition_error().is_none());
    seed_note(&store, &abandoned, "abandoned", "abandoned");
    handles
        .sender
        .borrow()
        .send(QueuedLifecycleResult {
            invocation,
            value: "current owner result".into(),
        })
        .unwrap();
    for _ in 0..3 {
        tile.sync(&outer, &ROOT);
        tile.update(&outer, &ROOT);
    }

    assert_eq!(
        accepted(&handles.accepted),
        one("current", "current owner result"),
        "later target availability must not replay the superseded bind"
    );
    assert_eq!(handles.releases.get(), 0);
    assert_eq!(handles.last_owner.borrow().as_deref(), Some("current"));
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. })
            if completed == invocation
    ));
}

#[test]
fn refused_history_transition_is_accessible_and_back_to_live_remains_interactive() {
    let (_directory, _owners, store) = lifecycle_store("visible-transition-refusal");
    let current = note(734);
    let candidate = note(735);
    for (checkout, owner) in [(&current, "current"), (&candidate, "candidate")] {
        seed_note(&store, checkout, owner, owner);
    }
    let selected = store
        .read_checked(&current.id, |doc| doc.base_heads()[0])
        .unwrap();
    store
        .with_history(&current.id, |doc, history| {
            doc.put(ROOT, "body", "current with history").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(crate::collaboration::local_user_id()),
            );
        })
        .unwrap();
    let (probe, handles) = unfenced_retained_lifecycle_probe("visible-refusal", "current");
    let mut tile = bound_tile(TileEditor(probe), &store, &current);
    tile.register_document_history(&RESTORABLE).unwrap();
    let invocation = tilo_actions::InvocationId::new();
    let mut outer = automerge::AutoCommit::new();
    admit(&mut tile, invocation, &mut outer);
    let mut tile = Box::new(tile);
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let root = crate::widget::Row::new().push(tile);
    let mut driver = Driver::new(State::new(), Box::new(root), (900, 650));
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Notebook: current")
    );
    assert!(driver.targets().contains_key("history-back-live"));
    let old_owner = unsafe {
        (*tile_pointer)
            .history_owner()
            .expect("the selected revision retains the real live owner witness")
    };
    old_owner.validate().unwrap();
    handles.intent.replace(Some(candidate.clone()));
    driver.relayout();

    assert!(unsafe { (*tile_pointer).record_transition_error().is_some() });
    let accessible = format!("{:?}", driver.a11y_nodes());
    assert!(
        accessible.contains("Document switch blocked"),
        "the refusal must be presented through the production accessibility tree: {accessible}"
    );
    driver.click_named("history-back-live").unwrap();
    assert!(driver.targets().contains_key("history-open"));
    assert_eq!(stored_body(&store, &candidate), "candidate");
    old_owner.validate().unwrap();
    let retained_owner = unsafe {
        (*tile_pointer)
            .history_owner()
            .expect("the refusal must retain the live owner witness")
    };
    retained_owner.validate().unwrap();
    assert!(retained_owner.same_owner(&old_owner));
}

#[test]
fn busy_old_owner_preserves_completed_cancelled_result_until_retry() {
    let (_directory, mut owners, old_store) = lifecycle_store("busy-result-old");
    let candidate_store = owners
        .open(RESTORABLE.app_id, "busy-result-candidate")
        .unwrap();
    let old = note(736);
    let candidate = note(737);
    seed_note(&old_store, &old, "old", "old");
    seed_note(&candidate_store, &candidate, "candidate", "candidate");
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &old_store, &old);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: None,
        });

    old_store
        .with(&old.id, |_| {
            tile.bind_record(candidate_store.clone(), candidate.clone());
        })
        .unwrap();
    assert!(tile.record_transition_error().is_some());
    assert_eq!(handles.releases.get(), 0);
    assert_eq!(handles.completions.get(), 0);

    tile.bind_record(candidate_store.clone(), candidate.clone());
    assert!(tile.record_transition_error().is_none());
    assert_eq!(handles.releases.get(), 1);
    assert_eq!(handles.completions.get(), 1);
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Cancelled { invocation: completed, .. })
            if completed == invocation
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT)
            .is_none()
    );
    assert_eq!(stored_body(&candidate_store, &candidate), "candidate");
}

#[test]
fn cached_item_blocks_release_until_broker_drains_then_preserves_terminal_order() {
    let (_directory, mut owners, old_store) = lifecycle_store("stream-old");
    let candidate_store = owners.open(RESTORABLE.app_id, "stream-candidate").unwrap();
    let old = note(738);
    let candidate = note(739);
    for (store, checkout, owner) in [
        (&old_store, &old, "old"),
        (&candidate_store, &candidate, "candidate"),
    ] {
        seed_note(store, checkout, owner, owner);
    }
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(probe)));
    tile.bind_record(old_store, old);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!("first"),
        });

    tile.bind_record(candidate_store.clone(), candidate.clone());
    assert!(tile.record_transition_error().is_some());
    assert_eq!(handles.releases.get(), 0);
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: Some("after item".into()),
        });
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Item { sequence: 1, .. })
    ));

    tile.bind_record(candidate_store.clone(), candidate.clone());
    assert!(tile.record_transition_error().is_none());
    assert_eq!(handles.releases.get(), 1);
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation: completed,
            message: Some(message),
        }) if completed == invocation && message == "after item"
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT)
            .is_none()
    );
    assert_eq!(stored_body(&candidate_store, &candidate), "candidate");
}

#[test]
fn pending_missing_target_retries_after_cached_item_is_delivered() {
    let (_directory, mut owners, old_store) = lifecycle_store("pending-item-old");
    let candidate_store = owners
        .open(RESTORABLE.app_id, "pending-item-candidate")
        .unwrap();
    let old = note(741);
    let candidate = note(742);
    seed_note(&old_store, &old, "old", "old");
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &old_store, &old);
    let mut outer = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);

    tile.bind_record(candidate_store.clone(), candidate.clone());
    assert!(tile.record_transition_error().is_some());
    seed_note(&candidate_store, &candidate, "candidate", "candidate");
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!("first"),
        });
    tile.sync(&outer, &ROOT);
    assert!(tile.record_transition_error().is_some());
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Item {
            sequence: 1,
            value,
            ..
        }) if value == serde_json::json!("first")
    ));
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: Some("after item".into()),
        });

    tile.sync(&outer, &ROOT);

    assert_eq!(
        handles.last_owner.borrow().as_deref(),
        Some("candidate"),
        "ordinary ticks must resume the unsuperseded pending target after Item delivery"
    );
    let owner = tile.history_owner().unwrap();
    let (owner_store, owner_checkout) = owner.store_checkout();
    assert!(owner_store.same_owner(&candidate_store));
    assert_eq!(owner_checkout, &candidate);
    assert_eq!(stored_body(&candidate_store, &candidate), "candidate");
    assert!(tile.record_transition_error().is_none());
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation: completed,
            message: Some(message),
        }) if completed == invocation && message == "after item"
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT)
            .is_none()
    );
    assert_eq!(handles.releases.get(), 1);
}

#[test]
fn newer_busy_commit_bind_supersedes_available_target_queued_by_item() {
    assert_newer_refused_bind_supersedes_queued_target(false);
}

#[test]
fn newer_root_release_refusal_supersedes_available_target_queued_by_item() {
    assert_newer_refused_bind_supersedes_queued_target(true);
}

#[allow(
    clippy::too_many_lines,
    reason = "The assertion helper compares queued and refused bindings across one custody transition."
)]
fn assert_newer_refused_bind_supersedes_queued_target(refuse_release: bool) {
    let (_directory, mut owners, old_store) = lifecycle_store("superseded-old");
    let obsolete_store = owners
        .open(RESTORABLE.app_id, "superseded-obsolete")
        .unwrap();
    let latest_store = owners.open(RESTORABLE.app_id, "superseded-latest").unwrap();
    let old = note(743);
    let obsolete = note(744);
    let latest = note(745);
    for (store, checkout, owner) in [
        (&old_store, &old, "old"),
        (&obsolete_store, &obsolete, "obsolete"),
        (&latest_store, &latest, "latest"),
    ] {
        seed_note(store, checkout, owner, owner);
    }
    let (probe, handles) = releasable_lifecycle_probe();
    let mut tile = bound_tile(probe, &old_store, &old);
    let mut outer = automerge::AutoCommit::new();
    tile.sync(&outer, &ROOT);
    let invocation = tilo_actions::InvocationId::new();
    admit(&mut tile, invocation, &mut outer);
    let old_owner = tile.history_owner().unwrap();
    old_owner.validate().unwrap();
    let old_sender = handles.sender.borrow().clone();
    old_sender
        .send(QueuedLifecycleResult {
            invocation,
            value: "old draft".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);
    assert_eq!(accepted(&handles.accepted), one("old", "old draft"));

    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!("old item"),
        });
    tile.bind_record(obsolete_store.clone(), obsolete.clone());
    assert!(tile.record_transition_error().is_some());
    assert_eq!(handles.releases.get(), 0);
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Item {
            invocation: observed, sequence: 1, value,
        }) if observed == invocation && value == serde_json::json!("old item")
    ));
    handles
        .terminal_update
        .borrow_mut()
        .replace(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::json!("old result")),
                ..tilo_actions::ActionOutput::default()
            },
        });
    assert!(matches!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation: observed, output })
            if observed == invocation && output.value == Some(serde_json::json!("old result"))
    ));
    assert!(
        tile.poll_resource_operation(invocation, false, &outer, &ROOT)
            .is_none()
    );
    assert_eq!(handles.completions.get(), 1);

    if refuse_release {
        handles.refuse_release.set(true);
        tile.bind_record(latest_store.clone(), latest.clone());
        assert_eq!(
            tile.record_transition_error(),
            Some("draft release temporarily refused")
        );
        handles.refuse_release.set(false);
    } else {
        // Only A's store is borrowed: C admission and the empty retained-update
        // collector succeed, then committing A encounters the real Busy error.
        old_store
            .with(&old.id, |_| {
                tile.bind_record(latest_store.clone(), latest.clone());
            })
            .unwrap();
        assert_eq!(
            tile.record_transition_error(),
            Some("record is reserved or already initialized")
        );
    }
    assert_eq!(handles.releases.get(), 0);

    for _ in 0..3 {
        tile.sync(&outer, &ROOT);
        tile.update(&outer, &ROOT);
        assert_eq!(
            handles.last_owner.borrow().as_deref(),
            Some("old"),
            "ordinary ticks must retain A, never replay obsolete B after newer C refused"
        );
        let retained_owner = tile.history_owner().unwrap();
        retained_owner.validate().unwrap();
        old_owner.validate().unwrap();
        assert!(retained_owner.same_owner(&old_owner));
        for (store, checkout, body) in [
            (&obsolete_store, &obsolete, "obsolete"),
            (&latest_store, &latest, "latest"),
        ] {
            assert_eq!(stored_body(store, checkout), body);
        }
        assert!(
            tile.poll_resource_operation(invocation, false, &outer, &ROOT)
                .is_none()
        );
    }
    assert_eq!(handles.releases.get(), 0);

    tile.bind_record(latest_store.clone(), latest.clone());
    assert!(tile.record_transition_error().is_none());
    tile.sync(&outer, &ROOT);
    assert_eq!(handles.last_owner.borrow().as_deref(), Some("latest"));
    let latest_owner = tile.history_owner().unwrap();
    latest_owner.validate().unwrap();
    let (owner_store, owner_checkout) = latest_owner.store_checkout();
    assert!(owner_store.same_owner(&latest_store));
    assert_eq!(owner_checkout, &latest);
    assert_eq!(handles.releases.get(), 1);
    assert!(
        old_sender
            .send(QueuedLifecycleResult {
                invocation,
                value: "stale old result".into(),
            })
            .is_err()
    );

    let fresh = tilo_actions::InvocationId::new();
    admit(&mut tile, fresh, &mut outer);
    handles
        .sender
        .borrow()
        .send(QueuedLifecycleResult {
            invocation: fresh,
            value: "fresh latest result".into(),
        })
        .unwrap();
    tile.sync(&outer, &ROOT);
    assert!(matches!(
        tile.poll_resource_operation(fresh, false, &outer, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { invocation: observed, .. }) if observed == fresh
    ));
    assert!(
        tile.poll_resource_operation(fresh, false, &outer, &ROOT)
            .is_none()
    );
    assert_eq!(
        accepted(&handles.accepted),
        [
            one("old", "old draft"),
            one("latest", "fresh latest result")
        ]
        .concat()
    );
}
#[path = "readiness_tests.rs"]
mod readiness_tests;
