//! The bodies of [`Widget`]'s default methods that walk a widget's children.
//!
//! A default method is compiled again for every widget type that keeps it,
//! in every crate that builds that type's vtable: each app compiled each of
//! these walks once per toolkit and app widget it boxes, with the iterator
//! adapters and closures in them. Here they are compiled once, into the
//! toolkit; a default keeps only the part that names `Self` (asking for its
//! children, or what it answers itself) and hands that over.
//!
//! The functions are not generic and take the children as trait objects, so
//! every app links them from the shared runtime.

use automerge::ObjId;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{AppModel, ModelPath, Widget};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;

/// A widget's children, read-only: [`Widget::for_each_child`] of the widget
/// a default method runs for.
pub(super) type EachChild<'a> = &'a dyn Fn(&mut dyn FnMut(&dyn Widget));

/// A widget's children, mutably: [`Widget::children_mut`].
pub(super) type ChildrenMut<'a> = Vec<&'a mut (dyn Widget + 'a)>;

pub(super) fn visit_components(
    children: ChildrenMut<'_>,
    request: &mut crate::bound_dispatch::ComponentRequest<'_>,
) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
    for child in children {
        if request.target.is_some() && request.found {
            break;
        }
        child.visit_components(request)?;
    }
    Ok(())
}

pub(super) fn visit_resource_owners_mut(
    children: ChildrenMut<'_>,
    request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
) -> Result<(), tilo_actions::DoorError> {
    for child in children {
        request.visit(child)?;
    }
    Ok(())
}

pub(super) fn mount_native_views(
    children: ChildrenMut<'_>,
    host: &crate::widget::native_view::NativeViewHost<'_>,
) {
    for child in children {
        child.mount_native_views(host);
    }
}

pub(super) fn unmount_native_views(children: ChildrenMut<'_>) {
    for child in children {
        child.unmount_native_views();
    }
}

pub(super) fn native_view_frame_start(children: ChildrenMut<'_>) {
    for child in children {
        child.native_view_frame_start();
    }
}

pub(super) fn set_platform_conventions(
    children: ChildrenMut<'_>,
    conventions: crate::platform::PlatformConventions,
) {
    for child in children {
        child.set_platform_conventions(conventions);
    }
}

pub(super) fn set_interaction_environment(
    children: ChildrenMut<'_>,
    environment: crate::interaction::InteractionEnvironment,
) {
    for child in children {
        child.set_interaction_environment(environment);
    }
}

pub(super) fn sync(children: ChildrenMut<'_>, state: &AppModel, path: &ModelPath) {
    for child in children {
        child.sync(state, path);
    }
}

pub(super) fn update(children: ChildrenMut<'_>, state: &AppModel, path: &ModelPath) -> bool {
    let mut changed = false;
    for child in children {
        changed |= child.update(state, path);
    }
    changed
}

pub(super) fn settle(children: ChildrenMut<'_>, edits: &mut super::Edits<'_>, path: &ObjId) {
    for child in children {
        child.settle(edits, path);
    }
}

pub(super) fn for_each_child(children: Vec<&dyn Widget>, visitor: &mut dyn FnMut(&dyn Widget)) {
    for child in children {
        visitor(child);
    }
}

pub(super) fn reveal_descendant(children: ChildrenMut<'_>, target: *const dyn Widget) -> bool {
    children.into_iter().any(|child| {
        std::ptr::addr_eq(std::ptr::from_ref::<dyn Widget>(child), target)
            || child.reveal_descendant(target)
    })
}

pub(super) fn take_action_palette_request(
    children: ChildrenMut<'_>,
) -> Option<tilo_actions::ActionAddress> {
    children
        .into_iter()
        .find_map(|child| child.take_action_palette_request())
}

pub(super) fn take_search_request(children: ChildrenMut<'_>) -> bool {
    children
        .into_iter()
        .any(|child| child.take_search_request())
}

pub(super) fn focus_requested(
    children: ChildrenMut<'_>,
    target: Option<NodeId>,
    tree: &TaffyTree<()>,
) {
    for child in children {
        child.focus_requested(target, tree);
    }
}

/// The text-key-action path [`Widget::perform_semantic_action`] decodes: the
/// key path a `tilo.key-action:` target names, or `None` for any other
/// target.
pub(super) fn key_action_path(target: &str) -> Option<Vec<u64>> {
    // `AccessNode::with_keyed_action_routes` publishes a custom-painted
    // control through the same semantic target that search and agents
    // consume. Decode that target here, at the Widget boundary, so the
    // app author implements `perform_action` once and gets every input
    // route promised by the adapter. Explicit semantic targets keep using
    // their widget-specific implementations because they do not carry
    // this reserved prefix.
    let (_, encoded_path) = target
        .strip_prefix("tilo.key-action:")
        .and_then(|target| target.rsplit_once(':'))?;
    let key_path = encoded_path
        .split('/')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!key_path.is_empty()).then_some(key_path)
}

