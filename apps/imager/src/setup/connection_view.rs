//! Retained Connected management presentation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::Politeness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::typography::TextRole;
use ui::widget::{Button, Card, Column, Container, Row, Scrollable, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{Modifiers, WindowEvent};

use super::connection_actions::{self, ConnectionOperation, ManagementPhase, ManagementProjection};
use super::connection_controller::{ConnectionController, ControllerInvocation};
use super::{page, stack, strings};
use crate::rhythm::px::{PAGE_GUTTER, SCROLLBAR_GUTTER, SIBLING};

fn push_endpoint_char(output: &mut String, character: char) {
    let code = u32::from(character);
    let formatting =
        matches!(code, 0x061c | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x206f | 0xfeff);
    if character == '\\' {
        output.push_str("\\\\");
    } else if character.is_control() || formatting {
        let _ = write!(output, "\\u{{{code:04X}}}");
    } else {
        output.push(character);
    }
}

#[cfg(unix)]
fn format_endpoint_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut remaining = path.as_os_str().as_bytes();
    let mut output = String::new();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                valid
                    .chars()
                    .for_each(|character| push_endpoint_char(&mut output, character));
                break;
            }
            Err(error) => {
                let valid = &remaining[..error.valid_up_to()];
                std::str::from_utf8(valid)
                    .expect("UTF-8 validator identifies a valid prefix")
                    .chars()
                    .for_each(|character| push_endpoint_char(&mut output, character));
                let invalid = error
                    .error_len()
                    .unwrap_or(remaining.len() - error.valid_up_to());
                for byte in &remaining[error.valid_up_to()..error.valid_up_to() + invalid] {
                    let _ = write!(output, "\\x{byte:02X}");
                }
                remaining = &remaining[error.valid_up_to() + invalid..];
            }
        }
    }
    output
}

#[cfg(windows)]
fn format_endpoint_path(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt as _;
    let mut output = String::new();
    for decoded in char::decode_utf16(path.as_os_str().encode_wide()) {
        match decoded {
            Ok(character) => push_endpoint_char(&mut output, character),
            Err(error) => {
                let _ = write!(output, "\\u{{{:04X}}}", error.unpaired_surrogate());
            }
        }
    }
    output
}

