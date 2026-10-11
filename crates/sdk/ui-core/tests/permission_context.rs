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
    Access, ActionKind, Answer, AppKind, CapDecision, CapOverride, Denial, PermissionContext,
    PermissionScope, ResourceRef, Role, Scope, Subject, TreePath, UserId, policy,
};

/// A viewer in the room: role only, no individual capabilities.
fn viewer_ctx(user: &str) -> PermissionContext {
    PermissionContext::member(user, Role::Viewer)
}

/// An editor in the room, additionally shared one file.
///
/// The share is a capability, not a role property — roles carry no base
/// authority over the filesystem, so even an Owner needs one of these
/// to touch a path.
fn editor_ctx(user: &str) -> PermissionContext {
    PermissionContext::member(user, Role::Editor).granting(CapOverride::grant(
        Subject::person(user),
        Scope::FsPath("/var/workspace/report.md".into()),
        Access::Write,
    ))
}

/// What a denial was decided by, for the cases that pin it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Grants,
    Sandbox,
}

fn by(denial: Option<Denial>) -> Option<By> {
    match denial? {
        Denial::NotGranted(_) => Some(By::Grants),
        Denial::Sandbox(_) => Some(By::Sandbox),
        Denial::Unavailable(error) => panic!("decision unavailable: {error}"),
    }
}

/// One permission check: case label, context, scope, expected `check` answer,
/// and (when pinned) the expected `check_with_denied_feedback` report.
type CheckCase = (
    &'static str,
    PermissionContext,
    PermissionScope,
    bool,
    Option<Option<By>>,
);

