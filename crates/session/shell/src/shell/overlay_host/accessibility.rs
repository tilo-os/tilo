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
                // Stacked; published above.
                LayerId::DisplayConfirmation
                | LayerId::WindowSwitcher
                | LayerId::EmojiPicker
                | LayerId::TabContextMenu
                | LayerId::PowerMenu
                | LayerId::Dictation => {}
                LayerId::CommandPalette => {
                    if let Some(mut palette) = self.palette.accessibility() {
                        // The query owns focus, not its synthetic parent dialog.
                        palette.state.focused = false;
                        // The filter band is the host's, painted over the
                        // palette's reserved strip; it joins the palette's
                        // tree after the palette's own children (query,
                        // result list) so the dialog reads as one surface.
                        // A palette that is visible but not open publishes
                        // no children, and then neither does the band —
                        // `search_filters_child_index` is the one answer
                        // both this and `synthetic_accessibility_rect` use.
                        if self.search_filters_child_index() == Some(palette.children.len()) {
                            palette.children.push(self.search_filters_accessibility());
                        }
                        root.children.push(palette);
                    }
                }
                LayerId::MemberSheet => root.children.push(
                    AccessNode::new(Role::Dialog)
                        .with_name(t("a11y-member-sheet"))
                        .with_description(t("a11y-member-sheet-description"))
                        .with_state(AccessState::new().modal(true)),
                ),
                LayerId::Overview => {
                    let cards = self.overview_cards();
                    let mut overview = AccessNode::new(Role::Dialog)
                        .with_name(t("a11y-overview"))
                        .with_bounds(self.overview_bounds())
                        .with_state(AccessState::new().modal(true));
                    overview
                        .children
                        .extend(cards.iter().enumerate().map(|(index, card)| {
                            AccessNode::new(Role::Button)
                                .with_name(card.name.clone())
                                .with_bounds(self.overview_row_rect(index))
                                .with_state(AccessState::new().focusable(true).focused(
                                    self.overview_selection == Some(index)
                                        && self.overview_window_selection.is_none(),
                                ))
                        }));
                    let windows = self.overview_row_windows();
                    if !windows.is_empty() {
                        let total = windows.len();
                        let items = windows
                            .iter()
                            .enumerate()
                            .map(|(index, window)| {
                                let selected =
                                    self.overview_window_selection == Some(window.address.tab);
                                AccessNode::new(Role::ListItem)
                                    .with_semantic_key(format!(
                                        "overview-window:{}",
                                        window.address.tab
                                    ))
                                    .with_name(window.title.clone())
                                    .with_position(index.saturating_add(1), total)
                                    .with_bounds(self.overview_window_tile_rect(index))
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(selected)
                                            .selected(selected),
                                    )
                            })
                            .collect();
                        overview.children.push(
                            AccessNode::new(Role::List)
                                .with_bounds(self.overview_window_list_rect().unwrap_or(Rect::ZERO))
                                .with_name(crate::shell::i18n::t("overview-windows-title"))
                                .with_description(crate::shell::i18n::t(
                                    "overview-windows-description",
                                ))
                                .with_children(items),
                        );
                    }
                    overview.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(t("a11y-new-workspace"))
                            .with_bounds(self.overview_add_workspace_rect_for(cards.len()))
                            .with_state(AccessState::new().focusable(true).focused(
                                self.overview_selection == Some(cards.len())
                                    && self.overview_window_selection.is_none(),
                            )),
                    );
                    overview.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(t("overview-join-workspace"))
                            .with_bounds(self.overview_join_workspace_rect_for(cards.len()))
                            .with_state(AccessState::new().focusable(true).focused(
                                self.overview_selection == Some(cards.len().saturating_add(1))
                                    && self.overview_window_selection.is_none(),
                            )),
                    );
                    let site_rooms = self.site_rooms();
                    for (idx, card) in cards.iter().enumerate() {
                        let leave = self.overview_leave_rect(idx, &site_rooms);
                        if leave.area() > 0.0 {
                            overview.children.push(
                                AccessNode::new(Role::Button)
                                    .with_name(t_args(
                                        "a11y-leave-workspace",
                                        &[("name", card.name.clone().into())],
                                    ))
                                    .with_bounds(leave),
                            );
                        }
                    }
                    if let Some(strips) = self.overview_strips_accessibility() {
                        overview.children.push(strips);
                    }
                    root.children.push(overview);
                }
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
                LayerId::RoomActivity => root
                    .children
                    .push(self.room_activity.accessibility_snapshot()),
                LayerId::NowPlaying => {
                    if let Some(card) = self.now_playing_accessibility() {
                        root.children.push(card);
                    }
                }
                LayerId::QuickSettings => {
                    if let Some(chrome) = self.chrome.as_ref() {
                        let state = chrome.borrow();
                        if matches!(
                            state.quick_settings.current_level(),
                            crate::shell::quick_settings::QsLevel::Notifications
                        ) {
                            // One semantic snapshot has one clock edge, so
                            // adjacent rows cannot label their ages across a
                            // relative-time boundary on a loaded compositor.
                            root.children.push(self.notification_center.accessibility(
                                &ChromeNotifications::new(chrome).with_agents(&self.room_activity),
                                &self.qs_notification_layout(),
                                Instant::now(),
                            ));
                        } else if let crate::shell::quick_settings::QsLevel::BluetoothDetails(
                            address,
                        ) = state.quick_settings.current_level()
                        {
                            let accessory = state
                                .quick_settings
                                .bt_devices
                                .iter()
                                .find(|device| device.address.eq_ignore_ascii_case(address));
                            let accessory_fallback = t("a11y-bluetooth-accessory");
                            let accessory_name = accessory
                                .map_or(accessory_fallback.as_str(), |device| device.name.as_str());
                            let view = self
                                .device_fabric_call(DeviceFabricUiAction::Snapshot)
                                .unwrap_or_default();
                            let rows = std::iter::once(None)
                                .chain(view.devices.into_iter().map(Some))
                                .collect::<Vec<_>>();
                            let visible = self.qs_device_fabric_visible_rows();
                            let start = self
                                .qs_device_fabric_start
                                .min(rows.len().saturating_sub(visible));
                            let shown = visible.min(rows.len().saturating_sub(start));
                            let items = rows
                                .into_iter()
                                .skip(start)
                                .take(shown)
                                .enumerate()
                                .map(|(index, device)| match device {
                                    None => AccessNode::new(Role::Button)
                                        .with_name(t_args(
                                            "a11y-bluetooth-disconnect",
                                            &[("accessory", accessory_name.into())],
                                        ))
                                        .with_position(index.saturating_add(1), shown),
                                    Some(device) => AccessNode::new(Role::Button)
                                        .with_name(t_args(
                                            "a11y-bluetooth-move-to",
                                            &[("device", device.label.as_str().into())],
                                        ))
                                        .with_description(if device.is_local {
                                            t("a11y-bluetooth-destination-local")
                                        } else if device.online {
                                            t("a11y-bluetooth-destination-online")
                                        } else {
                                            t("a11y-bluetooth-destination-offline")
                                        })
                                        .with_state(
                                            AccessState::new()
                                                .disabled(device.is_local || !device.online),
                                        )
                                        .with_position(index.saturating_add(1), shown),
                                })
                                .collect();
                            root.children.push(
                                AccessNode::new(Role::Dialog)
                                    .with_name(t_args(
                                        "a11y-bluetooth-move",
                                        &[("accessory", accessory_name.into())],
                                    ))
                                    .with_description(t("a11y-bluetooth-move-description"))
                                    .with_state(AccessState::new().modal(true))
                                    .with_children(vec![
                                        AccessNode::new(Role::List)
                                            .with_name(t("a11y-bluetooth-destinations"))
                                            .with_children(items),
                                    ]),
                            );
                        } else {
                            root.children
                                .push(self.qs_root.accessibility_snapshot(&state.quick_settings));
                            // The inline password, while it is asked for,
                            // holds the keyboard like a sheet.
                            if self.wifi_password_prompt_up()
                                && let Some(prompt) = state.quick_settings.wifi_password.as_ref()
                            {
                                root.children.push(self.wifi_password_accessibility(prompt));
                            }
                        }
                    }
                }
                LayerId::DisplayLayouts => {
                    root.children.push(self.projection_overlay.accessibility());
                }
                LayerId::ShortcutHelp => {
                    use settings_store::hardware::ShortcutAction;
                    let shortcuts = &self.shortcut_config;
                    let chord = |action| {
                        shortcuts
                            .bindings
                            .get(&action)
                            .map_or_else(|| t("guide-unassigned-inline"), shortcut_guide_chord)
                    };
                    let essential_summary = shortcut_reference_rows(shortcuts)
                        .into_iter()
                        .map(|(label, chord)| format!("{label}: {chord}"))
                        .chain(
                            shortcut_action_rows(shortcuts)
                                .into_iter()
                                .map(|(label, chord)| format!("{label}: {chord}")),
                        )
                        .collect::<Vec<_>>()
                        .join("; ");
                    root.children.push(
                        AccessNode::new(Role::Dialog)
                            .with_name(t("guide-a11y-name"))
                            .with_description(t_args(
                                "guide-a11y-description",
                                &[
                                    (
                                        "window-back",
                                        chord(ShortcutAction::FocusPreviousWindow).into(),
                                    ),
                                    ("window-next", chord(ShortcutAction::FocusNextWindow).into()),
                                    ("tab-back", chord(ShortcutAction::PreviousTab).into()),
                                    ("tab-next", chord(ShortcutAction::NextTab).into()),
                                    ("maximize", chord(ShortcutAction::MaximizeWindow).into()),
                                    ("restore", chord(ShortcutAction::RestoreWindow).into()),
                                    ("strip-back", chord(ShortcutAction::PreviousStrip).into()),
                                    ("strip-next", chord(ShortcutAction::NextStrip).into()),
                                    ("strip-first", chord(ShortcutAction::FocusStrip1).into()),
                                    ("strip-last", chord(ShortcutAction::FocusStrip9).into()),
                                    (
                                        "workspace-back",
                                        chord(ShortcutAction::PreviousWorkspace).into(),
                                    ),
                                    (
                                        "workspace-next",
                                        chord(ShortcutAction::NextWorkspace).into(),
                                    ),
                                    ("group-left", chord(ShortcutAction::MoveGroupLeft).into()),
                                    ("group-right", chord(ShortcutAction::MoveGroupRight).into()),
                                    ("narrower", chord(ShortcutAction::ResizeNarrower).into()),
                                    ("wider", chord(ShortcutAction::ResizeWider).into()),
                                    ("essentials", essential_summary.into()),
                                ],
                            ))
                            .with_state(AccessState::new().modal(true)),
                    );
                }
            }
        }
        self.append_modal_sheet_accessibility(&mut root);
        root.children.extend(self.live_status_accessibility());
        // Last, so the overlay indices above keep their paths.
        root.children.extend(self.strip_switcher_accessibility());
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
            LayerId::CommandPalette => match child_path {
                // The trailing child is the host's filter band (see
                // `accessibility`); everything before it is the palette's.
                [first, rest @ ..] if Some(*first) == self.search_filters_child_index() => {
                    self.search_filters.rect_for_path(rest)
                }
                _ => self.palette.synthetic_accessibility_rect(child_path),
            },
            LayerId::RoomActivity => {
                let mut local_path = Vec::with_capacity(path.len());
                local_path.push(0);
                local_path.extend_from_slice(child_path);
                self.room_activity.accessibility_rect_for_path(&local_path)
            }
            LayerId::Collaborators => self
                .collaborators_overlay
                .accessibility_rect_for_path(child_path),
            LayerId::QuickSettings => {
                let drill_in_action = (self.chrome.is_some() && matches!(child_path, [0]))
                    .then(|| self.quick_settings_rect());
                let notification_action = self.chrome.as_ref().and_then(|chrome| {
                    if !matches!(
                        chrome.borrow().quick_settings.current_level(),
                        crate::shell::quick_settings::QsLevel::Notifications
                    ) {
                        return None;
                    }
                    let [0, dialog_path @ ..] = child_path else {
                        return None;
                    };
                    self.notification_center.accessibility_rect_for_path(
                        &ChromeNotifications::new(chrome).with_agents(&self.room_activity),
                        &self.qs_notification_layout(),
                        dialog_path,
                    )
                });
                drill_in_action
                    .or(notification_action)
                    .or_else(|| self.qs_root.accessibility_rect_for_path(child_path))
            }
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
            LayerId::Overview => self.overview_accessibility_rect(child_path),
            LayerId::NowPlaying => self.now_playing_accessibility_rect(child_path),
            LayerId::MemberSheet
            | LayerId::ShortcutHelp
            | LayerId::DisplayLayouts
            // Stacked; answered above.
            | LayerId::DisplayConfirmation
            | LayerId::WindowSwitcher
            | LayerId::EmojiPicker
            | LayerId::TabContextMenu
            | LayerId::PowerMenu | LayerId::Dictation => None,
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
            LayerId::CommandPalette => {
                self.palette.focus_synthetic_accessibility(child_path)
            }
            LayerId::RoomActivity => {
                let mut local_path = Vec::with_capacity(path.len());
                local_path.push(0);
                local_path.extend_from_slice(child_path);
                self.room_activity.focus_accessibility_path(&local_path)
            }
            LayerId::Collaborators => self
                .collaborators_overlay
                .focus_accessibility_path(child_path),
            LayerId::QuickSettings => {
                focus_quick_settings_accessibility(self, child_path)
            }
            LayerId::NowPlaying => self.focus_now_playing_accessibility(child_path),
            LayerId::WorkspaceSettings => {
                !child_path.is_empty() && self.synthetic_accessibility_rect(path).is_some()
            }
            LayerId::Overview => self.focus_overview_accessibility(child_path),
            LayerId::MemberSheet
            | LayerId::ShortcutHelp
            | LayerId::DisplayLayouts
            // Stacked; answered above.
            | LayerId::DisplayConfirmation
            | LayerId::WindowSwitcher
            | LayerId::EmojiPicker
            | LayerId::TabContextMenu
            | LayerId::PowerMenu | LayerId::Dictation => false,
        }
    }
}