pub(super) fn ime_requires_deferred_ack(caret: Option<Rect>, each: EachChild<'_>) -> bool {
    let mut required = false;
    each(&mut |child| {
        if child.ime_cursor_rect() == caret && child.ime_content().is_some() {
            required |= child.ime_requires_deferred_ack();
        }
    });
    required
}

pub(super) fn ime_refuses_ordinary_commits(caret: Option<Rect>, each: EachChild<'_>) -> bool {
    let mut refused = false;
    each(&mut |child| {
        if child.ime_cursor_rect() == caret && child.ime_content().is_some() {
            refused |= child.ime_refuses_ordinary_commits();
        }
    });
    refused
}

fn typing_child<'a>(children: ChildrenMut<'a>, caret: Rect) -> Option<&'a mut (dyn Widget + 'a)> {
    children
        .into_iter()
        .find(|child| child.ime_cursor_rect() == Some(caret) && child.ime_content().is_some())
}

pub(super) fn start_deferred_text(
    children: ChildrenMut<'_>,
    caret: Rect,
) -> Option<crate::webview::text_input::Receipt> {
    typing_child(children, caret)?.start_deferred_text()
}

pub(super) fn commit_deferred_text(
    children: ChildrenMut<'_>,
    caret: Rect,
    text: &str,
) -> Option<crate::webview::text_input::Receipt> {
    typing_child(children, caret)?.commit_deferred_text(text)
}

pub(super) fn ime_multiline(caret: Rect, each: EachChild<'_>) -> bool {
    let mut matching = 0_usize;
    let mut multiline = false;
    each(&mut |child| {
        if child.ime_cursor_rect() == Some(caret)
            && child.ime_content().is_some_and(|content| {
                !content.is_secret() && content.purpose != crate::ime::InputPurpose::Terminal
            })
        {
            matching = matching.saturating_add(1);
            multiline = child.ime_multiline();
        }
    });
    matching == 1 && multiline
}

pub(super) fn ime_edit_fingerprint(
    caret: Rect,
    key: &[u8; 32],
    each: EachChild<'_>,
    own: &dyn Fn() -> Option<crate::a11y::AccessNode>,
) -> Option<[u8; 32]> {
    let mut index = 0_usize;
    let mut selected = None;
    let mut ambiguous = false;
    each(&mut |child| {
        if child.ime_cursor_rect() == Some(caret) && child.ime_content().is_some() {
            if selected.is_some() {
                ambiguous = true;
            }
            selected = Some(
                child
                    .ime_edit_fingerprint(key)
                    .map(|stamp| crate::ime::scoped_fingerprint(&index.to_le_bytes(), stamp, key)),
            );
        }
        index = index.saturating_add(1);
    });
    if ambiguous {
        return None;
    }
    if let Some(selected) = selected {
        return selected;
    }
    let node = own()?;
    if node.state.protected || node.state.read_only {
        return None;
    }
    let selection = node.text_selection?;
    Some(crate::ime::edit_fingerprint(
        &node.value?,
        selection.anchor,
        selection.focus,
        key,
    ))
}

pub(super) fn on_nav_gesture(
    children: ChildrenMut<'_>,
    dir: crate::gesture::NavDirection,
    state: &AppModel,
    path: &ModelPath,
) -> bool {
    children
        .into_iter()
        .any(|child| child.on_nav_gesture(dir, state, path))
}

pub(super) fn nav_availability(children: Vec<&dyn Widget>) -> crate::gesture::NavAvailability {
    children
        .into_iter()
        .fold(crate::gesture::NavAvailability::default(), |seen, child| {
            seen.or(child.nav_availability())
        })
}

pub(super) fn take_accessibility_request(
    children: ChildrenMut<'_>,
) -> Option<crate::a11y::AccessibilityRequest> {
    children
        .into_iter()
        .find_map(|child| child.take_accessibility_request())
}

pub(super) fn take_app_activation(
    children: ChildrenMut<'_>,
) -> Option<crate::application::InstanceId> {
    children
        .into_iter()
        .find_map(|child| child.take_app_activation())
}

pub(super) fn dev_route(children: Vec<&dyn Widget>) -> Option<super::router::RouteSnapshot> {
    children.into_iter().find_map(|child| child.dev_route())
}