/// One retained presentation over one retained controller.
pub struct ConnectionView {
    controller: ConnectionController,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl ConnectionView {
    #[must_use]
    pub fn new(generation: u64) -> Self {
        Self {
            controller: ConnectionController::new(generation),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn for_shot(scenario: &str) -> Self {
        Self {
            controller: ConnectionController::for_shot(scenario),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.controller.generation()
    }

    pub fn set_generation(&mut self, generation: u64) {
        self.controller.set_generation(generation);
    }

    #[cfg(test)]
    pub(super) fn for_controlled_open(generation: u64) -> Self {
        Self {
            controller: ConnectionController::controlled_fixture_open(generation),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
    fn for_controlled_many_catalog() -> Self {
        Self {
            controller: ConnectionController::controlled_many_catalog(),
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
    fn hold_pending_for_test(&mut self) {
        self.controller.hold_pending();
    }

    #[cfg(test)]
    fn deliver_pending_error_for_test(&mut self, error: ui::folders::connected::ConnectionError) {
        self.controller.deliver_pending_error(error);
    }

    #[cfg(test)]
    fn inject_submission_error_for_test(&mut self, error: ui::folders::connected::ConnectionError) {
        self.controller.inject_submission_error(error);
    }

    #[cfg(test)]
    pub(super) const fn service_submission_count(&self) -> usize {
        self.controller.service_submission_count()
    }

    #[cfg(test)]
    pub(super) const fn opener_pending_for_test(&self) -> bool {
        self.controller.opener_pending()
    }

    #[must_use]
    pub fn projection(&self) -> ManagementProjection {
        self.controller.projection()
    }

    #[must_use]
    pub fn resource(&self) -> tilo_actions::Resource {
        connection_actions::resource(&self.projection())
    }

    /// Invoke one typed operation after validating its semantic door.
    pub fn invoke_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        operation: ConnectionOperation,
    ) -> Result<ControllerInvocation, String> {
        connection_actions::validate_kind(&operation, kind)?;
        self.controller.invoke(operation)
    }

    fn button(resource: &tilo_actions::Resource, action_id: &str, id: &str) -> Button {
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .expect("Connected composition binds only exported actions");
        let binding = tilo_actions::ActionBinding::unit_for(resource, action_id)
            .expect("Connected actions are nullary");
        Button::new(id, &action.label, None)
            .on_action(binding)
            .with_enabled(action.contract.availability.is_available())
    }

    fn status_text(code: &str) -> String {
        let key = match code {
            "not_open" => "imager-connected-status-not-open",
            "loading" => "imager-connected-status-loading",
            "loading_catalog" => "imager-connected-status-loading-catalog",
            "opening_settings" => "imager-connected-status-opening-settings",
            "empty" => "imager-connected-status-empty",
            "catalog" => "imager-connected-status-catalog",
            "unsupported_host" => "imager-connected-status-unsupported-host",
            "busy" => "imager-connected-status-busy",
            "closed" => "imager-connected-status-closed",
            "pending_not_compared" => "imager-connected-status-pending-not-compared",
            "pending_incomplete_observation" => {
                "imager-connected-status-pending-incomplete-observation"
            }
            "pending_native_refused" => "imager-connected-status-pending-native-refused",
            "pending_structural" => "imager-connected-status-pending-structural",
            "pending_run_limit" => "imager-connected-status-pending-run-limit",
            "scanning" => "imager-connected-status-scanning",
            "syncing" => "imager-connected-status-syncing",
            "awaiting_review" => "imager-connected-status-awaiting-review",
            "up_to_date" => "imager-connected-status-up-to-date",
            "conflict" => "imager-connected-status-conflict",
            "paused" => "imager-connected-status-paused",
            "disconnecting" => "imager-connected-status-disconnecting",
            "disconnected" => "imager-connected-status-disconnected",
            "detached" => "imager-connected-status-detached",
            other if other.starts_with("unavailable_source_") => {
                return strings::t_args(
                    "imager-connected-status-unavailable",
                    &[
                        ("side", Self::side_text("source").into()),
                        (
                            "error",
                            Self::error_text(other.trim_start_matches("unavailable_source_"))
                                .into(),
                        ),
                    ],
                );
            }
            other if other.starts_with("unavailable_destination_") => {
                return strings::t_args(
                    "imager-connected-status-unavailable",
                    &[
                        ("side", Self::side_text("destination").into()),
                        (
                            "error",
                            Self::error_text(other.trim_start_matches("unavailable_destination_"))
                                .into(),
                        ),
                    ],
                );
            }
            other if other.starts_with("recovery_required_") => {
                return strings::t_args(
                    "imager-connected-status-recovery-required",
                    &[(
                        "error",
                        Self::error_text(other.trim_start_matches("recovery_required_")).into(),
                    )],
                );
            }
            other => {
                return strings::t_args(
                    "imager-connected-status-other-error",
                    &[("detail", Self::error_text(other).into())],
                );
            }
        };
        strings::t(key)
    }

    fn side_text(code: &str) -> String {
        strings::t(match code {
            "source" => "imager-connected-side-source",
            "destination" => "imager-connected-side-destination",
            _ => "imager-connected-side-source",
        })
    }

    fn error_text(code: &str) -> String {
        strings::t(match code {
            "unsupported_host" => "imager-connected-reason-unsupported-host",
            "busy" => "imager-connected-reason-busy",
            "closed" => "imager-connected-reason-closed",
            "limit_exceeded" => "imager-connected-reason-limit-exceeded",
            "stale_review" => "imager-connected-reason-stale-review",
            "disconnected" => "imager-connected-reason-disconnected",
            "identity_changed" => "imager-connected-reason-identity-changed",
            "unavailable" => "imager-connected-reason-unavailable",
            "permission_denied" => "imager-connected-reason-permission-denied",
            "unsafe_volume" => "imager-connected-reason-unsafe-volume",
            "read_only" => "imager-connected-reason-read-only",
            "unverified" => "imager-connected-reason-unverified",
            "unstable" => "imager-connected-reason-unstable",
            "recovery_required" => "imager-connected-reason-recovery-required",
            "recovery_space" => "imager-connected-reason-recovery-space",
            "corrupt_store" => "imager-connected-reason-corrupt-store",
            "persistence" => "imager-connected-reason-persistence",
            _ => "imager-connected-reason-unknown",
        })
    }

    fn endpoint_issue_text(side: &str, error: &str) -> String {
        strings::t_args(
            "imager-connected-endpoint-issue",
            &[
                ("side", Self::side_text(side).into()),
                ("error", Self::error_text(error).into()),
            ],
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let state = self.projection();
        let resource = connection_actions::resource(&state);
        let header = stack()
            .push(Box::new(
                Text::new(&strings::t("imager-connected-title"))
                    .with_role(TextRole::Title)
                    .with_heading_level(1),
            ))
            .push(Box::new(
                Text::new(&strings::t("imager-connected-intro")).dimmed(),
            ))
            .push(Box::new(Row::new().push(Box::new(Self::button(
                &resource,
                "back",
                "connected.back",
            )))));
        let mut body = page(header);

        match state.phase {
            ManagementPhase::Loading => {
                body = body.push(Box::new(Card::titled(
                    strings::t("imager-connected-loading-title"),
                    Box::new(Text::new(&strings::t("imager-connected-loading-body"))),
                )));
            }
            ManagementPhase::Unavailable => {
                body = body.push(Box::new(Card::titled(
                    strings::t("imager-connected-unavailable-title"),
                    Box::new(Text::new(&strings::t("imager-connected-unavailable-body"))),
                )));
            }
            ManagementPhase::Empty => {
                body = body.push(Box::new(Card::titled(
                    strings::t("imager-connected-empty-title"),
                    Box::new(Text::new(&strings::t("imager-connected-empty-body"))),
                )));
            }
            ManagementPhase::Catalog | ManagementPhase::Settings => {
                let mut rows = stack();
                for (projection, row_resource) in state.rows.iter().zip(&resource.children) {
                    let details = connection_actions::row_detail(projection);
                    rows = rows.push(Box::new(Card::titled_with_action(
                        strings::t_args(
                            "imager-connected-row-title",
                            &[("number", projection.number.into())],
                        ),
                        Box::new(Self::button(
                            row_resource,
                            "open-settings",
                            &format!("connected.row.{}.open", projection.alias),
                        )),
                        Box::new(Text::new(&details)),
                    )));
                }
                if let (Some(selected), Some((source, destination))) =
                    (state.selected.as_ref(), self.controller.selected_paths())
                    && let Some(row_resource) = resource
                        .children
                        .iter()
                        .find(|child| child.id.0.ends_with(&format!(".{}", selected.alias)))
                {
                    let mut settings = stack()
                        .push(Box::new(Text::new(&strings::t_args(
                            "imager-connected-source",
                            &[("path", format_endpoint_path(source).into())],
                        ))))
                        .push(Box::new(Text::new(&strings::t_args(
                            "imager-connected-destination",
                            &[("path", format_endpoint_path(destination).into())],
                        ))))
                        .push(Box::new(
                            Text::new(&Self::status_text(&selected.phase))
                                .with_id("connected.settings.phase")
                                .with_live(if state.status_is_error {
                                    Politeness::Assertive
                                } else {
                                    Politeness::Polite
                                }),
                        ))
                        .push(Box::new(Text::new(&strings::t_args(
                            "imager-connected-steps",
                            &[("steps", selected.verified_steps.into())],
                        ))));
                    if selected.pending_operation {
                        settings = settings.push(Box::new(Text::new(&strings::t(
                            "imager-connected-pending-operation",
                        ))));
                    }
                    if let Some(issue) = &selected.endpoint_issue {
                        settings = settings.push(Box::new(Text::new(&Self::endpoint_issue_text(
                            &issue.side,
                            &issue.error,
                        ))));
                    }
                    if let Some(entries) = selected.awaiting_entries {
                        settings = settings.push(Box::new(Text::new(&strings::t_args(
                            "imager-connected-awaiting-review",
                            &[("entries", entries.into())],
                        ))));
                    } else {
                        settings = settings.push(Box::new(Text::new(&strings::t(
                            "imager-connected-summary-unavailable",
                        ))));
                    }
                    let mut controls = Row::new();
                    for (action, id) in [
                        ("sync-now", "connected.sync"),
                        ("pause", "connected.pause"),
                        ("disconnect", "connected.disconnect"),
                        ("accept-initial", "connected.accept"),
                        ("restore", "connected.restore"),
                        ("replace", "connected.replace"),
                    ] {
                        controls = controls.push(Box::new(Self::button(row_resource, action, id)));
                    }
                    settings = settings
                        .push(Box::new(controls))
                        .push(Box::new(Text::new(&strings::t(
                            "imager-connected-native-closed",
                        ))))
                        .push(Box::new(Text::new(&strings::t(
                            "imager-connected-retention-policy",
                        ))));
                    body = body.push(Box::new(Card::titled(
                        strings::t("imager-connected-settings-title"),
                        Box::new(settings),
                    )));
                }
                body = body.push(Box::new(
                    stack()
                        .push(Box::new(
                            Text::new(&strings::t("imager-connected-catalog-title"))
                                .with_id("connected.catalog.title")
                                .with_role(TextRole::Heading)
                                .with_heading_level(2),
                        ))
                        .push(Box::new(rows)),
                ));
            }
        }

        let controls = Row::new()
            .push(Box::new(Self::button(
                &resource,
                "reload-connections",
                "connected.reload",
            )))
            .push(Box::new(Self::button(
                &resource,
                "first-connections",
                "connected.first",
            )))
            .push(Box::new(Self::button(
                &resource,
                "next-connections",
                "connected.next",
            )));
        body = body.push(Box::new(controls));
        let content = Scrollable::new_vertical()
            .fill_parent_height()
            .always_visible()
            .with_content_gutter(SCROLLBAR_GUTTER)
            .with_child(Box::new(body));
        let status = Card::new(Box::new(
            Text::new(&Self::status_text(&state.status_code))
                .with_id("connected.status")
                .with_live(if state.status_is_error {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                }),
        ));
        let mut page = Column::new()
            .with_no_padding()
            .with_horizontal_padding(PAGE_GUTTER, PAGE_GUTTER)
            .with_vertical_padding(PAGE_GUTTER)
            .with_gap(SIBLING)
            .push(Box::new(content));
        let selected_status_already_visible = state
            .selected
            .as_ref()
            .is_some_and(|selected| selected.phase == state.status_code);
        if state.phase != ManagementPhase::Settings || !selected_status_already_visible {
            page = page.push(Box::new(status));
        }
        Box::new(Container::new().filling().with_child(Box::new(page)))
    }
}

impl Widget for ConnectionView {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[connection_actions::CONNECTION_OPERATION_TYPE]
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![self.resource()]
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let Some(operation) = connection_actions::decode(type_id, payload)? else {
            return Ok(None);
        };
        self.invoke_operation(kind, operation)
            .map(|result| Some(result.output))
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        let _ = self.controller.poll();
        if let Some(composed) = &mut self.composed {
            composed.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut changed = self.controller.poll();
        if let Some(composed) = &mut self.composed {
            changed |= composed.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            composed.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|child| child.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "ConnectionView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_idle_sync(&self) -> bool {
        self.controller.needs_poll()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.controller.take_structure_dirty()
            || self
                .composed
                .as_mut()
                .is_some_and(|child| child.take_structure_dirty())
    }
}

#[cfg(test)]
mod tests {
    use automerge::AutoCommit;
    use tilo_actions::OperationKind;
    use ui::widget::{ControlBindingKind, Widget};

    use super::*;
    use crate::setup::connection_actions::{self, ConnectionOperation};

    fn control_disabled(root: &dyn Widget, id: &str) -> bool {
        fn find(widget: &dyn Widget, id: &str) -> Option<bool> {
            if widget.test_id() == Some(id) {
                return Some(
                    widget
                        .accessibility()
                        .expect("canonical control has accessibility state")
                        .state
                        .disabled,
                );
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| find(child, id))
        }
        find(root, id).unwrap_or_else(|| panic!("missing canonical control {id}"))
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_paths_preserve_unicode_and_escape_ambiguous_native_units() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        assert_eq!(
            format_endpoint_path(Path::new("/media/Café/東京")),
            "/media/Café/東京"
        );
        assert_eq!(
            format_endpoint_path(Path::new("/literal\\xFF")),
            "/literal\\\\xFF"
        );
        assert_eq!(
            format_endpoint_path(Path::new(&OsString::from_vec(b"/native\xff".to_vec()))),
            "/native\\xFF"
        );
        assert_eq!(
            format_endpoint_path(Path::new("/line\nnext")),
            "/line\\u{000A}next"
        );
        assert_eq!(
            format_endpoint_path(Path::new("/safe\u{202e}txt")),
            "/safe\\u{202E}txt"
        );
    }

    #[test]
    fn every_connected_control_uses_the_canonical_shared_action_binding() {
        let view = ConnectionView::for_shot("connected-status");
        let composition = view.build_composition();
        let bindings = composition.control_action_bindings();
        assert!(!bindings.is_empty());
        assert!(
            bindings
                .iter()
                .all(|binding| matches!(binding.kind, ControlBindingKind::Action(_)))
        );
        let addresses = bindings
            .iter()
            .filter_map(|binding| match &binding.kind {
                ControlBindingKind::Action(address) => {
                    Some((address.resource.as_str(), address.action.as_str()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(addresses.contains(&(connection_actions::CONNECTION_RESOURCE_ID, "back")));
        assert!(
            addresses
                .iter()
                .any(|(_, action)| *action == "open-settings")
        );
        assert!(addresses.iter().any(|(_, action)| *action == "sync-now"));
    }

    #[test]
    fn canonical_buttons_follow_pending_and_protected_admission() {
        let mut pending = ConnectionView::for_shot("connected-catalog");
        let resource = pending.resource();
        let reload = resource
            .actions
            .iter()
            .find(|action| action.id == "reload-connections")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        let document = AutoCommit::new();
        Widget::invoke_resource_operation_v2(
            &mut pending,
            OperationKind::View,
            &reload.type_id,
            &reload.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        pending.hold_pending_for_test();
        let composition = pending.build_composition();
        for id in ["connected.reload", "connected.first", "connected.next"] {
            assert!(control_disabled(composition.as_ref(), id), "{id}");
        }
        for row in pending.projection().rows {
            assert!(control_disabled(
                composition.as_ref(),
                &format!("connected.row.{}.open", row.alias)
            ));
        }

        let mut protected = ConnectionView::for_shot("connected-catalog");
        let resource = protected.resource();
        let selected_alias = protected.projection().rows[1].alias;
        let row = resource
            .children
            .iter()
            .find(|row| row.id.0.ends_with(&format!(".{selected_alias}")))
            .unwrap();
        let open = row
            .actions
            .iter()
            .find(|action| action.id == "open-settings")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        let mut document = AutoCommit::new();
        Widget::invoke_resource_operation_v2(
            &mut protected,
            OperationKind::View,
            &open.type_id,
            &open.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        assert!(Widget::update(
            &mut protected,
            &mut document,
            &automerge::ROOT
        ));
        let composition = protected.build_composition();
        for id in ["connected.reload", "connected.first", "connected.next"] {
            assert!(control_disabled(composition.as_ref(), id), "{id}");
        }
        assert!(!control_disabled(
            composition.as_ref(),
            &format!("connected.row.{selected_alias}.open")
        ));
        for row in protected
            .projection()
            .rows
            .into_iter()
            .filter(|row| row.alias != selected_alias)
        {
            assert!(control_disabled(
                composition.as_ref(),
                &format!("connected.row.{}.open", row.alias)
            ));
        }
    }

    #[test]
    fn direct_widget_admission_validates_kind_and_refuses_native_effects() {
        let mut view = ConnectionView::for_shot("connected-status");
        let state = view.projection();
        let alias = state.selected.as_ref().unwrap().alias;
        let operation = ConnectionOperation::SyncNow {
            generation: state.generation,
            alias,
        };
        let encoded = connection_actions::encode(&operation);
        let mut document = AutoCommit::new();
        assert!(
            Widget::invoke_resource_operation_v2(
                &mut view,
                OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &document,
                &automerge::ROOT,
            )
            .is_err()
        );
        let error = Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::Effect,
            &encoded.type_id,
            &encoded.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap_err();
        assert!(error.contains("unavailable"));
        assert!(document.get_heads().is_empty());
    }

    #[test]
    fn all_native_actions_from_real_resources_fail_closed_through_the_owner_without_document_changes()
     {
        let mut view = ConnectionView::for_shot("connected-status");
        let mut document = AutoCommit::new();
        let before_bytes = document.save();
        let before_heads = document.get_heads();
        let resource = view.resource();
        let selected = resource
            .children
            .iter()
            .find(|row| row.actions.iter().any(|action| action.id == "sync-now"))
            .unwrap();
        let reopen = selected
            .actions
            .iter()
            .find(|action| action.id == "open-settings")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &reopen.type_id,
            &reopen.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        assert!(Widget::update(&mut view, &mut document, &automerge::ROOT));
        let baseline_submissions = view.service_submission_count();
        assert_eq!(baseline_submissions, 1, "fixture adapter counter is live");

        let resource = view.resource();
        let selected = resource
            .children
            .iter()
            .find(|row| row.actions.iter().any(|action| action.id == "sync-now"))
            .unwrap();
        let observe = resource
            .actions
            .iter()
            .find(|action| action.id == "observe")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &observe.type_id,
            &observe.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        let mut retained = Vec::new();
        for action_id in [
            "sync-now",
            "pause",
            "disconnect",
            "accept-initial",
            "restore",
            "replace",
        ] {
            let encoded = selected
                .actions
                .iter()
                .find(|action| action.id == action_id)
                .unwrap()
                .build_json(&serde_json::Map::new())
                .unwrap();
            retained.push((action_id, encoded.clone()));
            let error = Widget::invoke_resource_operation_v2(
                &mut view,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &document,
                &automerge::ROOT,
            )
            .unwrap_err();
            assert!(error.contains("unavailable"), "{action_id}: {error}");
            assert_eq!(view.service_submission_count(), baseline_submissions);

            let wrong_kind = Widget::invoke_resource_operation_v2(
                &mut view,
                OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &document,
                &automerge::ROOT,
            )
            .unwrap_err();
            assert!(wrong_kind.contains("wrong operation kind"), "{action_id}");
            assert_eq!(view.service_submission_count(), baseline_submissions);

            let mut malformed: serde_json::Value =
                serde_json::from_slice(&encoded.payload).unwrap();
            malformed
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), serde_json::Value::Bool(true));
            let malformed = serde_json::to_vec(&malformed).unwrap();
            assert!(
                Widget::invoke_resource_operation_v2(
                    &mut view,
                    OperationKind::Effect,
                    &encoded.type_id,
                    &malformed,
                    &document,
                    &automerge::ROOT,
                )
                .is_err()
            );
            assert_eq!(view.service_submission_count(), baseline_submissions);
        }
        let back = resource
            .actions
            .iter()
            .find(|action| action.id == "back")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &back.type_id,
            &back.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        for (action_id, encoded) in retained {
            let stale = Widget::invoke_resource_operation_v2(
                &mut view,
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
                &document,
                &automerge::ROOT,
            )
            .unwrap_err();
            assert!(stale.contains("no longer current"), "{action_id}: {stale}");
            assert_eq!(view.service_submission_count(), baseline_submissions);
        }
        assert_eq!(document.get_heads(), before_heads);
        assert_eq!(document.save(), before_bytes);
    }

    #[test]
    fn owning_widget_keeps_settings_and_announces_terminal_request_errors() {
        let mut view = ConnectionView::for_shot("connected-catalog");
        let resource = view.resource();
        let row = &resource.children[0];
        let open = row
            .actions
            .iter()
            .find(|action| action.id == "open-settings")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        let mut document = AutoCommit::new();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &open.type_id,
            &open.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        assert!(Widget::update(&mut view, &mut document, &automerge::ROOT));
        let selected_alias = view.projection().selected.as_ref().unwrap().alias;

        let resource = view.resource();
        let row = resource
            .children
            .iter()
            .find(|row| row.id.0.ends_with(&format!(".{selected_alias}")))
            .unwrap();
        let reopen = row
            .actions
            .iter()
            .find(|action| action.id == "open-settings")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &reopen.type_id,
            &reopen.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        view.hold_pending_for_test();
        assert_eq!(view.projection().status_code, "opening_settings");
        view.deliver_pending_error_for_test(ui::folders::connected::ConnectionError::Busy);
        assert!(Widget::update(&mut view, &mut document, &automerge::ROOT));
        let failed = view.projection();
        assert_eq!(failed.selected.as_ref().unwrap().alias, selected_alias);
        assert_eq!(failed.status_code, "busy");
        assert!(ConnectionView::status_text(&failed.status_code).contains("busy"));
        let composition = view.build_composition();
        fn contains_status(widget: &dyn Widget) -> bool {
            widget.test_id() == Some("connected.status")
                || widget
                    .children()
                    .iter()
                    .any(|child| contains_status(*child))
        }
        assert!(contains_status(composition.as_ref()));
        assert_eq!(view.service_submission_count(), 2);
    }

    #[test]
    fn retained_published_next_refuses_after_closed_without_reopening_or_submitting() {
        let mut view = ConnectionView::for_controlled_many_catalog();
        let catalog = view.resource();
        let retained_next = catalog
            .actions
            .iter()
            .find(|action| action.id == "next-connections")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        let first = catalog
            .actions
            .iter()
            .find(|action| action.id == "first-connections")
            .unwrap()
            .build_json(&serde_json::Map::new())
            .unwrap();
        let mut document = AutoCommit::new();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &first.type_id,
            &first.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        view.hold_pending_for_test();
        view.deliver_pending_error_for_test(ui::folders::connected::ConnectionError::Closed);
        assert!(Widget::update(&mut view, &mut document, &automerge::ROOT));
        assert!(!view.projection().next_available);
        let before = view.service_submission_count();

        let error = Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &retained_next.type_id,
            &retained_next.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap_err();
        assert!(error.contains("not available"), "{error}");
        assert_eq!(view.service_submission_count(), before);
        assert!(!view.opener_pending_for_test());

        let current = view.resource();
        let first = current
            .actions
            .iter()
            .find(|action| action.id == "first-connections")
            .unwrap();
        assert!(first.contract.availability.is_available());
        let first = first.build_json(&serde_json::Map::new()).unwrap();
        Widget::invoke_resource_operation_v2(
            &mut view,
            OperationKind::View,
            &first.type_id,
            &first.payload,
            &document,
            &automerge::ROOT,
        )
        .unwrap()
        .unwrap();
        assert!(view.opener_pending_for_test());
        assert_eq!(view.service_submission_count(), before);
    }

    #[test]
    fn published_next_preserves_the_page_for_submission_and_terminal_failures() {
        let invoke_next = |view: &mut ConnectionView, document: &AutoCommit| {
            let resource = view.resource();
            let next = resource
                .actions
                .iter()
                .find(|action| action.id == "next-connections")
                .unwrap()
                .build_json(&serde_json::Map::new())
                .unwrap();
            Widget::invoke_resource_operation_v2(
                view,
                OperationKind::View,
                &next.type_id,
                &next.payload,
                document,
                &automerge::ROOT,
            )
        };

        let mut submission = ConnectionView::for_controlled_many_catalog();
        let submission_rows = submission.projection().rows.len();
        let document = AutoCommit::new();
        submission.inject_submission_error_for_test(
            ui::folders::connected::ConnectionError::PermissionDenied,
        );
        let error = invoke_next(&mut submission, &document).unwrap_err();
        assert!(error.contains("permissions"), "{error}");
        assert_eq!(submission.projection().rows.len(), submission_rows);
        assert!(!submission.projection().next_available);
        assert_eq!(submission.service_submission_count(), 2);

        let mut terminal = ConnectionView::for_controlled_many_catalog();
        let terminal_rows = terminal.projection().rows.len();
        let mut document = AutoCommit::new();
        invoke_next(&mut terminal, &document).unwrap().unwrap();
        terminal.hold_pending_for_test();
        terminal.deliver_pending_error_for_test(ui::folders::connected::ConnectionError::Busy);
        assert!(Widget::update(
            &mut terminal,
            &mut document,
            &automerge::ROOT
        ));
        let failed = terminal.projection();
        assert_eq!(failed.rows.len(), terminal_rows);
        assert_eq!(failed.status_code, "busy");
        assert!(!failed.next_available);
        assert_eq!(terminal.service_submission_count(), 2);
    }

    #[test]
    fn every_facade_reason_has_human_copy_without_raw_debug_codes() {
        for code in [
            "unsupported_host",
            "busy",
            "closed",
            "limit_exceeded",
            "stale_review",
            "disconnected",
            "identity_changed",
            "unavailable",
            "permission_denied",
            "unsafe_volume",
            "read_only",
            "unverified",
            "unstable",
            "recovery_required",
            "recovery_space",
            "corrupt_store",
            "persistence",
        ] {
            let text = ConnectionView::error_text(code);
            assert_ne!(text, code);
            assert!(!text.contains('_'), "raw code leaked through: {text}");
        }
        let endpoint = ConnectionView::endpoint_issue_text("destination", "permission_denied");
        assert!(endpoint.to_lowercase().contains("destination folder"));
        assert!(!endpoint.contains("permission_denied"));
        let recovery = ConnectionView::status_text("recovery_required_recovery_space");
        assert!(recovery.contains("storage"));
        assert!(!recovery.contains("recovery_space"));
    }

    #[test]
    fn opening_settings_on_a_multirow_catalog_presents_settings_before_rows() {
        let mut view = ConnectionView::for_shot("connected-catalog");
        let catalog = view.projection();
        assert!(catalog.rows.len() > 1);
        let alias = catalog.rows[1].alias;
        view.invoke_operation(
            OperationKind::View,
            ConnectionOperation::OpenSettings {
                generation: catalog.generation,
                alias,
            },
        )
        .unwrap();
        let mut document = AutoCommit::new();
        assert!(view.update(&mut document, &automerge::ROOT));
        assert_eq!(view.projection().selected.as_ref().unwrap().alias, alias);

        let composition = view.build_composition();
        fn ids(widget: &dyn Widget, found: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                found.push(id.to_string());
            }
            for child in widget.children() {
                ids(child, found);
            }
        }
        let mut order = Vec::new();
        ids(composition.as_ref(), &mut order);
        let settings = order
            .iter()
            .position(|id| id == "connected.settings.phase")
            .unwrap();
        let catalog = order
            .iter()
            .position(|id| id == "connected.catalog.title")
            .unwrap();
        assert!(
            settings < catalog,
            "selected settings must precede a long catalog"
        );
    }
}
