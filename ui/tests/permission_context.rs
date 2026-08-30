use ui::permissions::{
    Actor, AppKind, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef,
    Role, Scope, Subject, UserId, Verb,
};
use ui::widget::Widget as _;

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

#[test]
fn viewer_cannot_edit_terminal() {
    let ctx = viewer_ctx("alice");
    let scope = PermissionScope::app_kind(AppKind::Terminal, Verb::Write);
    assert!(!ctx.check(&scope));
    assert_eq!(
        ctx.check_with_denied_feedback(&scope),
        Some(CapDecision::DeniedByRole)
    );
}

#[test]
fn editor_can_edit_file() {
    let ctx = editor_ctx("bob");
    let scope = PermissionScope::path("/var/workspace/report.md", Verb::Write);
    assert!(ctx.check(&scope));
    assert_eq!(ctx.check_with_denied_feedback(&scope), None);
}

#[test]
fn a_file_share_does_not_leak_to_its_siblings() {
    // The grant is scoped to one path. Component-aligned prefix
    // matching must not let it cover a sibling that merely shares a
    // textual prefix.
    let ctx = editor_ctx("bob");
    assert!(!ctx.check(&PermissionScope::path(
        "/var/workspace/report.md.bak",
        Verb::Write
    )));
    assert!(!ctx.check(&PermissionScope::path(
        "/var/workspace/payroll.md",
        Verb::Write
    )));
}

#[test]
fn workspace_admin_requires_admin_authority() {
    let ctx = editor_ctx("bob");
    let scope = PermissionScope::workspace(Verb::Admin);
    assert!(!ctx.check(&scope));
    assert_eq!(
        ctx.check_with_denied_feedback(&scope),
        Some(CapDecision::DeniedByRole)
    );
    // An Editor's ceiling is Write; writing the room is still fine.
    assert!(ctx.check(&PermissionScope::workspace(Verb::Write)));
}

#[test]
fn actor_with_no_role_denies_everything() {
    // An actor with no role has no base authority at all, so absent an
    // explicit capability it is denied — fail-closed by construction.
    let ctx = PermissionContext::from_capabilities(Actor::person(None, "x"), vec![]);
    let scope = PermissionScope::app_kind(AppKind::FileExplorer, Verb::Read);
    assert!(!ctx.check(&scope));
    assert_eq!(
        ctx.check_with_denied_feedback(&scope),
        Some(CapDecision::DeniedByRole)
    );
    assert!(!ctx.check(&PermissionScope::workspace(Verb::Read)));
    assert!(!ctx.check(&PermissionScope::path("/var/workspace/a.txt", Verb::Read)));
}

#[test]
fn an_explicitly_denied_role_cannot_be_rescued_by_a_grant() {
    // Role::Denied is a block, not an absence. A share must not undo it.
    let ctx = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Denied), "mallory"),
        vec![CapOverride::grant(
            Subject::person("mallory"),
            Scope::Workspace,
            Verb::Admin,
        )],
    );
    assert!(!ctx.check(&PermissionScope::workspace(Verb::Read)));
    assert_eq!(
        ctx.check_with_denied_feedback(&PermissionScope::workspace(Verb::Read)),
        Some(CapDecision::DeniedByRole)
    );
}

struct NoOpWidget;
impl ui::widget::Widget for NoOpWidget {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
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
        _txt: &mut ui::text_render::TextRenderer,
        _th: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &winit::event::WindowEvent,
        _sf: f64,
        _mods: &winit::event::Modifiers,
        _s: &mut automerge::AutoCommit,
        _p: &automerge::ObjId,
        _h: &mut bool,
        _ft: &mut Option<taffy::NodeId>,
        _nid: taffy::NodeId,
        _t: &taffy::TaffyTree<()>,
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
fn widget_default_presence_and_permission_are_none() {
    let w = NoOpWidget;
    assert!(w.presence_key().is_none());
    assert!(w.permission_scope().is_none());
}

#[test]
fn draw_with_presence_default_delegates_to_draw() {
    // If this compiles and runs without panic, the default
    // `draw_with_presence` implementation exists and forwards to `draw`.
    let mut w = NoOpWidget;
    let mut scene = vello::Scene::new();
    let mut tr = ui::text_render::TextRenderer::new();
    let theme = ui::theme::Theme::dark();
    let presence = ui::presence::PresenceCtx::empty();
    let perms = PermissionContext::permissive();
    w.draw_with_presence(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
        &perms,
    );
}

use ui::permissions::PermissionScope as PS;

struct GatedButton {
    clicked: bool,
    scope: PS,
}

impl ui::widget::Widget for GatedButton {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
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
        _txt: &mut ui::text_render::TextRenderer,
        _th: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _e: &winit::event::WindowEvent,
        _sf: f64,
        _mods: &winit::event::Modifiers,
        _s: &mut automerge::AutoCommit,
        _p: &automerge::ObjId,
        _h: &mut bool,
        _ft: &mut Option<taffy::NodeId>,
        _nid: taffy::NodeId,
        _t: &taffy::TaffyTree<()>,
    ) {
        self.clicked = true;
    }
    fn name(&self) -> &'static str {
        "gated"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn permission_scope(&self) -> Option<PS> {
        Some(self.scope.clone())
    }
}

#[test]
fn gated_widget_rejected_by_framework_helper() {
    let ctx = viewer_ctx("alice");
    let w = GatedButton {
        clicked: false,
        scope: PermissionScope::app_kind(AppKind::Terminal, Verb::Write),
    };
    let mut fired = false;
    let allowed = ui::app::dispatch_gated_event(&ctx, &w, || {
        fired = true;
    });
    assert!(!allowed);
    assert!(!fired);
}

#[test]
fn gated_widget_allowed_when_role_grants_edit() {
    let ctx = editor_ctx("bob");
    let w = GatedButton {
        clicked: false,
        scope: PermissionScope::app_kind(AppKind::Terminal, Verb::Write),
    };
    let mut fired = false;
    let allowed = ui::app::dispatch_gated_event(&ctx, &w, || {
        fired = true;
    });
    assert!(allowed);
    assert!(fired);
}

#[test]
fn permission_context_built_from_an_actor_and_its_capabilities() {
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

#[test]
fn a_lapsed_capability_stops_granting() {
    let cap = CapOverride::grant(
        Subject::person("alice"),
        Scope::App("terminal".into()),
        Verb::Admin,
    )
    .with_expiry(2_000);
    let actor = Actor::person(Some(Role::Editor), "alice");
    let scope = PermissionScope::app_kind(AppKind::Terminal, Verb::Admin);

    let live =
        PermissionContext::from_capabilities(actor.clone(), vec![cap.clone()]).at_unix_time(1_999);
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

#[test]
fn permissive_holds_one_real_workspace_wide_admin_capability() {
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
