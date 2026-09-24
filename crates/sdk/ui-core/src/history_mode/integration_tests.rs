use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHost, runner_permissions, runner_probe_definition,
};
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
fn managed_bound_history_resolves_real_owner_instead_of_outer_state() {
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
                crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                    &hash.to_string()
                ))
            )
        })
        .unwrap();
    let root = Box::new(BoundAppWidget { component });
    let source =
        HistorySourceRegistration::owned("document", APP, root.history_owner().unwrap()).unwrap();
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

pub(crate) static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.qos.restore-fixture",
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
fn managed_review_confirm_restores_exactly_one_authored_change_and_can_cancel() {
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
    driver.key("Enter");
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
fn grant(
    actor: &str,
    scope: crate::permissions::Scope,
    expiry: crate::permissions::Expiry,
) -> crate::permissions::PermissionContext {
    use crate::permissions::*;
    let mut cap = CapOverride::new(
        Subject::Principal(Principal::Person(actor.into())),
        scope,
        Effect::Grant,
        Verb::Write,
    );
    cap.expiry = expiry;
    PermissionContext::from_capabilities(Actor::person(None, actor), vec![cap]).at_unix_time(100)
}
#[test]
fn actual_confirmation_rechecks_unchanged_grant_expiry_replacement_and_actor_mapping() {
    use crate::permissions::*;
    for case in ["expiry", "replacement", "actor"] {
        let mut driver = review_driver();
        driver.set_permission_ctx(grant(
            LOCAL_USER,
            Scope::App(RESTORABLE.app_id.into()),
            Expiry::AtUnix(150),
        ));
        driver.click_named("history-restore").unwrap();
        let before = driver.state_mut().doc.base_heads();
        match case {
            "expiry" => NOW.set(200),
            "replacement" => driver.set_permission_ctx(PermissionContext::from_capabilities(
                Actor::person(None, LOCAL_USER),
                vec![],
            )),
            _ => driver.set_permission_ctx(grant(
                "another-person",
                Scope::App(RESTORABLE.app_id.into()),
                Expiry::Never,
            )),
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
#[test]
fn stale_review_requires_refresh_and_cancelled_footer_target_cannot_confirm() {
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
    driver.key("Enter");
    assert_eq!(driver.state_mut().doc.base_heads(), before);
    assert!(driver.a11y_node_named("Refresh review").is_some());
    driver.key("Enter");
    assert_eq!(
        driver.state_mut().doc.base_heads(),
        before,
        "refresh itself does not write"
    );
    driver.key("Enter");
    assert_eq!(
        crate::state::owned_restore_tests::value(&driver.state_mut().doc),
        "old"
    );
    assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
}

#[derive(Default)]
struct DeferredMedia(std::cell::RefCell<Vec<(HistoricalAssetRequest, HistoricalAssetResponse)>>);
impl HistoricalAssetSource for DeferredMedia {
    fn request(&self, request: HistoricalAssetRequest, response: HistoricalAssetResponse) {
        self.0.borrow_mut().push((request, response));
    }
}
#[test]
fn historical_media_requests_capture_both_sides_and_late_failure_cannot_replace_new_selection() {
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
    assert!(!format!("{:?}", driver.a11y_nodes()).contains("closed request must stay invisible"));
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
        .and_then(|(v, _)| v.to_str().map(str::to_owned))
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
#[test]
fn media_preview_uses_only_digest_recorded_in_immutable_selected_snapshot() {
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, sync::Arc};
    let historical: Arc<[u8]> = Arc::from(b"historical photograph".as_slice());
    let digest = format!("{:x}", Sha256::digest(&historical));
    let current: Arc<[u8]> = Arc::from(b"current photograph".as_slice());
    let current_digest = format!("{:x}", Sha256::digest(&current));
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
        snapshot.get(ROOT, "media").unwrap().unwrap().0.to_str(),
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
#[test]
fn managed_component_bundle_automatically_installs_shared_history_in_frame_runner() {
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

#[test]
fn managed_component_tile_automatically_composes_shared_history() {
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
fn tiled_history_semantic_open_preserves_pending_owner_and_other_tile_stays_live() {
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
                    .to_str()
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

pub(crate) struct TileEditor<T: Widget = crate::widget::TextInput>(pub(crate) T);
impl<T: Widget> crate::widget::AppWidget for TileEditor<T> {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Tile editor"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("T")
    }
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        None
    }
}
impl<T: Widget> Widget for TileEditor<T> {
    fn name(&self) -> &str {
        "TileEditor"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.0]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.0]
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.0.sync(doc, path);
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
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.0.get_rect()
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        mods: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.0.handle_event(
            event,
            scale,
            mods,
            doc,
            path,
            handled,
            focus,
            tree.children(node).unwrap()[0],
            tree,
        );
    }
}

#[test]
fn tiled_semantic_edit_checks_current_authority_and_focus_does_not_commit() {
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
        .set_permission_ctx(PermissionContext::from_capabilities(
            Actor::person(None, LOCAL_USER),
            vec![],
        ));
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

pub(crate) fn legacy_tiles_root(
    store: &crate::RecordStore,
    first: &crate::record::RecordCheckout,
    second: &crate::record::RecordCheckout,
) -> Box<dyn Widget> {
    let tile = |checkout: &crate::record::RecordCheckout, name: &str| {
        let mut tile =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(TileEditor(
                crate::widget::TextInput::from_document_string("body")
                    .with_id(name)
                    .with_accessible_name(name),
            ))));
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
fn legacy_raw_admission_refuses_busy_rebound_and_dropped_owner_before_callback() {
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
        crate::widget::TileContent::App(Box::new(TileEditor(
            crate::widget::TextInput::from_document_string("body").with_id("legacy-leaf"),
        )))
    };
    let mut tile = crate::widget::TileHost::new(content());
    tile.bind_record(a.clone(), checkout.clone());
    let mut outer = automerge::AutoCommit::new();
    outer.put(ROOT, "outer", "pending").unwrap();
    tile.sync(&mut outer, &ROOT);
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

#[test]
fn frame_legacy_at_alias_is_stable_for_owner_rebuild_and_rejects_replacement() {
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
        crate::widget::TileContent::App(Box::new(TileEditor(
            crate::widget::TextInput::from_document_string("body")
                .with_accessible_name("owned editor"),
        ))),
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

#[test]
fn late_asset_result_after_real_component_owner_close_stays_unavailable() {
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

#[test]
fn legacy_tile_review_restore_uses_actual_owner_one_undo_and_current_file_authority() {
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
        let mut capabilities = vec![CapOverride::new(
            principal.clone(),
            Scope::App(RESTORABLE.app_id.into()),
            Effect::Grant,
            Verb::Write,
        )];
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
            capabilities.push(CapOverride::new(
                principal,
                scope,
                Effect::Grant,
                Verb::Write,
            ));
        }
        driver.set_permission_ctx(PermissionContext::from_capabilities(
            Actor::person(None, LOCAL_USER),
            capabilities,
        ));
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

#[test]
fn legacy_ui_restore_schedules_only_target_and_reopens_durable_result() {
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
    let deadline = crate::time::now() + crate::time::Duration::from_secs(3);
    while store.save_status(&first.id).unwrap() != crate::record::RecordSaveStatus::Saved
        && crate::time::now() < deadline
    {
        std::thread::sleep(crate::time::Duration::from_millis(10));
    }
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
            crate::widget::TileContent::App(Box::new(TileEditor(
                crate::widget::TextInput::from_document_string("body")
                    .with_id("owned-editor")
                    .with_accessible_name("owned editor"),
            ))),
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
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|| doc.text(object).unwrap())
        })
        .unwrap()
}

#[test]
fn round1_top_level_frame_history_preserves_legacy_custody_and_stale_at_refusal() {
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
    driver.set_permission_ctx(PermissionContext::from_capabilities(
        Actor::person(None, LOCAL_USER),
        vec![],
    ));
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
                    HistorySourceRegistration::owned("document", RESTORABLE.app_id, owner).unwrap(),
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
        snapshot.get(ROOT, "body").unwrap().unwrap().0.to_str(),
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
#[test]
fn automatic_asset_factory_delivers_selected_requests_in_standalone_registration() {
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
#[test]
fn automatic_asset_factory_delivers_selected_requests_in_owned_component() {
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
#[test]
fn automatic_asset_factory_delivers_selected_requests_in_bound_tile() {
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
