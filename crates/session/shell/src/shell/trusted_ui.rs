//! Compositor-owned frame for OS-trusted surfaces.
//!
//! Permission prompts and portal sheets are painted by the shell, above any
//! client. Each one carries a seal band in that shell-owned region. The band
//! follows the active theme's accent and spacing so high contrast can recolour
//! it, but it is not a [`Theme`] token and
//! [`ThemeDocument`](ui::theme::ThemeDocument) cannot name it.
//!
//! Automation tells a real surface from an app fake by the target name. That
//! name is inserted only from shell chrome registration.
//! `frame::walk_widget` refuses the same name when a widget tries to publish
//! it through `name`, `test_id`, or `test_rects`.

use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

/// Namespace no widget may publish into the addressable target registry.
pub const FRAME_NAMESPACE: &str = "tilo.shell.trusted-frame";

/// Seal on the inline permission prompt.
pub const PERMISSION_PROMPT_TARGET: &str = "tilo.shell.trusted-frame/permission-prompt";

/// Seal on local one-session microphone consent.
pub const DICTATION_PROMPT_TARGET: &str = "tilo.shell.trusted-frame/dictation-microphone";

/// Seal on a portal sheet (review, authentication, secrets, device choice).
pub const PORTAL_DIALOG_TARGET: &str = "tilo.shell.trusted-frame/portal-dialog";