pub(super) fn restore_dev_route(
    children: ChildrenMut<'_>,
    route: &super::router::RouteSnapshot,
) -> bool {
    children
        .into_iter()
        .any(|child| child.restore_dev_route(route))
}

pub(super) fn set_motion_reduced(children: ChildrenMut<'_>, reduced: bool) {
    for child in children {
        child.set_motion_reduced(reduced);
    }
}

pub(super) fn needs_animation_frame(each: EachChild<'_>) -> bool {
    let mut needed = false;
    each(&mut |child| {
        if !needed {
            needed = child.needs_animation_frame();
        }
    });
    needed
}

pub(super) fn next_wake_deadline(each: EachChild<'_>) -> Option<Instant> {
    let mut earliest = None;
    each(&mut |child| {
        if let Some(deadline) = child.next_wake_deadline() {
            earliest = Some(earliest.map_or(deadline, |old: Instant| old.min(deadline)));
        }
    });
    earliest
}

pub(super) fn holds_frame_until(each: EachChild<'_>, now: Instant) -> Option<Instant> {
    let mut latest = None;
    each(&mut |child| {
        if let Some(until) = child.holds_frame_until(now) {
            latest = Some(latest.map_or(until, |old: Instant| old.max(until)));
        }
    });
    latest
}

pub(super) fn modal_capture(each: EachChild<'_>) -> Option<Rect> {
    let mut capture = None;
    each(&mut |child| {
        if capture.is_none() {
            capture = child.modal_capture();
        }
    });
    capture
}

pub(super) fn take_structure_dirty(children: ChildrenMut<'_>) -> bool {
    let mut dirty = false;
    for child in children {
        dirty |= child.take_structure_dirty();
    }
    dirty
}

pub(super) fn has_pending_visual_work(each: EachChild<'_>) -> bool {
    let mut pending = false;
    each(&mut |child| pending |= child.has_pending_visual_work());
    pending
}

pub(super) fn has_transient_feedback(children: Vec<&dyn Widget>) -> bool {
    children
        .into_iter()
        .any(|child| child.has_transient_feedback())
}

/// Whether any child answers `dirty`; `None` for a widget without children.
fn any_child(each: EachChild<'_>, dirty: fn(&dyn Widget) -> bool) -> Option<bool> {
    let mut has_children = false;
    let mut found = false;
    each(&mut |child| {
        has_children = true;
        if !found {
            found = dirty(child);
        }
    });
    has_children.then_some(found)
}

pub(super) fn needs_redraw(each: EachChild<'_>) -> bool {
    any_child(each, |child| child.needs_redraw()).unwrap_or(true)
}

pub(super) fn is_layout_dirty(each: EachChild<'_>) -> bool {
    any_child(each, |child| child.is_layout_dirty()).unwrap_or(true)
}

/// [`Widget::layout_needs_measure`] of the children; `None` without any.
pub(super) fn layout_needs_measure(each: EachChild<'_>) -> Option<bool> {
    any_child(each, |child| child.layout_needs_measure())
}

pub(super) fn mark_drawn(children: ChildrenMut<'_>) {
    for child in children {
        child.mark_drawn();
    }
}

pub(super) fn release_scene_cache(children: ChildrenMut<'_>) {
    for child in children {
        child.release_scene_cache();
    }
}

pub(super) fn draw_overlay(
    children: ChildrenMut<'_>,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    for child in children {
        child.draw_overlay(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keyed action target decodes to its key path, and nothing else does.
    #[test]
    fn a_key_action_target_decodes_to_its_key_path() {
        assert_eq!(
            key_action_path("tilo.key-action:board:3/14/15"),
            Some(vec![3, 14, 15])
        );
        assert_eq!(key_action_path("tilo.key-action:board:"), None);
        assert_eq!(key_action_path("tilo.key-action:board:3/x"), None);
        assert_eq!(key_action_path("tilo.key-action:no-path"), None);
        assert_eq!(key_action_path("dialog:1:primary"), None);
    }

    /// A widget without children asks for a redraw and counts as laid out
    /// anew; with children it answers what they answer.
    #[test]
    fn a_childless_widget_is_dirty_and_a_container_answers_for_its_children() {
        let none: EachChild<'_> = &|_| {};
        assert!(needs_redraw(none));
        assert!(is_layout_dirty(none));
        assert_eq!(layout_needs_measure(none), None);
        let label = crate::widget::Text::new("x");
        let one: EachChild<'_> = &|visit| visit(&label);
        assert_eq!(
            needs_redraw(one),
            crate::widget::Widget::needs_redraw(&label)
        );
        assert_eq!(
            layout_needs_measure(one),
            Some(crate::widget::Widget::layout_needs_measure(&label))
        );
    }
}
