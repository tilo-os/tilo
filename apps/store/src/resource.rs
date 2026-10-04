//! App Store as a [`tilo_resource::Resource`]: report drafts are document
//! state; install/update jobs and the live catalogue are extras when hosted.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, ExecutionMode, OperationKind, ParamType,
    Resource as PublishedResource, ResultSpec, RetryPolicy,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::app::StoreApp;
use crate::doc_actions::{
    STORE_DOCUMENT_ACTION_TYPE, StoreDocumentAction, register, report_document_children,
    state_from_doc,
};

/// Report drafts persisted in the shared document (plus note text).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportProjection {
    pub id: String,
    pub app_id: String,
    pub status: String,
    pub note: String,
}

/// Shared catalogue projection: collaborative reports only. Installed apps,
/// remotes and download progress are live host state, not document rows.
#[derive(Clone, Debug, Default)]
pub struct StoreState {
    pub reports: Vec<ReportProjection>,
}

/// The id of the catalogue's root resource: its kind, which is the id the
/// framework publishes it under, so a control bound to it resolves in every
/// host.
pub const CATALOGUE_ID: &str = "store.catalog";

/// The app-store catalogue resource.
pub struct Store;

impl Resource for Store {
    const KIND: &'static str = CATALOGUE_ID;
    const LABEL: &'static str = "App Store";
    const ACTION_TYPE: &'static str = STORE_DOCUMENT_ACTION_TYPE;

    type State = StoreState;
    type Action = StoreDocumentAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<StoreState, ModelError> {
        Ok(state_from_doc(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![ActionDecl::new(
            "draft-report",
            "Create report draft",
            vec![ActionParam::required(
                "app_id",
                ParamType::Text,
                "App identity (for example so.tilo.notes)",
            )],
            |args, _| {
                let app_id = args
                    .text("app_id")
                    .ok_or_else(|| "missing app id".to_owned())?
                    .trim()
                    .to_owned();
                if app_id.is_empty() {
                    return Err("missing app id".into());
                }
                Ok(StoreDocumentAction::CreateReportDraft { app_id })
            },
        )]
    }

    fn children(state: &StoreState) -> Vec<PublishedResource> {
        report_document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Refresh is an Effect with no document delta. Search / browse Views
        // and per-app install Effects ride live extras or standalone overlay.
        let mut ops = Vec::new();
        if let Ok(refresh) = ActionSpec::operation(
            "refresh",
            "Refresh catalog",
            OperationKind::Effect,
            "store.refresh",
            &"",
        ) {
            ops.push(refresh.with_contract(tilo_actions::ActionContract {
                retry: RetryPolicy::Idempotent,
                execution: ExecutionMode::Background { cancellable: false },
                result: ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            }));
        }
        // Search apps — View with no document delta (device-local browse).
        ops.extend(
            crate::browse_actions::resource(&[])
                .actions
                .into_iter()
                .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect)),
        );
        ops
    }

    fn search(state: &StoreState) -> Vec<SearchEntry> {
        state
            .reports
            .iter()
            .map(|report| SearchEntry {
                anchor: Some(format!("store.report:{}", report.app_id)),
                title: format!("Report for {}", report.app_id),
                text: report.status.clone(),
            })
            .collect()
    }
}

/// Framework app.
pub struct StoreShell;

impl ResourceApp for StoreShell {
    type Resource = Store;
    const ID: &'static str = "so.tilo.store";
    const TITLE: &'static str = "App Store";
    const ICON: Option<&'static str> = Some("store");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Store>) -> Element {
        cx.surface(crate::live::LiveStore::new(None, None))
    }

    fn scenarios() -> &'static [&'static str] {
        crate::shots::SCENARIOS
    }

    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        name.ends_with("narrow")
            .then_some(crate::shots::NARROW_VIEWPORT)
    }
}

impl ResourceSurface<Store> for StoreApp {
    fn present(&mut self, state: &std::rc::Rc<StoreState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Store>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        crate::shots::stage(self, scenario);
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Store>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Store>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &StoreDocumentAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last =
                    tilo_actions::EncodedAction::encode(STORE_DOCUMENT_ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &StoreState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Store>(
        CATALOGUE_ID,
        "App Store",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc_actions::StoreDocumentAction;
    use tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_catalog_door_and_hosted_publishes_the_live_resources_beside_it() {
        let mut doc = AutoCommit::new();
        StoreDocumentAction::CreateReportDraft {
            app_id: "so.tilo.notes".into(),
        }
        .apply(&mut doc)
        .unwrap();
        StoreDocumentAction::SetReportNote {
            app_id: "so.tilo.notes".into(),
            note: "Unsigned binary".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Store::read(&doc).expect("seeded store state");
        let published = ui::tilo_resource::publish::<Store>(CATALOGUE_ID, "App Store", &state);
        assert_eq!(published.kind, "store.catalog");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "draft-report"),
            "draft-report declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "refresh"),
            "refresh Effect must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-search"),
            "Search apps View must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "store.moderation"),
            "moderation children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .flat_map(|child| child.children.iter())
                .any(|child| child.kind == "store.report"),
            "report children must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .all(|child| child.kind != "store.install-job"),
            "shared projection must not carry live install jobs"
        );

        let mut app = StoreApp::new();
        app.set_flatpak_available(true);
        app.set_items(crate::shots::fixture_catalog());
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "store.catalog");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "store.publisher" || child.kind == "store.app"),
            "standalone overlays live catalogue children"
        );

        app.set_status_line("Installing so.tilo.notes…");
        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "store.catalog"),
            "hosted must not republish store.catalog beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let kinds: Vec<&str> = extras
            .iter()
            .map(|resource| resource.kind.as_str())
            .collect();
        for kind in [
            "store.browse",
            "store.navigation",
            "store.publisher",
            "store.remote-list",
            "store.install-job",
        ] {
            assert!(
                kinds.contains(&kind),
                "hosted publishes the live {kind} beside the framework root: {kinds:?}"
            );
        }

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "store.catalog");
        assert!(
            door[0]
                .children
                .iter()
                .any(|child| child.kind == "store.moderation")
        );
        assert_eq!(state.reports.len(), 1);

        let mut local = AutoCommit::new();
        // Hosted sink records the last encoded action; clear hosted so the
        // local ActionHost applies into `local` for this assertion.
        let mut standalone = StoreApp::new();
        standalone.perform_document_action(
            &mut local,
            &StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.files".into(),
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
    }
}