/// Whether `name` would impersonate a shell trusted frame.
///
/// Matches the namespace itself, `namespace/...`, and `namespace[...]` so a
/// widget `name`, `test_id`, path key, or painted affordance cannot take the
/// slot `list-targets` uses for the real frame.
pub fn is_reserved_target(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(FRAME_NAMESPACE) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/') || rest.starts_with('[')
}

/// Widget-published registry keys. Reserved frame names are dropped.
pub fn widget_may_publish(name: &str) -> bool {
    !is_reserved_target(name)
}

/// How far a horizontal band at `depth` must sit in from a rounded corner
/// so the band stays inside the curve.
fn horizontal_corner_inset(radius: f64, depth: f64) -> f64 {
    if radius <= 0.0 || depth >= radius {
        return 0.0;
    }
    if depth <= 0.0 {
        return radius;
    }
    let from_center = radius - depth;
    let reach = (radius * radius - from_center * from_center).sqrt();
    (radius - reach).max(0.0)
}

/// Reserved band along the top of a shell-owned surface.
///
/// `thickness` and `corner_radius` come from the active theme so the band
/// tracks spacing and corner scale. The band is empty when the surface cannot
/// hold it.
pub fn seal_band(surface: Rect, thickness: f64, corner_radius: f64) -> Rect {
    if thickness <= 0.0 || surface.width() <= 0.0 || surface.height() <= 0.0 {
        return Rect::ZERO;
    }
    let y0 = surface.y0 + thickness;
    let y1 = y0 + thickness;
    if y1 > surface.y1 {
        return Rect::ZERO;
    }
    let inset = horizontal_corner_inset(corner_radius, thickness);
    let x0 = surface.x0 + inset;
    let x1 = surface.x1 - inset;
    if x1 <= x0 {
        return Rect::ZERO;
    }
    Rect::new(x0, y0, x1, y1)
}

/// Paint the seal. Callers are shell chrome, after the surface fill and
/// before the surface's own labels.
pub fn paint_seal(
    scene: &mut Scene,
    transform: Affine,
    surface: Rect,
    theme: &Theme,
    corner_radius: f64,
) {
    let band = seal_band(surface, f64::from(theme.tokens.spacing.s1), corner_radius);
    if band.width() <= 0.0 || band.height() <= 0.0 {
        return;
    }
    // The seal wears the person's own colour: an app cannot know it, so it
    // cannot paint a convincing copy.
    let ink = theme.tokens.colors.intent.you;
    scene.fill(Fill::NonZero, transform, ink, None, &band);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::PendingPrompt;
    use crate::shell::frame::{FrameBuilder, walk_widget};
    use crate::shell::portal_dialog::{PortalDialogQueue, ReviewRequest};
    use crate::shell::state::{Capability, GRANT_ONCE};
    use std::collections::HashMap;
    use taffy::{NodeId, Style, TaffyTree};
    use ui::widget::TaffyTreeExt;
    use ui::widget::Widget;

    struct Forgery {
        rect: Rect,
        test_id: Option<&'static str>,
        rects: Vec<(String, Rect)>,
        name: &'static str,
    }

    impl Widget for Forgery {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), node_map, self)
        }

        fn name(&self) -> &str {
            self.name
        }

        fn test_id(&self) -> Option<&str> {
            self.test_id
        }

        fn test_rects(&self) -> Vec<(String, Rect)> {
            self.rects.clone()
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn published(widget: &Forgery) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        let mut counts = HashMap::new();
        walk_widget(widget, "", &mut counts, &mut out);
        out
    }

    #[test]
    fn an_app_widget_cannot_mint_the_trusted_frame() {
        let rect = Rect::new(10.0, 20.0, 400.0, 100.0);
        let forged = Forgery {
            rect,
            name: PERMISSION_PROMPT_TARGET,
            test_id: Some(PORTAL_DIALOG_TARGET),
            rects: vec![
                (FRAME_NAMESPACE.to_string(), rect),
                (PERMISSION_PROMPT_TARGET.to_string(), rect),
                (format!("{FRAME_NAMESPACE}[0]"), rect),
            ],
        };
        let registry = published(&forged);
        assert!(
            registry.keys().all(|name| !is_reserved_target(name)),
            "widget walk published a reserved frame: {registry:?}"
        );
        assert!(
            registry.is_empty(),
            "every key this widget can mint is inside the reserved namespace"
        );

        let prompt = ui::widget::PermissionPrompt::for_owner("org.example.App", "Camera");
        assert_eq!(Widget::test_id(&prompt), None);
        assert!(Widget::test_rects(&prompt).is_empty());
    }

    #[test]
    fn seal_band_stays_inside_the_shell_surface() {
        let surface = Rect::new(40.0, 80.0, 640.0, 160.0);
        let thickness = 4.0;
        let band = seal_band(surface, thickness, 24.0);
        assert!(band.x0 > surface.x0);
        assert!(band.x1 < surface.x1);
        assert!(band.y0 > surface.y0);
        assert!(band.y1 < surface.y1);
        assert!((band.height() - thickness).abs() < 0.01);
        assert_eq!(
            seal_band(Rect::new(0.0, 0.0, 2.0, 2.0), thickness, 8.0).area(),
            0.0
        );
    }

    /// The prompt holds the keyboard while it is up, so the window behind it
    /// never sees Escape: the shell must answer the prompt with it, as Deny.
    /// A Terminal asking for the device's own shell was left in front of a
    /// prompt that no key could close, and its next session asked again.
    #[test]
    fn escape_answers_the_permission_prompt_as_its_deny_button() {
        use winit::keyboard::{Key, NamedKey};
        let mut frame = FrameBuilder::new((1280, 800));
        frame.relayout();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        frame
            .chrome_handle()
            .borrow_mut()
            .pending_grants
            .push_back(PendingPrompt::with_reply(
                "so.tilo.terminal".to_string(),
                Capability::HostShell {
                    scope: crate::shell::state::GRANT_ALWAYS,
                },
                reply,
            ));
        frame.host_mut().tick_permission_prompt_for_tests();
        frame.relayout();
        assert!(
            frame
                .widget_registry()
                .contains_key(PERMISSION_PROMPT_TARGET),
            "the prompt is on screen"
        );
        assert!(
            crate::shell::input_policy::chrome_captures_input(&frame),
            "the prompt holds the keyboard, so Escape comes to the shell"
        );
        let mut doc = automerge::AutoCommit::new();
        let mut focus = None;
        for pressed in [true, false] {
            frame.dispatch_event(
                &crate::shell::test_control::keyboard_input_event(
                    &Key::Named(NamedKey::Escape),
                    pressed,
                ),
                &winit::event::Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            );
        }
        assert_eq!(
            answer.try_recv().ok(),
            Some(crate::portal::PromptOutcome::Deny),
            "the app waiting on the prompt hears it was not allowed"
        );
        assert!(frame.chrome_handle().borrow().pending_grants.is_empty());
        frame.host_mut().tick_permission_prompt_for_tests();
        frame.relayout();
        let prompt_shown = frame
            .widget_registry()
            .contains_key(PERMISSION_PROMPT_TARGET);
        let keyboard_held = crate::shell::input_policy::chrome_captures_input(&frame);
        drop(frame);
        assert!(!prompt_shown, "the prompt is gone");
        assert!(!keyboard_held, "and the keyboard goes back to the window");
    }

    #[test]
    fn the_shell_registry_publishes_real_frames_only() {
        let mut frame = FrameBuilder::new((1280, 800));
        frame.relayout();
        let before = frame.widget_registry();
        assert!(
            before.keys().all(|name| !is_reserved_target(name)),
            "a desktop with no trusted surface published {before:?}"
        );

        frame
            .chrome_handle()
            .borrow_mut()
            .pending_grants
            .push_back(PendingPrompt::new(
                "org.example.App".to_string(),
                Capability::Camera { scope: GRANT_ONCE },
            ));
        frame.host_mut().tick_permission_prompt_for_tests();
        frame.relayout();
        let with_prompt = frame.widget_registry();
        let prompt = with_prompt
            .get(PERMISSION_PROMPT_TARGET)
            .copied()
            .expect("the permission prompt's seal is addressable");
        assert!(prompt.area() > 0.0);
        assert!(!with_prompt.contains_key(PORTAL_DIALOG_TARGET));

        let queue = PortalDialogQueue::default();
        queue
            .enqueue_review(ReviewRequest {
                handle: "/trusted-ui/review".into(),
                app_id: "org.example.App".into(),
                title: "Share".into(),
                reason: "Send the archive.".into(),
                resource: "archive.zip".into(),
                confirm_label: "Share".into(),
            })
            .expect("review queues");
        frame.host_mut().set_portal_dialog_queue(queue);
        assert!(frame.host_mut().pump_portal_dialogs());
        let with_portal = frame.widget_registry();
        drop(frame);
        let portal = with_portal
            .get(PORTAL_DIALOG_TARGET)
            .copied()
            .expect("the portal sheet's seal is addressable");
        assert!(portal.area() > 0.0);
        assert!(with_portal.contains_key(PERMISSION_PROMPT_TARGET));
    }
}
