//! Quick settings device, Bluetooth and clipboard rows: clicks and row models.

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
    pub(super) fn handle_qs_device_fabric_click(&mut self, point: Point) {
        // The rows hit-tested are the ones the fabric has now.
        self.refresh_qs_hosted_rows();
        let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) else {
            return;
        };
        self.normalise_qs_selected_media(&view);
        let realtime = self
            .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
            .unwrap_or_default();
        let rows = device_fabric_pane_rows(&view, &realtime);
        let Some((row_index, item)) = rows
            .iter()
            .enumerate()
            .find(|(index, _)| self.qs_devices_row_rect(*index).contains(point))
            .map(|(index, item)| (index, item.clone()))
        else {
            return;
        };
        let secondary = self.qs_device_secondary_action(point, row_index, &item);
        if let Some((action, success)) = secondary {
            self.finish_qs_device_action(action, success);
            return;
        }
        let action = match item {
            DeviceFabricPaneRow::CalibrationProposal(proposal) => {
                let (apply, dismiss) =
                    Self::qs_realtime_consent_actions(self.qs_devices_row_rect(row_index));
                if apply.contains(point) {
                    Some((
                        DeviceFabricUiAction::ApplyCalibration {
                            device: proposal.peer,
                        },
                        String::new(),
                    ))
                } else if dismiss.contains(point) {
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
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
            DeviceFabricPaneRow::RemoteCaptureConsent(consent) => {
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
                    return;
                };
                Some((
                    DeviceFabricUiAction::AuthorizeRemoteCapture {
                        request_id: consent.request_id,
                        approved,
                    },
                    String::new(),
                ))
            }
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
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
                let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
            DeviceFabricPaneRow::LocalOutput(output) if !output.is_default => Some((
                DeviceFabricUiAction::RouteLocalAudio {
                    node_id: output.node_id,
                },
                String::new(),
            )),
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
                    let Some(approved) = self.qs_device_row_choice(row_index, point) else {
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
            _ => None,
        };
        let Some((action, success)) = action else {
            return;
        };
        self.finish_qs_device_action(action, success);
    }

    pub(super) fn handle_qs_bluetooth_details_click(&mut self, point: Point, address: &str) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let accessory = chrome
            .borrow()
            .quick_settings
            .bt_devices
            .iter()
            .find(|device| device.address.eq_ignore_ascii_case(address))
            .cloned();
        let Some(accessory) = accessory else {
            return;
        };
        let Ok(view) = self.device_fabric_call(DeviceFabricUiAction::Snapshot) else {
            return;
        };
        let rows = std::iter::once(None)
            .chain(view.devices.into_iter().map(Some))
            .collect::<Vec<_>>();
        let _ = self.clamp_qs_device_fabric_start(rows.len());
        let selected = rows
            .into_iter()
            .skip(self.qs_device_fabric_start)
            .take(self.qs_device_fabric_visible_rows())
            .enumerate()
            .find(|(index, _)| self.qs_device_row_rect(*index).contains(point))
            .map(|(_, row)| row);
        match selected {
            Some(None) => {
                let result = self.qs_actions.disconnect_bluetooth(&accessory.address);
                match result {
                    Ok(()) => {
                        crate::shell::quick_settings::actions::log_qs_result(
                            "disconnect_bluetooth",
                            Ok(()),
                        );
                        let mut state = chrome.borrow_mut();
                        if let Some(device) =
                            state.quick_settings.bt_devices.iter_mut().find(|device| {
                                device.address.eq_ignore_ascii_case(&accessory.address)
                            })
                        {
                            device.connected = false;
                        }
                        // The device row now reads as disconnected.
                        state.quick_settings.pop_level();
                        state.bump();
                    }
                    Err(error) => {
                        crate::shell::quick_settings::actions::log_qs_result(
                            "disconnect_bluetooth",
                            Err(error.clone()),
                        );
                        chrome.borrow_mut().push_notification(
                            t_args(
                                "qs-bluetooth-disconnect-failed",
                                &[("device", accessory.name.into())],
                            ),
                            t_args("qs-action-failed-refreshing", &[("error", error.into())]),
                        );
                    }
                }
                self.qs_refresh.kick_bt_devices();
            }
            Some(Some(device)) if !device.is_local && device.online => {
                self.finish_qs_device_action(
                    DeviceFabricUiAction::HandoffBluetooth {
                        address: accessory.address,
                        name: accessory.name.clone(),
                        target: device.id,
                    },
                    t_args(
                        "host-fabric-bluetooth-moving",
                        &[
                            ("accessory", accessory.name.into()),
                            ("device", device.label.into()),
                        ],
                    ),
                );
            }
            _ => {}
        }
    }

    pub(super) fn qs_device_secondary_action(
        &self,
        point: Point,
        row_index: usize,
        item: &DeviceFabricPaneRow,
    ) -> Option<(DeviceFabricUiAction, String)> {
        if let DeviceFabricPaneRow::LocalInput(input) = item {
            if !input.is_default {
                return None;
            }
            // The first button lowers, the second raises.
            let raise = !self.qs_device_row_choice(row_index, point)?;
            // A level step is its own feedback: the row's level updates.
            return Some((
                DeviceFabricUiAction::StepInputLevel {
                    node_id: input.node_id,
                    raise,
                },
                String::new(),
            ));
        }
        let DeviceFabricPaneRow::Device(device) = item else {
            return None;
        };
        let (placements, control) =
            Self::qs_device_row_actions(self.qs_devices_row_rect(row_index));
        let edge = placements
            .iter()
            .position(|rect| rect.contains(point))
            .and_then(|index| {
                [
                    crate::shell::device_fabric::DisplayEdge::Left,
                    crate::shell::device_fabric::DisplayEdge::Right,
                    crate::shell::device_fabric::DisplayEdge::Top,
                    crate::shell::device_fabric::DisplayEdge::Bottom,
                ]
                .get(index)
                .copied()
            });
        if let Some(edge) = edge
            && !device.is_local
            && device.online
            && device.display
        {
            return Some((
                DeviceFabricUiAction::Place {
                    device: device.id.clone(),
                    edge,
                },
                {
                    let args = [("device", ui::i18n::ArgValue::from(device.label.clone()))];
                    match edge {
                        crate::shell::device_fabric::DisplayEdge::Left => {
                            t_args("host-fabric-place-left", &args)
                        }
                        crate::shell::device_fabric::DisplayEdge::Right => {
                            t_args("host-fabric-place-right", &args)
                        }
                        crate::shell::device_fabric::DisplayEdge::Top => {
                            t_args("host-fabric-place-above", &args)
                        }
                        crate::shell::device_fabric::DisplayEdge::Bottom => {
                            t_args("host-fabric-place-below", &args)
                        }
                    }
                },
            ));
        }
        if !control.contains(point) || device.is_local || !device.online {
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

    /// A press on copy `index` of the Clipboard section: its pin, its
    /// remove, or the copy itself (`point` is `None` from the keyboard).
    pub(super) fn press_qs_clipboard_row(&mut self, index: usize, point: Option<Point>) {
        let history = self.filtered_clipboard_history();
        let start = self.qs_clipboard_history_start;
        let Some(item) = history.get(start.saturating_add(index)) else {
            return;
        };
        let row = self.qs_clipboard_row_rect(index);
        let (pin, remove) = crate::shell::quick_settings::hosted::pair_rects(row);
        if point.is_some_and(|point| pin.contains(point)) {
            self.qs_selected_clipboard = Some(item.id);
            let _ = self.clipboard_manager_call(ClipboardManagerAction::SetPinned {
                id: item.id,
                pinned: !item.pinned,
            });
        } else if point.is_some_and(|point| remove.contains(point)) {
            let _ = self.clipboard_manager_call(ClipboardManagerAction::Remove(item.id));
            let remaining = self.filtered_clipboard_history();
            let _ = self.clamp_qs_clipboard_history_start(remaining.len());
            self.qs_selected_clipboard = remaining
                .get(
                    start
                        .saturating_add(index)
                        .min(remaining.len().saturating_sub(1)),
                )
                .map(|item| item.id);
        } else {
            self.choose_clipboard_item(item.id);
            return;
        }
        self.refresh_qs_hosted_rows();
        self.bump_chrome();
    }

    /// Clear the clipboard history's unpinned copies.
    pub(super) fn clear_qs_clipboard(&mut self) {
        let _ = self.clipboard_manager_call(ClipboardManagerAction::ClearUnpinned);
        let history = self.filtered_clipboard_history();
        let _ = self.clamp_qs_clipboard_history_start(history.len());
        if !history
            .iter()
            .any(|item| Some(item.id) == self.qs_selected_clipboard)
        {
            self.qs_selected_clipboard = history.first().map(|item| item.id);
        }
        self.refresh_qs_hosted_rows();
        self.bump_chrome();
    }

    /// A press on row `index` of the Devices section. From the keyboard a
    /// request is allowed and any other row does what its press does.
    pub(super) fn press_qs_device_row(&mut self, index: usize, point: Option<Point>) {
        let Some(point) = point.or_else(|| self.qs_device_row_key_point(index)) else {
            return;
        };
        self.handle_qs_device_fabric_click(point);
        self.refresh_qs_hosted_rows();
    }

    /// Where a key press on row `index` of the Devices section lands: on
    /// Allow when the row asks, else on the row.
    fn qs_device_row_key_point(&self, index: usize) -> Option<Point> {
        let view = self
            .device_fabric_call(DeviceFabricUiAction::Snapshot)
            .ok()?;
        let realtime = self
            .realtime_continuity_call(RealtimeContinuityUiAction::Snapshot)
            .unwrap_or_default();
        let rows = device_fabric_pane_rows(&view, &realtime);
        let row = self.qs_devices_row_rect(index);
        Some(match rows.get(index) {
            Some(item) if Self::qs_device_row_asks(item) => {
                Self::qs_realtime_consent_actions(row).0.center()
            }
            _ => row.center(),
        })
    }

    /// Whether a Devices row is a request to allow or decline.
    pub(super) fn qs_device_row_asks(item: &DeviceFabricPaneRow) -> bool {
        matches!(
            item,
            DeviceFabricPaneRow::RemoteAudioConsent(_)
                | DeviceFabricPaneRow::SpeakerConsent(_)
                | DeviceFabricPaneRow::RemoteCaptureConsent(_)
                | DeviceFabricPaneRow::RealtimeConsent(_)
                | DeviceFabricPaneRow::GamepadAuthorization(_)
        ) || matches!(
            item,
            DeviceFabricPaneRow::Tethering(status)
                if status.phase == crate::shell::personal_tethering_runtime::TetheringPhase::ConsentRequired
        )
    }

    /// Read what the open Devices or Clipboard section lists into chrome
    /// state, for the root page to lay out and paint.
    pub(super) fn refresh_qs_hosted_rows(&mut self) {
        use crate::shell::quick_settings::QsSection;
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let section = {
            let st = chrome.borrow();
            (st.overlays.quick_settings)
                .then_some(st.quick_settings.section)
                .flatten()
        };
        if std::mem::replace(&mut self.qs_hosted_section, section) != section
            && section == Some(QsSection::Clipboard)
        {
            self.qs_clipboard_query.clear();
            self.qs_clipboard_history_start = 0;
            self.qs_selected_clipboard = None;
        }
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
                let (caption, rows) = self.qs_clipboard_section_rows();
                let mut st = chrome.borrow_mut();
                if st.quick_settings.clipboard_rows != rows
                    || st.quick_settings.clipboard_caption != caption
                {
                    st.quick_settings.clipboard_rows = rows;
                    st.quick_settings.clipboard_caption = caption;
                    st.bump();
                }
            }
            _ => {}
        }
    }

    /// The Devices section's rows, from the device fabric.
    pub(super) fn qs_device_section_rows(
        &mut self,
    ) -> Option<Vec<crate::shell::quick_settings::hosted::HostedRow>> {
        use crate::shell::quick_settings::hosted::{HostedActions, HostedRow};
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
            .map(|item| {
                let (title, detail, icon, selected, enabled) =
                    self.qs_device_row_face(&view, selected, item);
                let names = Self::qs_device_row_button_names(item);
                let actions = match (item, names.as_slice()) {
                    (DeviceFabricPaneRow::Device(device), [l, r, a, b, control]) => {
                        HostedActions::Placement {
                            names: [l.clone(), r.clone(), a.clone(), b.clone()],
                            control: control.clone(),
                            controlling: device.controls_local,
                            placeable: !device.is_local && device.online && device.display,
                            controllable: !device.is_local && device.online,
                        }
                    }
                    (_, [first, second]) => {
                        let icons = if matches!(item, DeviceFabricPaneRow::LocalInput(_)) {
                            [ui::icons::Icon::ArrowDown, ui::icons::Icon::ArrowUp]
                        } else {
                            [ui::icons::Icon::Check, ui::icons::Icon::X]
                        };
                        HostedActions::Pair {
                            first: (icons[0], first.clone()),
                            second: (icons[1], second.clone()),
                        }
                    }
                    _ => item
                        .select_action_key()
                        .map_or(HostedActions::None, |key| HostedActions::Select(t(key))),
                };
                HostedRow {
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

    /// The Clipboard section's caption and rows: the latest copies, or the
    /// ones matching what was typed.
    pub(super) fn qs_clipboard_section_rows(
        &mut self,
    ) -> (String, Vec<crate::shell::quick_settings::hosted::HostedRow>) {
        use crate::shell::quick_settings::hosted::{HostedActions, HostedRow};
        let history = self.filtered_clipboard_history();
        let _ = self.clamp_qs_clipboard_history_start(history.len());
        let visible = QS_CLIPBOARD_ROWS;
        let start = self.qs_clipboard_history_start;
        let shown = visible.min(history.len().saturating_sub(start));
        let caption = if history.is_empty() && !self.qs_clipboard_query.is_empty() {
            t_args(
                "qs-clipboard-no-matches",
                &[("query", self.qs_clipboard_query.as_str().into())],
            )
        } else if !self.qs_clipboard_query.is_empty() {
            t_args(
                "qs-clipboard-search-matches",
                &[
                    ("query", self.qs_clipboard_query.as_str().into()),
                    ("count", history.len().into()),
                ],
            )
        } else if history.len() > visible {
            t_args(
                "qs-clipboard-search-range",
                &[
                    ("first", start.saturating_add(1).into()),
                    ("last", start.saturating_add(shown).into()),
                    ("total", history.len().into()),
                ],
            )
        } else if history.is_empty() {
            String::new()
        } else {
            t_args(
                "qs-clipboard-search-count",
                &[("count", history.len().into())],
            )
        };
        let fabric = self.device_fabric_call(DeviceFabricUiAction::Snapshot).ok();
        let now_unix_ms = unix_now_ms();
        let rows = history
            .iter()
            .skip(start)
            .take(visible)
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
                    icon: if item.text_preview.is_some() {
                        ui::icons::Icon::FileText
                    } else {
                        ui::icons::Icon::File
                    },
                    selected: self.qs_selected_clipboard == Some(item.id),
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
        (caption, rows)
    }
}
