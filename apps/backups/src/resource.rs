//! Backups as a [`tilo_resource::Resource`]: retention policy is document
//! state; backup/restore runs are Effects in [`Resource::local_operations`].
//!
//! Document mutations (set / bump retention) live on this resource via
//! [`Resource::children`]. Hosted [`PublishesResources::resources`] returns
//! only live catalog extras — never a second hand-built `backups.policy` /
//! `backups` Document door beside [`tilo_resource::publish`].

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    BACKUPS_ACTION_TYPE, BACKUPS_RESOURCE_ID, BackupsAction, live_catalog_extras,
    publish_policy_document, register, workspace_effect_specs,
};
use crate::app::BackupsApp;
use crate::doc::BackupsDoc;
use crate::model::RetentionPolicy;

/// Retention policy projection from the shared document.
#[derive(Clone, Debug, Default)]
pub struct BackupsState {
    pub policy: RetentionPolicy,
}

/// The backups workspace resource.
pub struct Backups;

impl Resource for Backups {
    const KIND: &'static str = "backups.workspace";
    const LABEL: &'static str = "Backups";
    const ACTION_TYPE: &'static str = BACKUPS_ACTION_TYPE;

    type State = BackupsState;
    type Action = BackupsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<BackupsState, ModelError> {
        Ok(BackupsState {
            policy: BackupsDoc::policy(doc, &automerge::ROOT),
        })
    }

    fn children(state: &BackupsState) -> Vec<PublishedResource> {
        vec![publish_policy_document(&state.policy)]
    }

    fn local_operations() -> Vec<ActionSpec> {
        workspace_effect_specs(
            false,
            "Choose a source and destination before running a backup",
        )
        .into_iter()
        .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
        .collect()
    }
}

/// Hosted-only extras that are not document state: the live catalog of
/// sources, destinations and snapshots. Policy Document ops stay on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &BackupsApp) -> Vec<PublishedResource> {
    live_catalog_extras(
        app.resource_catalog(),
        app.is_configured(),
        app.unavailable_reason(),
    )
}

/// Framework app.
pub struct BackupsShell;

impl ResourceApp for BackupsShell {
    type Resource = Backups;
    const ID: &'static str = "so.tilo.backups";
    const TITLE: &'static str = "Backups";
    const ICON: Option<&'static str> = Some("backups");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Backups>) -> Element {
        cx.surface(BackupsApp::new())
    }

    /// Every capture but `setup` and `unavailable` stages the sample catalog
    /// ([`BackupsApp::stage_scenario`]); a plain `--shots` shows it on the
    /// overview.
    fn scenarios() -> &'static [&'static str] {
        &[
            "setup",
            "unavailable",
            "status",
            "progress",
            "failure",
            "restore",
            "sources",
            "destinations",
            "drive",
            "offsite",
            "schedules",
            "drawer",
        ]
    }

    /// The sidebar becomes a drawer only where the content would be squeezed.
    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        (name == "drawer").then_some((640, 800))
    }
}

impl ResourceSurface<Backups> for BackupsApp {
    fn present(&mut self, state: &std::rc::Rc<BackupsState>) {
        // Adopt shared policy only — never wipe unpublished local retention.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Backups>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Backups>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Backups>) {
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
        action: &BackupsAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
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

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`] plus
/// the live catalog extras that stay on the widget when hosted.
pub fn provide_framework_door(state: &BackupsState) -> Vec<PublishedResource> {
    let mut door = vec![ui::tilo_resource::publish::<Backups>(
        BACKUPS_RESOURCE_ID,
        "Backups",
        state,
    )];
    door.extend(live_catalog_extras(
        &crate::model::Catalog::empty(),
        false,
        None,
    ));
    door
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::PolicyField;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion covers publish ownership, hosted extras, and empty adopt"
    )]
    fn resource_publish_owns_policy_document_ops_and_hosted_keeps_no_second_door() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &automerge::ROOT);
        BackupsDoc::set_retention(&mut doc, &automerge::ROOT, PolicyField::Daily, 11);
        doc.commit();

        let state = Backups::read(&doc).expect("seeded backups state");
        let published =
            ui::tilo_resource::publish::<Backups>(BACKUPS_RESOURCE_ID, "Backups", &state);
        assert_eq!(published.kind, "backups.workspace");
        for id in ["backup-now", "setup"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} Effect must be on the framework workspace resource"
            );
        }
        assert!(
            published
                .actions
                .iter()
                .filter(|action| action.kind == OperationKind::Document)
                .all(|action| action.id == "apply" || action.id == "group"),
            "retention Document ops must not sit as root actions: {:?}",
            published
                .actions
                .iter()
                .filter(|action| action.kind == OperationKind::Document)
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>()
        );
        let policy = published
            .children
            .iter()
            .find(|child| child.kind == "backups.policy")
            .expect("policy child on the published door");
        assert_eq!(policy.children.len(), 4, "one child per retention class");
        let daily = policy
            .children
            .iter()
            .find(|child| child.id.as_str() == "backups.retention:ret_daily")
            .expect("daily retention child");
        for id in ["set", "increment", "decrement"] {
            assert!(
                daily
                    .actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the retention child"
            );
        }

        let mut app = BackupsApp::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        let daily_id = "backups.retention:ret_daily";
        let on_tree = {
            fn find<'a>(
                resources: &'a [PublishedResource],
                id: &str,
            ) -> Option<&'a PublishedResource> {
                for resource in resources {
                    if resource.id.as_str() == id {
                        return Some(resource);
                    }
                    if let Some(found) = find(&resource.children, id) {
                        return Some(found);
                    }
                }
                None
            }
            find(&standalone, daily_id).expect("daily retention is on the published tree")
        };
        let looked_up = app
            .publish_resource(&seeded, daily_id)
            .expect("daily retention resolves");
        assert_eq!(
            looked_up
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            on_tree
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            "an eager retention row is the published row"
        );
        let workspace = standalone
            .iter()
            .find(|resource| resource.kind == "backups.workspace")
            .expect("standalone keeps the workspace door");
        let policy = workspace
            .children
            .iter()
            .find(|child| child.kind == "backups.policy")
            .expect("standalone keeps the policy on the workspace");
        assert!(
            policy.children.iter().any(|child| {
                child.id.as_str() == "backups.retention:ret_daily"
                    && child.actions.iter().any(|action| action.id == "increment")
            }),
            "standalone policy keeps Document steppers"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "backups.policy"
                    && resource.kind != "backups.workspace"),
            "hosted must not republish backups.policy/workspace beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "backups.source-set"),
            "hosted still surfaces the live catalog extras"
        );

        let door = provide_framework_door(&state);
        assert_eq!(door[0].kind, "backups.workspace");
        assert!(
            door[0]
                .children
                .iter()
                .any(|child| child.kind == "backups.policy"),
            "publish children carry retention Document ops"
        );

        // Empty present must not wipe unpublished local policy.
        let mut local = BackupsApp::new();
        local.ui_policy_mut().daily = 42;
        assert_ne!(*local.ui_policy(), RetentionPolicy::default());
        local.adopt_resource_state(&BackupsState::default());
        assert_eq!(
            local.ui_policy().daily,
            42,
            "adopting an empty projection must not clear unpublished retention"
        );
    }
}
