use quick_ui::permissions::{
    AppKind, FileId, Op, PermissionContext, PermissionScope, ResourceId,
    Role, RoleId, RolePermissions, UserId, WorkspaceId, DenyReason,
};
use quick_ui::widget::Widget as _;

fn viewer_role() -> Role {
    Role {
        id: RoleId("viewer".into()),
        name: "Viewer".into(),
        permissions: RolePermissions {
            app_access: vec![(AppKind::FileExplorer, Op::View)],
            file_ops: vec![],
            workspace_admin: false,
        },
    }
}

fn editor_role() -> Role {
    Role {
        id: RoleId("editor".into()),
        name: "Editor".into(),
        permissions: RolePermissions {
            app_access: vec![
                (AppKind::FileExplorer, Op::Edit),
                (AppKind::Terminal, Op::Edit),
            ],
            file_ops: vec![(FileId("report.md".into()), Op::Edit)],
            workspace_admin: false,
        },
    }
}

#[test]
fn viewer_cannot_edit_terminal() {
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role(), editor_role()],
        RoleId("viewer".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::Role { .. })
    ));
}

#[test]
fn editor_can_edit_file() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[viewer_role(), editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::File(FileId("report.md".into())),
        op: Op::Edit,
    };
    assert!(ctx.check(&scope));
    assert_eq!(ctx.check_with_denied_feedback(&scope), None);
}

#[test]
fn workspace_admin_requires_admin_flag() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::Workspace(WorkspaceId("ws-42".into())),
        op: Op::Admin,
    };
    assert!(!ctx.check(&scope));
}

#[test]
fn unknown_role_denies_everything() {
    let ctx = PermissionContext::new(
        UserId("x".into()),
        &[viewer_role()],
        RoleId("does-not-exist".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::FileExplorer),
        op: Op::View,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::RoleNotAssigned)
    ));
}

// --- Task 6: Widget trait hooks for presence + permission ---

struct NoOpWidget;
impl quick_ui::widget::Widget for NoOpWidget {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &quick_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn quick_ui::widget::Widget>,
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
        _txt: &mut quick_ui::text_render::TextRenderer,
        _th: &quick_ui::theme::Theme,
        _presence: &quick_ui::presence::PresenceCtx,
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
    fn name(&self) -> &str {
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
    // Compile-time assertion: draw_with_presence is defined on Widget with the
    // expected signature. If this compiles and runs without panic, the default
    // implementation exists and forwards to draw.
    let mut w = NoOpWidget;
    let mut scene = vello::Scene::new();
    let mut tr = quick_ui::text_render::TextRenderer::new();
    let theme = quick_ui::theme::Theme::dark();
    let presence = quick_ui::presence::PresenceCtx::empty();
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

// --- Task 7: Framework-level permission gating in event dispatch ---

use quick_ui::permissions::{
    AppKind as AK, Op as OpX, PermissionScope as PS, ResourceId as RI,
};

struct GatedButton {
    clicked: bool,
    scope: PS,
}

impl quick_ui::widget::Widget for GatedButton {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &quick_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn quick_ui::widget::Widget>,
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
        _txt: &mut quick_ui::text_render::TextRenderer,
        _th: &quick_ui::theme::Theme,
        _presence: &quick_ui::presence::PresenceCtx,
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
    fn name(&self) -> &str {
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
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role()],
        RoleId("viewer".into()),
    );
    let w = GatedButton {
        clicked: false,
        scope: PS {
            resource: RI::App(AK::Terminal),
            op: OpX::Edit,
        },
    };
    let mut fired = false;
    let allowed = quick_ui::app::dispatch_gated_event(&ctx, &w, || {
        fired = true;
    });
    assert!(!allowed);
    assert!(!fired);
}

#[test]
fn gated_widget_allowed_when_role_grants_edit() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[editor_role()],
        RoleId("editor".into()),
    );
    let w = GatedButton {
        clicked: false,
        scope: PS {
            resource: RI::App(AK::Terminal),
            op: OpX::Edit,
        },
    };
    let mut fired = false;
    let allowed = quick_ui::app::dispatch_gated_event(&ctx, &w, || {
        fired = true;
    });
    assert!(allowed);
    assert!(fired);
}
