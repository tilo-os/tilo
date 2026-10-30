//! Quick settings device and clipboard rows: what a press on one does, and
//! the rows the panel lists.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    #[expect(
        clippy::too_many_lines,
        reason = "one click dispatcher maps every fabric pane row to its action"
    )]
    pub(super) fn handle_qs_device_fabric_press(
        &mut self,
        row_index: usize,
        part: tilo_quick_settings::RowPart,
    ) {
        let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) else {
            return;
        };
        self.normalise_qs_selected_media(&view);
        let realtime = self
            .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
            .unwrap_or_default();
        let rows = device_fabric_pane_rows(&view, &realtime);
        let Some(item) = rows.get(row_index).cloned() else {
            return;
        };
        let secondary = Self::qs_device_secondary_action(part, &item);
        if let Some((action, success)) = secondary {
            self.finish_qs_device_action(action, success);
            return;
        }
        let action = match item {
            DeviceFabricPaneRow::CalibrationProposal(proposal) => {
                if part == tilo_quick_settings::RowPart::Allow {
                    Some((
                        DeviceFabricUiAction::ApplyCalibration {
                            device: proposal.peer,
                        },
                        String::new(),
                    ))
                } else if part == tilo_quick_settings::RowPart::Decline {
                    Some((
                        DeviceFabricUiAction::DismissCalibration {
                            device: proposal.peer,
                        },
                        String::new(),
                    ))
                } else {
                    None
                }
            }
            DeviceFabricPaneRow::CalibrationRevert(device) => Some((
                DeviceFabricUiAction::RevertCalibration { device },
                String::new(),
            )),
            DeviceFabricPaneRow::RemoteAudioConsent(consent) => {
                let Some(approved) = Self::qs_device_row_choice(part) else {
                    return;
                };
                Some((
                    DeviceFabricUiAction::AuthorizeRemoteAudio {
                        session_id: consent.session_id,
                        approved,
                    },
                    String::new(),
                ))
            }
            DeviceFabricPaneRow::RemoteAudioSource(source) => {
                self.qs_selected_source = Some(QsDeviceSource::RemoteAudio(source.token));
                self.bump_chrome();
                return;
            }
            DeviceFabricPaneRow::SpeakerConsent(consent) => {
                let Some(approved) = Self::qs_device_row_choice(part) else {
                    return;
                };
                Some((
                    DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                        group: consent.group,
                        kind: consent.scope.kind(),
                        approved,
                    },
                    String::new(),
                ))
            }
            DeviceFabricPaneRow::SpeakerSession(session) => Some((
                DeviceFabricUiAction::StopSpeakerGroupSession {
                    session_id: session.session_id,
                },
                String::new(),
            )),
            DeviceFabricPaneRow::SpeakerGroup(group)
                if let Some(QsDeviceSource::RemoteAudio(source_token)) =
                    self.qs_selected_source =>
            {
                Some((
                    DeviceFabricUiAction::StartSpeakerGroupAudio {
                        source_token,
                        group: group.id,
                    },
                    String::new(),
                ))
            }
            DeviceFabricPaneRow::SpeakerGroup(group)
                if let Some(QsDeviceSource::Media(session_id)) = &self.qs_selected_source =>
            {
                Some((
                    DeviceFabricUiAction::PlayMediaOnSpeakerGroup {
                        session_id: session_id.clone(),
                        group: group.id,
                    },
                    t_args(
                        "host-fabric-speaker-playing-media",
                        &[("group", group.name.into())],
                    ),
                ))
            }
            DeviceFabricPaneRow::SpeakerSetup => {
                if let Some(chrome) = self.chrome.clone() {
                    self.open_settings(&chrome, Some("settings://speakers"));
                }
                return;
            }
            DeviceFabricPaneRow::SpeakerMembership(membership) => Some((
                DeviceFabricUiAction::LeaveSpeakerGroup {
                    group: membership.group,
                    kind: membership.scope.kind(),
                },
                String::new(),
            )),
            DeviceFabricPaneRow::RemoteAudioRoute(route) => Some((
                DeviceFabricUiAction::StopRemoteAudio {
                    session_id: route.session_id,
                },
                String::new(),
            )),
            DeviceFabricPaneRow::RemoteCaptureSource(kind) => {
                self.qs_selected_source = Some(QsDeviceSource::RemoteCapture(kind));
                self.bump_chrome();
                return;
            }
            DeviceFabricPaneRow::RemoteCaptureSession(session) => {
                let Some(session_id) = session.session_id else {
                    return;
                };
                if session.receiving
                    && session.phase
                        == crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase::AppApprovalRequired
                {
                    Some((
                        DeviceFabricUiAction::ApproveRemoteCaptureTarget { session_id },
                        String::new(),
                    ))
                } else {
                    Some((
                        DeviceFabricUiAction::StopRemoteCapture { session_id },
                        String::new(),
                    ))
                }
            }
            DeviceFabricPaneRow::RealtimeConsent(consent) => {
                let Some(approved) = Self::qs_device_row_choice(part) else {
                    return;
                };
                let result =
                    self.realtime_continuity_call(RealtimeContinuityUiAction::ResolveConsent {
                        session: consent.session,
                        approved,
                    });
                // The person's own choice: the call moving here (or staying
                // put) is the answer. Only a failure needs saying.
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    if let Err(error) = result {
                        state.push_notification(t("host-call-move-failed"), error);
                    }
                    state.bump();
                }
                return;
            }
            DeviceFabricPaneRow::RealtimeSource(source) => {
                self.qs_selected_source = Some(QsDeviceSource::Realtime(source.session));
                self.bump_chrome();
                return;
            }
            DeviceFabricPaneRow::BluetoothAuthorization(authorization) => {
                let Some(approved) = Self::qs_device_row_choice(part) else {
                    return;
                };
                Some((
                    DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                        request_id: authorization.request_id,
                        approved,
                    },
                    String::new(),
                ))
            }
            DeviceFabricPaneRow::BluetoothHandoff(status) => {
                use crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView;
                match status.phase {
                    BluetoothHandoffPhaseView::Completed => None,
                    BluetoothHandoffPhaseView::Failed(_) | BluetoothHandoffPhaseView::Cancelled
                        if status.outbound =>
                    {
                        Some((
                            DeviceFabricUiAction::RetryBluetoothHandoff {
                                request_id: status.request_id,
                            },
                            String::new(),
                        ))
                    }
                    BluetoothHandoffPhaseView::Failed(_) | BluetoothHandoffPhaseView::Cancelled => {
                        None
                    }
                    _ => Some((
                        DeviceFabricUiAction::CancelBluetoothHandoff {
                            request_id: status.request_id,
                        },
                        String::new(),
                    )),
                }
            }
            DeviceFabricPaneRow::GamepadAuthorization(authorization) => {
                let Some(approved) = Self::qs_device_row_choice(part) else {
                    return;
                };
                Some((
                    DeviceFabricUiAction::AuthorizeGamepadForwarding {
                        forwarding_id: authorization.forwarding_id,
                        approved,
                    },
                    String::new(),
                ))
            }
            DeviceFabricPaneRow::LocalGamepad(gamepad) => {
                if let Some(forwarding) = gamepad.forwarding {
                    Some((
                        DeviceFabricUiAction::StopPhysicalGamepad {
                            session: forwarding.session,
                        },
                        String::new(),
                    ))
                } else {
                    self.qs_selected_source = Some(QsDeviceSource::Gamepad(gamepad.token));
                    self.bump_chrome();
                    return;
                }
            }
            DeviceFabricPaneRow::MediaHandoff(status) => match status.phase {
                crate::shell::device_fabric_runtime::MediaHandoffPhase::Pending => Some((
                    DeviceFabricUiAction::CancelMediaHandoff {
                        request_id: status.request_id,
                    },
                    String::new(),
                )),
                crate::shell::device_fabric_runtime::MediaHandoffPhase::Failed(_)
                | crate::shell::device_fabric_runtime::MediaHandoffPhase::Cancelled => Some((
                    DeviceFabricUiAction::RetryMediaHandoff {
                        request_id: status.request_id,
                    },
                    String::new(),
                )),
                crate::shell::device_fabric_runtime::MediaHandoffPhase::Completed => None,
            },
            DeviceFabricPaneRow::Media(media) => {
                self.qs_selected_source = Some(QsDeviceSource::Media(media.session_id));
                self.bump_chrome();
                return;
            }
            DeviceFabricPaneRow::LocalInput(input) if !input.is_default => Some((
                DeviceFabricUiAction::SelectLocalInput {
                    node_id: input.node_id,
                },
                String::new(),
            )),
            DeviceFabricPaneRow::Tethering(status) => {
                if status.phase
                    == crate::shell::personal_tethering_runtime::TetheringPhase::ConsentRequired
                {
                    let Some(approved) = Self::qs_device_row_choice(part) else {
                        return;
                    };
                    Some((
                        DeviceFabricUiAction::AuthorizePhoneTethering {
                            request_id: status.request_id,
                            approved,
                        },
                        String::new(),
                    ))
                } else if status.can_disconnect() {
                    Some((
                        DeviceFabricUiAction::DisconnectPhoneTethering {
                            request_id: status.request_id,
                        },
                        String::new(),
                    ))
                } else {
                    None
                }
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && let Some(QsDeviceSource::RemoteCapture(kind)) = self.qs_selected_source =>
            {
                Some((
                    DeviceFabricUiAction::RequestRemoteCapture {
                        peer: device.id,
                        kind,
                    },
                    t_args(
                        "host-fabric-asked-capture",
                        &[
                            ("device", device.label.into()),
                            ("input", t(remote_capture_kind_key(kind)).into()),
                        ],
                    ),
                ))
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && device.audio_output
                    && let Some(QsDeviceSource::RemoteAudio(source_token)) =
                        self.qs_selected_source =>
            {
                Some((
                    DeviceFabricUiAction::StartRemoteAudio {
                        source_token,
                        target: device.id,
                    },
                    t_args(
                        "host-fabric-asked-audio",
                        &[("device", device.label.into())],
                    ),
                ))
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && device.kind == crate::shell::device_fabric::DeviceKind::Phone
                    && self.qs_selected_source.is_none() =>
            {
                Some((
                    DeviceFabricUiAction::RequestPhoneTethering { phone: device.id },
                    t_args(
                        "host-fabric-asked-tethering",
                        &[("device", device.label.into())],
                    ),
                ))
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && let Some(QsDeviceSource::Gamepad(source_token)) =
                        self.qs_selected_source =>
            {
                Some((
                    DeviceFabricUiAction::StartPhysicalGamepad {
                        source_token,
                        target: device.id,
                    },
                    t_args(
                        "host-fabric-asked-controller",
                        &[("device", device.label.into())],
                    ),
                ))
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && let Some(QsDeviceSource::Realtime(session)) = &self.qs_selected_source =>
            {
                let session = session.clone();
                let result = self.realtime_continuity_call(RealtimeContinuityUiAction::Propose {
                    session,
                    target: device.id,
                });
                if let Some(chrome) = self.chrome.as_ref() {
                    let mut state = chrome.borrow_mut();
                    match result {
                        Ok(_) => state.push_notification(
                            t("host-call-handoff-title"),
                            t_args(
                                "host-call-handoff-waiting",
                                &[("device", device.label.into())],
                            ),
                        ),
                        Err(error) => state.push_notification(t("host-call-move-failed"), error),
                    };
                    state.bump();
                }
                return;
            }
            DeviceFabricPaneRow::Device(device)
                if !device.is_local
                    && device.online
                    && let Some(QsDeviceSource::Media(session_id)) = &self.qs_selected_source =>
            {
                let session_id = session_id.clone();
                let compatible = view.local_media.iter().any(|media| {
                    media.session_id == session_id && media.targets.contains(&device.id)
                });
                compatible.then(|| {
                    (
                        DeviceFabricUiAction::HandoffMedia {
                            session_id,
                            target: device.id,
                        },
                        t_args(
                            "host-fabric-media-preparing",
                            &[("device", device.label.clone().into())],
                        ),
                    )
                })
            }
            // Another device itself, with nothing chosen to send it: its
            // settings pane, where its display is placed beside this one's
            // (Displays) or the device managed (Devices & sync).
            DeviceFabricPaneRow::Device(device)
                if !device.is_local && self.qs_selected_source.is_none() =>
            {
                if let Some(chrome) = self.chrome.clone() {
                    self.open_settings(&chrome, Some(device_settings_pane(&device)));
                }
                return;
            }
            _ => None,
        };
        let Some((action, success)) = action else {
            return;
        };
        self.finish_qs_device_action(action, success);
    }

    pub(super) fn qs_device_secondary_action(
        part: tilo_quick_settings::RowPart,
        item: &DeviceFabricPaneRow,
    ) -> Option<(DeviceFabricUiAction, String)> {
        let DeviceFabricPaneRow::Device(device) = item else {
            return None;
        };
        if part != tilo_quick_settings::RowPart::Control || device.is_local || !device.online {
            return None;
        }
        if device.controls_local {
            Some((
                DeviceFabricUiAction::RevokeControl {
                    controller: device.id.clone(),
                },
                String::new(),
            ))
        } else {
            Some((
                DeviceFabricUiAction::GrantControl {
                    controller: device.id.clone(),
                },
                t_args(
                    "host-fabric-control-granted",
                    &[("device", device.label.clone().into())],
                ),
            ))
        }
    }

    /// Runs a device action the person chose. `next_step` is empty when the
    /// pane itself shows the result; it is only said when something still
    /// happens elsewhere (another device must confirm, a grant expires).
    pub(super) fn finish_qs_device_action(&self, action: DeviceFabricUiAction, next_step: String) {
        let result = self.device_fabric_call(action);
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            match result {
                Ok(_) if next_step.is_empty() => {}
                Ok(_) => {
                    state.push_notification(t("host-fabric-notice-title"), next_step);
                }
                Err(error) => {
                    state.push_notification(t("host-fabric-action-failed"), error);
                }
            }
            state.bump();
        }
    }

    /// Read what the open Devices or Clipboard section lists into chrome
    /// state, for the root page to lay out and paint.
    pub(super) fn refresh_qs_hosted_rows(&mut self) {
        use tilo_quick_settings::QsSection;
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let section = {
            let st = chrome.borrow();
            (st.overlays.quick_settings)
                .then_some(st.quick_settings.section)
                .flatten()
        };
        // Without a device fabric or a clipboard manager (a capture, a
        // test) the rows stay as they are.
        match section {
            Some(QsSection::Devices) => {
                let Some(rows) = self.qs_device_section_rows() else {
                    return;
                };
                let mut st = chrome.borrow_mut();
                if st.quick_settings.device_rows != rows {
                    st.quick_settings.device_rows = rows;
                    st.bump();
                }
            }
            Some(QsSection::Clipboard) => {
                if self.clipboard_manager_bridge.is_none() {
                    return;
                }
                let view = self.qs_clipboard_view();
                let mut st = chrome.borrow_mut();
                if st.quick_settings.clipboard != view {
                    st.quick_settings.clipboard = view;
                    st.bump();
                }
            }
            _ => {}
        }
    }

    /// The Devices section's rows, from the device fabric.
    pub(super) fn qs_device_section_rows(
        &mut self,
    ) -> Option<Vec<tilo_quick_settings::hosted::HostedRow>> {
        use tilo_quick_settings::hosted::{HostedActions, HostedRow};
        let view = self
            .device_fabric_call(DeviceFabricUiAction::Snapshot)
            .ok()?;
        self.normalise_qs_selected_media(&view);
        let selected = self.qs_selected_media().and_then(|id| {
            view.local_media
                .iter()
                .find(|media| &media.session_id == id)
        });
        let realtime = self
            .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
            .unwrap_or_default();
        let rows = device_fabric_pane_rows(&view, &realtime)
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let (title, detail, icon, selected, enabled) =
                    self.qs_device_row_face(&view, selected, item);
                let names = Self::qs_device_row_button_names(item);
                let actions = match (item, names.as_slice()) {
                    (DeviceFabricPaneRow::Device(device), [control]) => HostedActions::Control {
                        name: control.clone(),
                        controlling: device.controls_local,
                        controllable: !device.is_local && device.online,
                    },
                    (_, [first, second]) => HostedActions::Pair {
                        first: (ui::icons::Icon::Check, first.clone()),
                        second: (ui::icons::Icon::X, second.clone()),
                    },
                    _ => item
                        .select_action_key()
                        .map_or(HostedActions::None, |key| HostedActions::Select(t(key))),
                };
                HostedRow {
                    key: index.to_string(),
                    title,
                    detail,
                    icon,
                    selected,
                    enabled,
                    actions,
                }
            })
            .collect();
        Some(rows)
    }

    /// The clipboard history as Quick Settings shows it, from one snapshot:
    /// the row's count is that snapshot's length and the section's copies
    /// are its latest entries (or its matches while searching).
    pub(super) fn qs_clipboard_view(&self) -> tilo_quick_settings::ClipboardView {
        use tilo_quick_settings::hosted::{HostedActions, HostedRow};
        let history = self.clipboard_manager_call(ClipboardManagerAction::Snapshot);
        let total = history.len();
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot).ok();
        let now_unix_ms = unix_now_ms();
        let rows = history
            .iter()
            .map(|item| {
                let title = item.text_preview.as_deref().map_or_else(
                    || {
                        item.mime_types.first().map_or_else(
                            || t("a11y-clipboard-data"),
                            |mime| {
                                t_args(
                                    "qs-clipboard-mime-bytes",
                                    &[
                                        ("mime", mime.as_str().into()),
                                        ("bytes", item.byte_len.to_string().into()),
                                    ],
                                )
                            },
                        )
                    },
                    |text| text.split_whitespace().collect::<Vec<_>>().join(" "),
                );
                let detail = t_args(
                    "qs-clipboard-copied",
                    &[
                        (
                            "source",
                            clipboard_history_source_label(item, fabric.as_ref()).into(),
                        ),
                        (
                            "age",
                            clipboard_history_relative_time(item.created_unix_ms, now_unix_ms)
                                .into(),
                        ),
                    ],
                );
                HostedRow {
                    key: hex::encode(item.id),
                    icon: if item.text_preview.is_some() {
                        ui::icons::Icon::FileText
                    } else {
                        ui::icons::Icon::File
                    },
                    selected: false,
                    enabled: true,
                    actions: HostedActions::Clipboard {
                        pinned: item.pinned,
                        pin: t(if item.pinned {
                            "a11y-clipboard-unpin"
                        } else {
                            "a11y-clipboard-pin"
                        }),
                        remove: t("qs-clipboard-remove-a11y"),
                    },
                    title,
                    detail,
                }
            })
            .collect();
        tilo_quick_settings::ClipboardView {
            total,
            caption: String::new(),
            rows,
        }
    }
}

/// The settings pane a device row opens: Displays, where a device with a
/// display is placed beside this one, else Devices & sync.
pub(super) fn device_settings_pane(device: &FabricDeviceView) -> &'static str {
    if device.display {
        "settings://displays"
    } else {
        "settings://devices_sync"
    }
}
