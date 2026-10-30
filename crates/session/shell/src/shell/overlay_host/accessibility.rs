//! The host's accessibility tree: the open overlays as assistive technology
//! sees them, their hit rects and focus.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    #[allow(
        clippy::too_many_lines,
        reason = "The accessibility tree is kept as one ordered overlay projection so assistive focus order matches retained visual overlay order."
    )]
    pub(super) fn host_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut root = AccessNode::new(Role::Group).with_name(t("a11y-workspace"));
        let outputs = self.layer_outputs();
        let geometry = self.layer_geometry(&outputs);
        for overlay in self.accessible_overlay_order() {
            if let Some(layer) = self.layers.layer(overlay) {
                root.children.extend(layer.accessibility(geometry));
                continue;
            }
            match overlay {
                // Stacked and published above, or an app that publishes its
                // own tree as a child of the host; the launcher publishes its
                // own card, as its own child.
                LayerId::ShortcutHelp
                | LayerId::Directory
                | LayerId::DisplayLayouts
                | LayerId::DisplayConfirmation
                | LayerId::WindowSwitcher
                | LayerId::EmojiPicker
                | LayerId::TabContextMenu
                | LayerId::PowerMenu
                | LayerId::Overview
                | LayerId::Dictation
                | LayerId::CommandPalette => {}
                LayerId::MemberSheet => root.children.push(
                    AccessNode::new(Role::Dialog)
                        .with_name(t("a11y-member-sheet"))
                        .with_description(t("a11y-member-sheet-description"))
                        .with_state(AccessState::new().modal(true)),
                ),
                LayerId::WorkspaceSettings => {
                    let mut settings = AccessNode::new(Role::Dialog)
                        .with_name(t("a11y-workspace-settings"))
                        .with_description(t("a11y-workspace-settings-description"))
                        .with_state(AccessState::new().modal(true));
                    if self
                        .workspace_settings_overlay
                        .ownership_action_rect()
                        .area()
                        > 0.0
                    {
                        settings.children.push(
                            AccessNode::new(Role::Button)
                                .with_name(t("a11y-manage-ownership"))
                                .with_description(t("a11y-manage-ownership-description")),
                        );
                    }
                    if let Some(chrome) = &self.chrome {
                        let state = chrome.borrow();
                        if matches!(state.ws_settings_tab, WsTab::General) {
                            let body = self.workspace_settings_overlay.general_body_rect();
                            let summary = self.workspace_settings_overlay.general_summary_rect();
                            let value = state.editing_workspace_name.clone().or_else(|| {
                                state
                                    .tree()
                                    .desktop
                                    .active_workspace()
                                    .map(|workspace| workspace.name.clone())
                            });
                            settings.children.push(
                                AccessNode::new(Role::TextInput)
                                    .with_name(t("a11y-workspace-name"))
                                    .with_optional_value(value)
                                    .with_text_selection(
                                        state.editing_workspace_name_selection.anchor,
                                        state.editing_workspace_name_selection.cursor,
                                    )
                                    .with_bounds(self.workspace_settings_overlay.name_rect())
                                    .with_state(AccessState {
                                        focusable: true,
                                        hidden: self.workspace_settings_overlay.name_rect().y0
                                            < body.y0
                                            || self.workspace_settings_overlay.name_rect().y1
                                                > body.y1,
                                        focused: !self.palette_visible
                                            && state.editing_workspace_name.is_some(),
                                        ..Default::default()
                                    }),
                            );
                            settings.children.push(
                                AccessNode::new(Role::Group)
                                    .with_name(t("a11y-workspace-settings-content"))
                                    .with_bounds(body),
                            );
                            settings.children.push(
                                AccessNode::new(Role::Label)
                                    .with_name(t("a11y-at-a-glance"))
                                    .with_bounds(summary)
                                    .with_state(AccessState {
                                        hidden: summary.y0 < body.y0 || summary.y1 > body.y1,
                                        ..Default::default()
                                    }),
                            );
                        }
                    }
                    root.children.push(settings);
                }
                LayerId::Collaborators => root
                    .children
                    .push(self.collaborators_overlay.accessibility_snapshot()),
                LayerId::NowPlaying => {
                    if let Some(card) = self.now_playing_accessibility() {
                        root.children.push(card);
                    }
                }
                LayerId::QuickSettings => {
                    if let Some(mut panel) = self.quick_settings_accessibility() {
                        super::sheet_accessibility::route_quick_settings(&mut panel);
                        root.children.push(panel);
                    }
                }
            }
        }
        self.append_modal_sheet_accessibility(&mut root);
        root.children.extend(self.live_status_accessibility());
        // Last, so the overlay indices above keep their paths.
        root.children.extend(self.strip_switcher_accessibility());
        // The control hint on screen, said politely.
        root.children.extend(self.control_hint_accessibility());
        // Last, so the paths above keep their indices: the desk widgets a
        // reader can open, fold and pin exactly as the pointer and Tab do.
        let desk = self.desk_accessibility_children();
        if !desk.is_empty() {
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name(t("desk-widgets-a11y"))
                    .with_children(desk),
            );
        }
        root
    }

    pub(super) fn host_synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let (&overlay_index, child_path) = path.split_first()?;
        let overlay = *self.accessible_overlay_order().get(overlay_index)?;
        if let Some(layer) = self.layers.layer(overlay) {
            let outputs = self.layer_outputs();
            return layer.accessibility_rect(self.layer_geometry(&outputs), child_path);
        }
        match overlay {
            LayerId::Collaborators => self
                .collaborators_overlay
                .accessibility_rect_for_path(child_path),
            LayerId::QuickSettings => self.quick_settings_accessibility_rect(child_path),
            LayerId::WorkspaceSettings => {
                let ownership = self.workspace_settings_overlay.ownership_action_rect();
                let ownership_present = ownership.area() > 0.0;
                match child_path {
                    [] => Some(self.workspace_settings_overlay.rect()),
                    [0] if ownership_present => Some(ownership),
                    [index] => self.chrome.as_ref().and_then(|chrome| {
                        let state = chrome.borrow();
                        if !matches!(state.ws_settings_tab, WsTab::General) {
                            return None;
                        }
                        let index = index.checked_sub(usize::from(ownership_present))?;
                        let body = self.workspace_settings_overlay.general_body_rect();
                        let visible =
                            |rect: Rect| (rect.y0 >= body.y0 && rect.y1 <= body.y1).then_some(rect);
                        match index {
                            0 => visible(self.workspace_settings_overlay.name_rect()),
                            1 => Some(body),
                            2 => visible(self.workspace_settings_overlay.general_summary_rect()),
                            _ => None,
                        }
                    }),
                    _ => None,
                }
            }
            LayerId::NowPlaying => self.now_playing_accessibility_rect(child_path),
            LayerId::MemberSheet
            | LayerId::ShortcutHelp
            | LayerId::Directory
            | LayerId::DisplayLayouts
            // Stacked; answered above.
            | LayerId::DisplayConfirmation
            | LayerId::WindowSwitcher
            | LayerId::EmojiPicker
            | LayerId::TabContextMenu
            | LayerId::Overview
            | LayerId::PowerMenu | LayerId::Dictation
            // The launcher answers for its own card.
            | LayerId::CommandPalette => None,
        }
    }

    pub(super) fn host_focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let Some((&overlay_index, child_path)) = path.split_first() else {
            return false;
        };
        let Some(overlay) = self.accessible_overlay_order().get(overlay_index).copied() else {
            return false;
        };
        if let Some(layer) = self.layers.layer_mut(overlay) {
            let focused = layer.focus_accessibility(child_path);
            if focused && let Some(chrome) = self.chrome.as_ref() {
                chrome.borrow_mut().bump();
            }
            return focused;
        }
        match overlay {
            LayerId::Collaborators => self
                .collaborators_overlay
                .focus_accessibility_path(child_path),
            LayerId::QuickSettings => self.quick_settings_accessibility_rect(child_path).is_some(),
            LayerId::NowPlaying => self.focus_now_playing_accessibility(child_path),
            LayerId::WorkspaceSettings => {
                !child_path.is_empty() && self.synthetic_accessibility_rect(path).is_some()
            }
            LayerId::MemberSheet
            | LayerId::ShortcutHelp
            | LayerId::Directory
            | LayerId::DisplayLayouts
            // Stacked; answered above.
            | LayerId::DisplayConfirmation
            | LayerId::WindowSwitcher
            | LayerId::EmojiPicker
            | LayerId::TabContextMenu
            | LayerId::Overview
            | LayerId::PowerMenu | LayerId::Dictation
            // The launcher answers for its own card.
            | LayerId::CommandPalette => false,
        }
    }
}
