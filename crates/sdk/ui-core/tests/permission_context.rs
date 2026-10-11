#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::permissions::{
    Actor, AppKind, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef,
    Role, Scope, Subject, UserId, Verb,
};

/// A viewer in the room: role only, no individual capabilities.
fn viewer_ctx(user: &str) -> PermissionContext {
    PermissionContext::from_capabilities(Actor::person(Some(Role::Viewer), user), vec![])
}

/// An editor in the room, additionally shared one file.
///
/// The share is a capability, not a role property — roles carry no base
/// authority over the filesystem, so even an Owner needs one of these
/// to touch a path.
fn editor_ctx(user: &str) -> PermissionContext {
    PermissionContext::from_capabilities(
        Actor::person(Some(Role::Editor), user),
        vec![CapOverride::grant(
            Subject::person(user),
            Scope::FsPath("/var/workspace/report.md".into()),
            Verb::Write,
        )],
    )
}

/// One permission check: case label, context, scope, expected `check` answer,
/// and (when pinned) the expected `check_with_denied_feedback` report.
type CheckCase = (
    &'static str,
    PermissionContext,
    PermissionScope,
    bool,
    Option<Option<CapDecision>>,
);

/// Role ceilings and scoped shares decide every check: `allowed` is what
/// `check` answers, `feedback` (when the case pins it) what
/// `check_with_denied_feedback` reports.
#[test]
fn roles_and_scoped_shares_decide_each_permission_check() {
    // An actor with no role has no base authority at all, so absent an
    // explicit capability it is denied — fail-closed by construction.
    let no_role = || PermissionContext::from_capabilities(Actor::person(None, "x"), vec![]);
    // Role::Denied is a block, not an absence. A share must not undo it.
    let denied_with_admin_grant = || {
        PermissionContext::from_capabilities(
            Actor::person(Some(Role::Denied), "mallory"),
            vec![CapOverride::grant(
                Subject::person("mallory"),
                Scope::Workspace,
                Verb::Admin,
            )],
        )
    };
    let denied = Some(Some(CapDecision::DeniedByRole));
    let cases: Vec<CheckCase> = vec![
        (
            "viewer_cannot_edit_terminal",
            viewer_ctx("alice"),
            PermissionScope::app_kind(AppKind::Terminal, Verb::Write),
            false,
            denied,
        ),
        (
            "editor_can_edit_file",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/report.md", Verb::Write),
            true,
            Some(None),
        ),
        // The grant is scoped to one path. Component-aligned prefix
        // matching must not let it cover a sibling that merely shares a
        // textual prefix.
        (
            "a_file_share_does_not_leak_to_its_siblings: textual prefix",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/report.md.bak", Verb::Write),
            false,
            None,
        ),
        (
            "a_file_share_does_not_leak_to_its_siblings: sibling",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/payroll.md", Verb::Write),
            false,
            None,
        ),
        (
            "workspace_admin_requires_admin_authority",
            editor_ctx("bob"),
            PermissionScope::workspace(Verb::Admin),
            false,
            denied,
        ),
        // An Editor's ceiling is Write; writing the room is still fine.
        (
            "workspace_admin_requires_admin_authority: editor writes the room",
            editor_ctx("bob"),
            PermissionScope::workspace(Verb::Write),
            true,
            None,
        ),
        (
            "actor_with_no_role_denies_everything: app",
            no_role(),
            PermissionScope::app_kind(AppKind::FileExplorer, Verb::Read),
            false,
            denied,
        ),
        (
            "actor_with_no_role_denies_everything: workspace",
            no_role(),
            PermissionScope::workspace(Verb::Read),
            false,
            None,
        ),
        (
            "actor_with_no_role_denies_everything: path",
            no_role(),
            PermissionScope::path("/var/workspace/a.txt", Verb::Read),
            false,
            None,
        ),
        (
            "an_explicitly_denied_role_cannot_be_rescued_by_a_grant",
            denied_with_admin_grant(),
            PermissionScope::workspace(Verb::Read),
            false,
            denied,
        ),
    ];
    for (case, ctx, scope, allowed, feedback) in cases {
        assert_eq!(ctx.check(&scope), allowed, "case {case}: check");
        if let Some(feedback) = feedback {
            assert_eq!(
                ctx.check_with_denied_feedback(&scope),
                feedback,
                "case {case}: denied feedback"
            );
        }
    }
}

struct NoOpWidget;
impl tilo_ui::widget::Widget for NoOpWidget {
    #[allow(
        clippy::unimplemented,
        reason = "test-only Widget stub; build_layout is never invoked by these tests (they \
                  exercise the presence_key/permission_scope defaults)"
    )]
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &tilo_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui::widget::Widget>,
    ) -> taffy::NodeId {
        unimplemented!()
    }
    fn apply_layout(
        &mut self,
        _t: &taffy::TaffyTree<()>,
        _n: taffy::NodeId,
        _o: vello::kurbo::Point,
    ) {
    }
    fn draw(
        &mut self,
        _s: &mut vello::Scene,
        _tr: vello::kurbo::Affine,
        _txt: &mut tilo_ui::text_render::TextRenderer,
        _th: &tilo_ui::theme::Theme,
        _presence: &tilo_ui::presence::PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "noop"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
}