/// Role ceilings and scoped shares decide every check: `allowed` is what
/// `check` answers, `feedback` (when the case pins it) what
/// `check_with_denied_feedback` reports.
#[test]
fn roles_and_scoped_shares_decide_each_permission_check() {
    // An actor with no role has no base authority at all, so absent an
    // explicit capability it is denied — fail-closed by construction.
    let no_role = || PermissionContext::stranger("x");
    // A block is not an absence. A broader grant must not undo it.
    let denied_with_admin_grant = || {
        let ctx = PermissionContext::member("mallory", Role::Editor);
        let block = ctx
            .standing()
            .policy
            .block("mallory", policy::Subject::person("owner"));
        ctx.with_grant(block).with_grant(policy::Grant::new(
            "everything",
            policy::Subject::person("mallory"),
            TreePath::root(),
            policy::KindSet::ALL,
            Answer::Allow,
            policy::Subject::person("owner"),
        ))
    };
    let denied = Some(Some(By::Grants));
    let cases: Vec<CheckCase> = vec![
        (
            "viewer_cannot_edit_terminal",
            viewer_ctx("alice"),
            PermissionScope::app_kind(AppKind::Terminal, ActionKind::Undoable),
            false,
            denied,
        ),
        (
            "editor_can_edit_file",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/report.md", Access::Write),
            true,
            Some(None),
        ),
        // The grant is scoped to one path. Component-aligned prefix
        // matching must not let it cover a sibling that merely shares a
        // textual prefix.
        (
            "a_file_share_does_not_leak_to_its_siblings: textual prefix",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/report.md.bak", Access::Write),
            false,
            None,
        ),
        (
            "a_file_share_does_not_leak_to_its_siblings: sibling",
            editor_ctx("bob"),
            PermissionScope::path("/var/workspace/payroll.md", Access::Write),
            false,
            None,
        ),
        (
            "workspace_admin_requires_admin_authority",
            editor_ctx("bob"),
            PermissionScope::governance(policy::roles::governance::SETTINGS),
            false,
            denied,
        ),
        // An Editor changes the room; governing it is another matter.
        (
            "workspace_admin_requires_admin_authority: editor writes the room",
            editor_ctx("bob"),
            PermissionScope::workspace(ActionKind::Undoable),
            true,
            None,
        ),
        (
            "actor_with_no_role_denies_everything: app",
            no_role(),
            PermissionScope::app_kind(AppKind::FileExplorer, ActionKind::Read),
            false,
            denied,
        ),
        (
            "actor_with_no_role_denies_everything: workspace",
            no_role(),
            PermissionScope::workspace(ActionKind::Read),
            false,
            None,
        ),
        (
            "actor_with_no_role_denies_everything: path",
            no_role(),
            PermissionScope::path("/var/workspace/a.txt", Access::Read),
            false,
            None,
        ),
        (
            "an_explicitly_denied_role_cannot_be_rescued_by_a_grant",
            denied_with_admin_grant(),
            PermissionScope::workspace(ActionKind::Read),
            false,
            denied,
        ),
    ];
    for (case, ctx, scope, allowed, feedback) in cases {
        assert_eq!(ctx.check(&scope), allowed, "case {case}: check");
        if let Some(feedback) = feedback {
            assert_eq!(
                by(ctx.check_with_denied_feedback(&scope)),
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
            scope: PermissionScope::app_kind(AppKind::Terminal, ActionKind::Undoable),
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
fn grants_and_capabilities_are_inspectable_scoped_and_expire() {
    let terminal = TreePath::root().app(AppKind::Terminal.app_id());
    // Case `a_context_built_from_a_role_and_a_grant`.
    {
        let ctx = PermissionContext::member("alice", Role::Editor).with_grant(policy::Grant::new(
            "terminal",
            policy::Subject::person("alice"),
            TreePath::room(tilo_ui::permissions::LOCAL_ROOM).join(&terminal),
            policy::KindSet::ALL,
            Answer::Allow,
            policy::Subject::person("owner"),
        ));
        assert_eq!(ctx.user(), &UserId("alice".into()));
        assert_eq!(ctx.role(), Some(Role::Editor));
        assert!(ctx.check(&PermissionScope::app_kind(
            AppKind::Terminal,
            ActionKind::Undoable
        )));
        // The individual grant lifts her past her role's "asks first" for
        // outside effects — but only inside the node it names.
        assert_eq!(ctx.answer(&terminal, ActionKind::Outside), Answer::Allow);
        assert_eq!(
            ctx.answer(
                &TreePath::root().app(AppKind::FileExplorer.app_id()),
                ActionKind::Outside
            ),
            Answer::Ask
        );
    }

    // Case `a_lapsed_grant_stops_granting`.
    {
        let grant = policy::Grant::new(
            "terminal",
            policy::Subject::person("alice"),
            TreePath::room(tilo_ui::permissions::LOCAL_ROOM).join(&terminal),
            policy::KindSet::ALL,
            Answer::Allow,
            policy::Subject::person("owner"),
        )
        .with_lifetime(policy::Lifetime::Until(2_000_000));
        let ctx = || PermissionContext::member("alice", Role::Editor).with_grant(grant.clone());
        let live = ctx().at_unix_time(1_999);
        assert_eq!(live.answer(&terminal, ActionKind::Outside), Answer::Allow);
        let lapsed = ctx().at_unix_time(2_000);
        assert_eq!(
            lapsed.answer(&terminal, ActionKind::Outside),
            Answer::Ask,
            "the deadline is exclusive: at it, the grant is dead and the role answers"
        );
    }

    // Case `permissive_is_an_owner_with_one_real_workspace_wide_capability`.
    {
        let ctx = PermissionContext::permissive();
        // Not a bypass flag — inspectable capabilities and a structural owner.
        let scopes: Vec<_> = ctx.capabilities().iter().map(|cap| &cap.scope).collect();
        assert_eq!(
            scopes,
            [
                &Scope::FsPath("/".into()),
                &Scope::Device("*".into()),
                &Scope::NetHost("*".into()),
                &Scope::FsRecord("*".into()),
            ]
        );
        assert!(
            ctx.capabilities()
                .iter()
                .all(|cap| cap.access == Access::Write)
        );
        assert!(ctx.check(&PermissionScope::governance(
            policy::roles::governance::SETTINGS
        )));
        assert!(ctx.check(&PermissionScope::app_kind(
            AppKind::Terminal,
            ActionKind::Undoable
        )));
        assert!(ctx.check(&PermissionScope::path(
            "/var/workspace/anything",
            Access::Write
        )));
        assert!(ctx.check(&PermissionScope::Sandbox {
            resource: ResourceRef::Device("camera".into()),
            access: Access::Read,
        }));
        assert_eq!(
            ctx.check_with_denied_feedback(&PermissionScope::Sandbox {
                resource: ResourceRef::Device("camera".into()),
                access: Access::Read,
            }),
            None
        );
        let _ = CapDecision::Allow;
    }
}