#[test]
fn widget_defaults_carry_no_presence_or_permission() {
    let w = NoOpWidget;
    assert!(tilo_ui::presence::capability::presence_key(&w).is_none());
    assert!(tilo_ui::permissions::capability::permission_scope(&w).is_none());
}

use tilo_ui::permissions::PermissionScope as PS;

struct GatedButton {
    clicked: bool,
    scope: PS,
}

impl tilo_ui::widget::Widget for GatedButton {
    tilo_ui::provides!(dyn tilo_ui::permissions::capability::PermissionGated);

    #[allow(
        clippy::unimplemented,
        reason = "test-only Widget stub; build_layout is never invoked by these tests (they \
                  exercise dispatch_gated_event via handle_event)"
    )]
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &tilo_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn tilo_ui::widget::Widget>,
    ) -> taffy::NodeId {
        unimplemented!()
    }
    fn apply_layout(
        &mut self,
        _t: &taffy::TaffyTree<()>,
        _n: taffy::NodeId,
        _o: vello::kurbo::Point,
    ) {
    }
    fn draw(
        &mut self,
        _s: &mut vello::Scene,
        _tr: vello::kurbo::Affine,
        _txt: &mut tilo_ui::text_render::TextRenderer,
        _th: &tilo_ui::theme::Theme,
        _presence: &tilo_ui::presence::PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &winit::event::WindowEvent,
        _ctx: &mut tilo_ui::widget::EventCtx<'_>,
    ) {
        self.clicked = true;
    }
    fn name(&self) -> &'static str {
        "gated"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
}

impl tilo_ui::permissions::capability::PermissionGated for GatedButton {
    fn permission_scope(&self) -> Option<PS> {
        Some(self.scope.clone())
    }
}

/// The framework's gate fires a widget's handler exactly when the context
/// grants the widget's declared scope.
#[test]
fn gated_widget_fires_only_when_its_scope_is_granted() {
    let cases = [
        (
            "gated_widget_rejected_by_framework_helper",
            viewer_ctx("alice"),
            false,
        ),
        (
            "gated_widget_allowed_when_role_grants_edit",
            editor_ctx("bob"),
            true,
        ),
    ];
    for (case, ctx, want) in cases {
        let w = GatedButton {
            clicked: false,
            scope: PermissionScope::app_kind(AppKind::Terminal, Verb::Write),
        };
        let mut fired = false;
        let allowed = tilo_ui::app::dispatch_gated_event(&ctx, &w, || {
            fired = true;
        });
        assert_eq!(allowed, want, "case {case}: allowed");
        assert_eq!(fired, want, "case {case}: fired");
    }
}

#[test]
fn capabilities_are_inspectable_scoped_and_expire() {
    // Case `permission_context_built_from_an_actor_and_its_capabilities`.
    {
        let ctx = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "alice"),
            vec![CapOverride::grant(
                Subject::person("alice"),
                Scope::App("terminal".into()),
                Verb::Admin,
            )],
        );
        assert_eq!(ctx.user(), &UserId("alice".into()));
        assert_eq!(ctx.role(), Some(Role::Editor));
        assert!(ctx.check(&PermissionScope::app_kind(AppKind::Terminal, Verb::Write)));
        // The individual grant lifts her above her role's ceiling — but only
        // inside the scope it names.
        assert!(ctx.check(&PermissionScope::app_kind(AppKind::Terminal, Verb::Admin)));
        assert!(!ctx.check(&PermissionScope::app_kind(
            AppKind::FileExplorer,
            Verb::Admin
        )));
    }

    // Case `a_lapsed_capability_stops_granting`.
    {
        let cap = CapOverride::grant(
            Subject::person("alice"),
            Scope::App("terminal".into()),
            Verb::Admin,
        )
        .with_expiry(2_000);
        let actor = Actor::person(Some(Role::Editor), "alice");
        let scope = PermissionScope::app_kind(AppKind::Terminal, Verb::Admin);

        let live = PermissionContext::from_capabilities(actor.clone(), vec![cap.clone()])
            .at_unix_time(1_999);
        assert!(live.check(&scope));

        let lapsed = PermissionContext::from_capabilities(actor, vec![cap]).at_unix_time(2_000);
        assert!(
            !lapsed.check(&scope),
            "the deadline is exclusive: at it, the grant is dead"
        );
        assert_eq!(
            lapsed.check_with_denied_feedback(&scope),
            Some(CapDecision::DeniedByRole)
        );
    }

    // Case `permissive_holds_one_real_workspace_wide_admin_capability`.
    {
        let ctx = PermissionContext::permissive();
        // Not a bypass flag — an inspectable capability.
        assert_eq!(ctx.capabilities().len(), 1);
        assert_eq!(ctx.capabilities()[0].scope, Scope::Workspace);
        assert_eq!(ctx.capabilities()[0].verb, Verb::Admin);
        assert!(ctx.check(&PermissionScope::workspace(Verb::Admin)));
        assert!(ctx.check(&PermissionScope::app_kind(AppKind::Terminal, Verb::Write)));
        assert!(ctx.check(&PermissionScope::path(
            "/var/workspace/anything",
            Verb::Write
        )));
        assert_eq!(
            ctx.decide(&ResourceRef::Device("camera".into()), Verb::Read),
            CapDecision::Allow
        );
    }
}
